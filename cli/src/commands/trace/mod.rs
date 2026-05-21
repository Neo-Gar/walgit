// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

//! `walgit trace` — record, inspect, diff, and flush reasoning traces.
//!
//! Two operating modes:
//!
//! - **Manual** (autonomous agents driving walgit directly): `start`, `record`
//!   tool calls explicitly, `set` decision fields, then `git commit` triggers
//!   the prepare-commit-msg hook which calls `flush`.
//! - **Adapter** (Claude Code today, more later): `--from-claude-hook` flags
//!   read hook JSON from stdin and translate it into the same accumulator.
//!
//! `install` / `uninstall` wire up the git hook and the per-agent hook files
//! so the manual or adapter mode actually fires automatically. Without
//! `install`, none of this runs — it's opt-in.

mod diff;
mod helpers;
mod install;
mod memwal;
mod record;

// Re-export the public API so callers use `commands::trace::*` unchanged.
pub use diff::diff;
pub use install::{
    AgentDef, AgentStatus, InstallOpts, Scope, UninstallOpts, AGENTS, install, resolve_agent_arg,
    uninstall,
};
pub use memwal::{
    UploadPushSummary, format_for_memwal, parse_memwal_payload, recall, upload, upload_for_push,
};
pub use record::{ClaudeEvent, RecordKind, record};

use crate::error::{Result, WalGitError};
use crate::{git, trace_pending, ui};
use console::style;
use serde_json::Value;
use std::collections::HashSet;

// ─── start ─────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct StartOpts {
    pub agent_id: Option<String>,
    pub run_id: Option<String>,
    pub task: Option<String>,
    pub parent_run_id: Option<String>,
    pub source: Option<String>,
    pub from_claude_hook: bool,
    /// Allow overwriting an existing pending trace from a *different* run.
    /// Without this, start refuses rather than silently dropping an
    /// in-progress trace that hasn't been committed yet.
    pub force: bool,
    /// Set by the user-global Claude Code hook so it no-ops outside opted-in
    /// repos. See [`crate::trace_pending::is_enabled`].
    pub only_if_enabled: bool,
}

pub async fn start(opts: StartOpts) -> Result<()> {
    let Some(git_dir) = helpers::resolve_git_dir_or_skip(opts.only_if_enabled)? else {
        return Ok(());
    };
    let (agent_id, run_id, source, task, parent) = if opts.from_claude_hook {
        let payload = helpers::read_stdin_json_silent()?;
        let session_id = payload
            .get("session_id")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        (
            "claude-code".to_string(),
            format!("claude-{}", session_id),
            Some("claude-code".to_string()),
            opts.task,
            opts.parent_run_id,
        )
    } else {
        let agent = opts
            .agent_id
            .ok_or_else(|| WalGitError::other("--agent is required (or use --from-claude-hook)"))?;
        let run = opts.run_id.unwrap_or_else(helpers::generate_run_id);
        (agent, run, opts.source, opts.task, opts.parent_run_id)
    };

    // Reentry on same run_id is idempotent — useful for Claude Code resumes.
    if let Some(existing) = trace_pending::load(&git_dir)? {
        if existing.run_id == run_id {
            return Ok(());
        }
        if !opts.force && !opts.from_claude_hook {
            return Err(WalGitError::other(format!(
                "pending trace already exists for run {} (use --force to overwrite, or `walgit trace abort`)",
                existing.run_id
            )));
        }
        // Different run: archive the prior trace so it isn't silently lost.
        let _ = trace_pending::consume(&git_dir)?;
    }

    let mut pt = trace_pending::PendingTrace::new(agent_id, run_id, source);
    if let Some(t) = task {
        pt.task = t;
    }
    if let Some(p) = parent {
        pt.parent_run_id = Some(p);
    }
    trace_pending::save(&git_dir, &pt)?;
    if !opts.from_claude_hook {
        ui::success(format!(
            "trace started: {} ({})",
            ui::highlight(&pt.agent_id),
            ui::dim(&pt.run_id),
        ));
    }
    Ok(())
}

// ─── set ────────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct SetOpts {
    pub task: Option<String>,
    pub decision: Option<String>,
    pub alternative: Vec<String>,
    pub confidence: Option<f32>,
    pub parent_run_id: Option<String>,
}

pub async fn set(opts: SetOpts) -> Result<()> {
    let git_dir = helpers::current_git_dir()?;
    let mut pt = trace_pending::load(&git_dir)?
        .ok_or_else(|| WalGitError::other("no pending trace — run `walgit trace start` first"))?;
    if let Some(t) = opts.task {
        pt.task = t;
    }
    if let Some(d) = opts.decision {
        pt.decision = d;
    }
    for alt in opts.alternative {
        pt.alternatives_considered.push(alt);
    }
    if let Some(c) = opts.confidence {
        pt.confidence = Some(c);
    }
    if let Some(p) = opts.parent_run_id {
        pt.parent_run_id = Some(p);
    }
    trace_pending::save(&git_dir, &pt)?;
    Ok(())
}

// ─── abort / status ─────────────────────────────────────────────────────────

pub async fn abort() -> Result<()> {
    let git_dir = helpers::current_git_dir()?;
    if trace_pending::load(&git_dir)?.is_none() {
        ui::info("no pending trace");
        return Ok(());
    }
    let _ = trace_pending::consume(&git_dir)?;
    ui::success("pending trace archived to .git/walgit/last-trace.json");
    Ok(())
}

pub async fn status() -> Result<()> {
    let git_dir = helpers::current_git_dir()?;
    let Some(pt) = trace_pending::load(&git_dir)? else {
        ui::info("no pending trace");
        ui::info(format!(
            "({})",
            trace_pending::pending_path(&git_dir).display()
        ));
        return Ok(());
    };
    ui::header("pending trace");
    println!(
        "  {}: {}",
        ui::label("agent_id     "),
        ui::highlight(&pt.agent_id)
    );
    println!("  {}: {}", ui::label("run_id       "), ui::dim(&pt.run_id));
    if let Some(p) = &pt.parent_run_id {
        println!("  {}: {}", ui::label("parent_run_id"), ui::dim(p));
    }
    println!(
        "  {}: {}",
        ui::label("task         "),
        if pt.task.is_empty() {
            ui::dim("(empty)")
        } else {
            pt.task.clone()
        }
    );
    println!(
        "  {}: {}",
        ui::label("tools_called "),
        if pt.tools_called.is_empty() {
            ui::dim("(none)")
        } else {
            format!("{}", pt.tools_called.len())
        }
    );
    for tc in &pt.tools_called {
        println!(
            "      {} {} {}",
            style("·").cyan(),
            ui::highlight(&tc.name),
            ui::dim(&tc.input_summary),
        );
    }
    println!(
        "  {}: {}",
        ui::label("decision     "),
        if pt.decision.is_empty() {
            ui::dim("(empty)")
        } else {
            pt.decision.clone()
        }
    );
    let alts_set: HashSet<&String> = pt.alternatives_considered.iter().collect();
    println!(
        "  {}: {}",
        ui::label("alternatives "),
        if alts_set.is_empty() {
            ui::dim("(none)")
        } else {
            format!("{}", alts_set.len())
        }
    );
    for alt in &pt.alternatives_considered {
        println!("      {} {}", style("·").cyan(), ui::dim(alt));
    }
    if let Some(c) = pt.confidence {
        println!("  {}: {:.2}", ui::label("confidence   "), c);
    }
    if let Some(t) = pt.started_at {
        let age = chrono::Utc::now().timestamp().saturating_sub(t);
        println!("  {}: {}s ago", ui::label("started      "), age);
    }
    Ok(())
}

// ─── snapshot (called by post-commit hook) ──────────────────────────────────

/// Move the pending trace to `traces/<commit_sha>.json`. Default commit is
/// `HEAD` (the freshly-created one when invoked from a post-commit hook).
///
/// Silently no-ops when there's no pending trace, so the hook never causes
/// `git commit` to fail and never produces stderr noise on plain commits.
pub async fn snapshot(commit_sha: Option<String>) -> Result<()> {
    let git_dir = helpers::current_git_dir()?;
    let Some(pt) = trace_pending::load(&git_dir)? else {
        return Ok(());
    };

    let sha = match commit_sha {
        Some(s) => s,
        None => {
            let cwd = std::env::current_dir()?;
            git::rev_parse(&cwd, "HEAD")?
        }
    };

    let path = trace_pending::save_snapshot(&git_dir, &sha, &pt)?;
    let _ = trace_pending::consume(&git_dir)?;
    ui::success(format!(
        "snapshot: {} → {}",
        ui::short_hash(&sha),
        ui::dim(&path.display().to_string())
    ));
    Ok(())
}
