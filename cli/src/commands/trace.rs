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

use crate::commands::find_repo;
use crate::error::{Result, WalGitError};
use crate::hooks;
use crate::trace::{self, ToolCall, Trace};
use crate::trace_pending::{self, PendingTrace};
use crate::{git, ui};
use console::style;
use serde_json::Value;
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};

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
    let Some(git_dir) = resolve_git_dir_or_skip(opts.only_if_enabled)? else {
        return Ok(());
    };
    let (agent_id, run_id, source, task, parent) = if opts.from_claude_hook {
        let payload = read_stdin_json_silent()?;
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
        let run = opts.run_id.unwrap_or_else(generate_run_id);
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

    let mut pt = PendingTrace::new(agent_id, run_id, source);
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

// ─── record (manual + claude hook) ─────────────────────────────────────────

pub enum RecordKind {
    /// Explicit tool call from an autonomous agent.
    Tool {
        name: String,
        input: String,
        output: String,
    },
    /// Decode Claude Code's hook stdin JSON.
    ClaudeHook { event: ClaudeEvent },
}

pub enum ClaudeEvent {
    UserPrompt,
    PostToolUse,
    Stop,
}

pub async fn record(kind: RecordKind, only_if_enabled: bool) -> Result<()> {
    let Some(git_dir) = resolve_git_dir_or_skip(only_if_enabled)? else {
        return Ok(());
    };

    // Read stdin once up front for Claude hook events — we may need
    // `session_id` for autostart *and* the rest of the payload to decode.
    // For the manual `Tool` variant stdin is irrelevant.
    let payload = match &kind {
        RecordKind::ClaudeHook { .. } => read_stdin_json_silent()?,
        RecordKind::Tool { .. } => Value::Null,
    };

    let mut pt = match trace_pending::load(&git_dir)? {
        Some(pt) => pt,
        None => {
            if matches!(kind, RecordKind::ClaudeHook { .. }) {
                // Autostart so a first-time user doesn't lose the session's
                // tool calls just because `walgit trace start` never ran.
                let session_id = payload
                    .get("session_id")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let pt = PendingTrace::new(
                    "claude-code".to_string(),
                    format!("claude-{}", session_id),
                    Some("claude-code".to_string()),
                );
                pt
            } else {
                return Err(WalGitError::other(
                    "no pending trace — run `walgit trace start` first",
                ));
            }
        }
    };

    match kind {
        RecordKind::Tool {
            name,
            input,
            output,
        } => {
            pt.push_tool(ToolCall {
                name,
                input_summary: input,
                output_summary: output,
            });
        }
        RecordKind::ClaudeHook { event } => {
            apply_claude_event(&mut pt, event, &payload);
        }
    }

    trace_pending::save(&git_dir, &pt)?;
    Ok(())
}

fn apply_claude_event(pt: &mut PendingTrace, event: ClaudeEvent, payload: &Value) {
    match event {
        ClaudeEvent::UserPrompt => {
            // First user prompt of a session is a strong signal for the
            // task field; later prompts shouldn't clobber it.
            if pt.task.trim().is_empty() {
                if let Some(p) = payload.get("prompt").and_then(Value::as_str) {
                    pt.task = truncate(p.lines().next().unwrap_or(p), 200);
                }
            }
        }
        ClaudeEvent::PostToolUse => {
            let name = payload
                .get("tool_name")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let input = summarize_tool_input(&name, payload.get("tool_input"));
            let output = summarize_tool_output(&name, payload.get("tool_response"));
            pt.push_tool(ToolCall {
                name,
                input_summary: input,
                output_summary: output,
            });
        }
        ClaudeEvent::Stop => {
            // No semantic action for now: pending trace stays as-is until the
            // next commit triggers flush. Future: optional LLM summarisation
            // here to fill `decision` from the transcript.
        }
    }
}

/// Pick the most meaningful field from `tool_input` for the given tool. The
/// raw JSON has way more than fits in the 200-char cap and includes the body
/// of files/commands; we trim it down to "what was this tool *about*."
fn summarize_tool_input(tool: &str, v: Option<&Value>) -> String {
    let Some(v) = v else {
        return String::new();
    };
    let pick = |keys: &[&str]| -> Option<String> {
        for k in keys {
            if let Some(s) = v.get(*k).and_then(Value::as_str) {
                return Some(s.to_string());
            }
        }
        None
    };
    match tool {
        "Bash" => pick(&["command"]).unwrap_or_default(),
        "Read" | "Write" | "Edit" | "NotebookEdit" | "MultiEdit" => {
            pick(&["file_path", "notebook_path", "path"]).unwrap_or_default()
        }
        "Glob" | "Grep" => pick(&["pattern"])
            .map(|p| match v.get("path").and_then(Value::as_str) {
                Some(path) => format!("{} in {}", p, path),
                None => p,
            })
            .unwrap_or_default(),
        "WebFetch" => pick(&["url"]).unwrap_or_default(),
        "WebSearch" => pick(&["query"]).unwrap_or_default(),
        "TodoWrite" => v
            .get("todos")
            .and_then(Value::as_array)
            .map(|arr| format!("{} todos", arr.len()))
            .unwrap_or_default(),
        "Task" => pick(&["description", "subagent_type", "prompt"]).unwrap_or_default(),
        _ => summarize_value(Some(v)),
    }
}

/// Pick the most meaningful summary of `tool_response`. Most Claude tools
/// return JSON whose body contains the entire file/command output — useless
/// to dump verbatim. We extract a one-line "result shape" per tool.
fn summarize_tool_output(tool: &str, v: Option<&Value>) -> String {
    let Some(v) = v else {
        return String::new();
    };
    match tool {
        "Read" => {
            if let Some(content) = v.pointer("/file/content").and_then(Value::as_str) {
                let lines = content.lines().count();
                return format!("{} lines, {} bytes", lines, content.len());
            }
            "ok".into()
        }
        "Bash" => {
            if v.get("interrupted")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                return "interrupted".into();
            }
            let stdout = v.get("stdout").and_then(Value::as_str).unwrap_or("");
            let stderr = v.get("stderr").and_then(Value::as_str).unwrap_or("");
            if stdout.is_empty() && !stderr.is_empty() {
                let first = stderr.lines().next().unwrap_or("");
                return format!("err: {}", first);
            }
            let first = stdout.lines().next().unwrap_or("");
            if first.is_empty() {
                "ok".into()
            } else {
                first.to_string()
            }
        }
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => "ok".into(),
        "Glob" | "Grep" => {
            // Response shapes vary: array of matches, or a string with newlines.
            if let Some(arr) = v.as_array() {
                return format!("{} matches", arr.len());
            }
            if let Some(s) = v.as_str() {
                let n = s.lines().filter(|l| !l.trim().is_empty()).count();
                return format!("{} matches", n);
            }
            summarize_value(Some(v))
        }
        "TodoWrite" => "ok".into(),
        _ => summarize_value(Some(v)),
    }
}

/// Fallback for tools we don't have a tailored summary for. Picks the most
/// likely "what is this" field, else compact-JSON. Kept dumb on purpose.
fn summarize_value(v: Option<&Value>) -> String {
    let Some(v) = v else {
        return String::new();
    };
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        Value::Bool(_) | Value::Number(_) => v.to_string(),
        Value::Array(_) | Value::Object(_) => {
            // Pull the most useful field if present, else compact JSON.
            for k in ["file_path", "path", "command", "pattern", "query", "url"] {
                if let Some(s) = v.get(k).and_then(Value::as_str) {
                    return s.to_string();
                }
            }
            serde_json::to_string(v).unwrap_or_default()
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
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
    let git_dir = current_git_dir()?;
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
    let git_dir = current_git_dir()?;
    if trace_pending::load(&git_dir)?.is_none() {
        ui::info("no pending trace");
        return Ok(());
    }
    let _ = trace_pending::consume(&git_dir)?;
    ui::success("pending trace archived to .git/walgit/last-trace.json");
    Ok(())
}

pub async fn status() -> Result<()> {
    let git_dir = current_git_dir()?;
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

// ─── push-flow integration ──────────────────────────────────────────────────

/// Drain local trace snapshots for the commits being pushed and ship them
/// synchronously to MemWal. Called from `git-remote-walgit::do_push` BEFORE
/// the Walrus upload, so a MemWal failure aborts the push without wasting
/// storage fees on the repo blob.
///
/// Semantics:
///
/// - Opt-in: if `<git-dir>/walgit/enabled` is missing, returns Ok with no
///   work — push is unaffected for users who don't want trace recording.
/// - If MemWal isn't configured but the repo is opted-in, returns a friendly
///   error so the user can fix it instead of silently dropping traces.
/// - Walks `git rev-list parent..head` and uploads every commit whose
///   snapshot exists locally. Commits without a snapshot (e.g., made
///   before `walgit trace install`) are silently skipped — they predate
///   the system, no trace was ever captured.
/// - On the first failed upload, returns Err and stops. The pending
///   snapshots stay in `traces/` so retry on the next push picks them up.
pub async fn upload_for_push(
    repo_dir: &Path,
    git_dir: &Path,
    namespace: &str,
    head_sha: &str,
    parent_sha: Option<&str>,
) -> Result<UploadPushSummary> {
    use crate::memwal::MemWalClient;

    if !trace_pending::is_enabled(git_dir) {
        return Ok(UploadPushSummary::default()); // not opted-in
    }

    // Enumerate commits being pushed (newest first by default; order doesn't
    // matter for upload, but we keep `git rev-list`'s order for logging).
    let shas = revs_in_push(repo_dir, head_sha, parent_sha)?;

    // Cross-reference against local snapshots: only those with files are
    // candidates. Missing ones are silently ignored.
    let candidates: Vec<(String, std::path::PathBuf)> = shas
        .iter()
        .map(|sha| (sha.clone(), trace_pending::trace_path(git_dir, sha)))
        .filter(|(_, p)| p.exists())
        .collect();

    if candidates.is_empty() {
        return Ok(UploadPushSummary::default());
    }

    let cfg = crate::config::load()?;
    let mw = cfg.memwal.as_ref().ok_or_else(|| {
        WalGitError::other(
            "this repo has reasoning traces enabled (\
             `walgit trace install` marker is present) but [memwal] is not \
             configured. Either configure MemWal in ~/.walgit/config.toml, or \
             run `walgit trace uninstall` to disable trace upload for this repo.",
        )
    })?;
    let priv_bytes = mw.load_delegate_key()?;
    let client = MemWalClient::new(mw.relayer_url.clone(), mw.account_id.clone(), priv_bytes);

    let mut summary = UploadPushSummary {
        attempted: candidates.len(),
        uploaded: 0,
        skipped_already_uploaded: 0,
    };

    for (sha, path) in candidates {
        // Idempotency: if a `.uploaded` sibling marker is present, the
        // snapshot was already shipped on a prior push (e.g. a previous
        // upload succeeded but the consumer-side cleanup hadn't run).
        let marker = path.with_extension("uploaded");
        if marker.exists() {
            summary.skipped_already_uploaded += 1;
            continue;
        }

        let pt = trace_pending::load_snapshot(&path)?;
        let text = format_for_memwal(&sha, &pt);
        client
            .remember(&text, Some(namespace))
            .await
            .map_err(|e| WalGitError::other(format!("MemWal upload for {}: {}", sha, e)))?;

        // Mark as uploaded so a re-push doesn't double-ship. We deliberately
        // do NOT delete the snapshot — keeping it lets `walgit show --trace`
        // resolve from local cache without a network round-trip.
        std::fs::write(&marker, "")?;
        summary.uploaded += 1;
    }

    Ok(summary)
}

#[derive(Default, Debug, Clone)]
pub struct UploadPushSummary {
    pub attempted: usize,
    pub uploaded: usize,
    pub skipped_already_uploaded: usize,
}

/// `git rev-list parent..head` (or `head` alone if no parent). Returns the
/// list of full SHAs that this push is making canonical. The list may be
/// empty when nothing changed.
fn revs_in_push(repo_dir: &Path, head: &str, parent: Option<&str>) -> Result<Vec<String>> {
    use std::process::Command;
    let mut args: Vec<String> = vec!["rev-list".into()];
    match parent {
        Some(p) => args.push(format!("{}..{}", p, head)),
        None => args.push(head.to_string()),
    }
    let out = Command::new("git")
        .args(args.iter().map(String::as_str))
        .current_dir(repo_dir)
        .output()
        .map_err(|e| WalGitError::git(format!("git rev-list: {}", e)))?;
    if !out.status.success() {
        return Err(WalGitError::git(format!(
            "git rev-list {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect())
}

// ─── snapshot (called by post-commit hook) ──────────────────────────────────

/// Move the pending trace to `traces/<commit_sha>.json`. Default commit is
/// `HEAD` (the freshly-created one when invoked from a post-commit hook).
///
/// Silently no-ops when there's no pending trace, so the hook never causes
/// `git commit` to fail and never produces stderr noise on plain commits.
pub async fn snapshot(commit_sha: Option<String>) -> Result<()> {
    let git_dir = current_git_dir()?;
    let Some(pt) = trace_pending::load(&git_dir)? else {
        return Ok(());
    };

    let sha = match commit_sha {
        Some(s) => s,
        None => {
            // We're after `git commit`; HEAD is the new commit. Get the full SHA.
            let cwd = std::env::current_dir()?;
            git::rev_parse(&cwd, "HEAD")?
        }
    };

    let path = trace_pending::save_snapshot(&git_dir, &sha, &pt)?;
    // Move pending → last so the next commit starts fresh.
    let _ = trace_pending::consume(&git_dir)?;
    // Hook output is suppressed; this only matters when invoked manually.
    ui::success(format!(
        "snapshot: {} → {}",
        ui::short_hash(&sha),
        ui::dim(&path.display().to_string())
    ));
    Ok(())
}

// ─── upload (manual MemWal upload) ──────────────────────────────────────────

/// Push one or all local trace snapshots to MemWal. The trace JSON itself is
/// uploaded as the memory `text` — the relayer handles embedding + Seal.
///
/// `namespace` defaults to the basename of the repo root, which is a stable
/// per-repo bucket for the user's MemWal account.
pub async fn upload(commit: Option<String>, namespace_override: Option<String>) -> Result<()> {
    use crate::memwal::MemWalClient;

    let git_dir = current_git_dir()?;

    // Config: pull MemWal creds from global walgit config.
    let cfg = crate::config::load()?;
    let mw = cfg.memwal.as_ref().ok_or_else(|| {
        WalGitError::other(
            "[memwal] not configured — set account_id / relayer_url / delegate_key_path \
             in ~/.walgit/config.toml",
        )
    })?;
    let priv_bytes = mw.load_delegate_key()?;
    let client = MemWalClient::new(mw.relayer_url.clone(), mw.account_id.clone(), priv_bytes);

    let namespace = namespace_override
        .unwrap_or_else(|| default_namespace().unwrap_or_else(|| "default".into()));

    let entries: Vec<(String, std::path::PathBuf)> = match commit {
        Some(sha) => vec![(sha.clone(), trace_pending::trace_path(&git_dir, &sha))],
        None => trace_pending::list_snapshots(&git_dir)?,
    };
    if entries.is_empty() {
        ui::info("no local trace snapshots to upload");
        return Ok(());
    }

    ui::header(&format!("uploading {} trace(s) to MemWal", entries.len()));
    ui::info(format!("namespace: {}", ui::highlight(&namespace)));
    ui::info(format!("relayer:   {}", ui::dim(&client.account_id)));

    let mut ok = 0usize;
    let mut failed: Vec<String> = Vec::new();
    for (sha, path) in entries {
        if !path.exists() {
            failed.push(format!("{} (file missing: {})", sha, path.display()));
            continue;
        }
        let pt = trace_pending::load_snapshot(&path)?;
        // Encode the structured trace as the memory text. We prepend a
        // stable header so the relayer's embedding picks up the commit
        // SHA, agent_id, and task — those are the signals semantic recall
        // wants ("show me past traces touching X").
        let text = format_for_memwal(&sha, &pt);
        match client.remember(&text, Some(&namespace)).await {
            Ok(resp) => {
                ok += 1;
                ui::success(format!(
                    "{} job={} {}",
                    ui::short_hash(&sha),
                    resp.job_id.as_deref().unwrap_or("?"),
                    ui::dim(&pt.task)
                ));
            }
            Err(e) => {
                failed.push(format!("{} ({})", sha, e));
                ui::warn(format!("{} failed: {}", ui::short_hash(&sha), e));
            }
        }
    }

    if !failed.is_empty() {
        return Err(WalGitError::other(format!(
            "{} of {} uploads failed",
            failed.len(),
            ok + failed.len()
        )));
    }
    ui::success(format!("uploaded {} traces", ok));
    Ok(())
}

/// Pull semantic matches for `query` from the project's namespace.
pub async fn recall(query: String, limit: u32, namespace_override: Option<String>) -> Result<()> {
    use crate::memwal::MemWalClient;

    let cfg = crate::config::load()?;
    let mw = cfg.memwal.as_ref().ok_or_else(|| {
        WalGitError::other("[memwal] not configured (see `walgit trace upload --help`)")
    })?;
    let priv_bytes = mw.load_delegate_key()?;
    let client = MemWalClient::new(mw.relayer_url.clone(), mw.account_id.clone(), priv_bytes);

    let namespace = namespace_override
        .unwrap_or_else(|| default_namespace().unwrap_or_else(|| "default".into()));
    let resp = client.recall(&query, Some(limit), Some(&namespace)).await?;

    ui::header(&format!("recall: \"{}\"", query));
    if resp.results.is_empty() {
        match resp.dropped_count.unwrap_or(0) {
            0 => ui::info("no matches"),
            n => ui::info(format!(
                "no matches above similarity threshold ({} item(s) in namespace were too distant)",
                n
            )),
        }
        return Ok(());
    }
    for (i, m) in resp.results.iter().enumerate() {
        let dist = m
            .distance
            .or(m.score)
            .map(|d| format!("{:.3}", d))
            .unwrap_or_else(|| "-".into());
        println!("  {} {}", ui::dim(&format!("#{} dist={}", i + 1, dist)), "");
        if let Some(t) = &m.text {
            for line in t.lines().take(4) {
                println!("    {}", line);
            }
            if t.lines().count() > 4 {
                println!("    {}", ui::dim("…"));
            }
        }
        println!();
    }
    Ok(())
}

/// Pack a snapshot into the textual form MemWal indexes. Kept stable so
/// semantic recall over multiple commits stays consistent.
fn format_for_memwal(commit_sha: &str, pt: &PendingTrace) -> String {
    let mut s = String::new();
    s.push_str(&format!("commit: {}\n", commit_sha));
    s.push_str(&format!("agent:  {}\n", pt.agent_id));
    s.push_str(&format!("run:    {}\n", pt.run_id));
    if let Some(p) = &pt.parent_run_id {
        s.push_str(&format!("parent: {}\n", p));
    }
    s.push_str(&format!(
        "task:   {}\n",
        if pt.task.trim().is_empty() {
            "(none)"
        } else {
            &pt.task
        }
    ));
    s.push_str("\n");
    if !pt.tools_called.is_empty() {
        s.push_str("tools:\n");
        for tc in &pt.tools_called {
            s.push_str(&format!(
                "  · {} {} → {}\n",
                tc.name, tc.input_summary, tc.output_summary
            ));
        }
        s.push_str("\n");
    }
    if !pt.decision.trim().is_empty() {
        s.push_str("decision:\n");
        s.push_str(&pt.decision);
        s.push_str("\n");
    }
    if !pt.alternatives_considered.is_empty() {
        s.push_str("\nalternatives:\n");
        for a in &pt.alternatives_considered {
            s.push_str(&format!("  ✗ {}\n", a));
        }
    }
    s
}

/// Pick a stable MemWal namespace for the current repo.
///
/// Preference order:
/// 1. **`LocalRepoConfig.id`** — the on-chain Sui repo object ID. Stable
///    across machines and contributors; the right answer for any
///    walgit-managed repo.
/// 2. **Directory basename** — fallback when we're inside a plain git repo
///    that isn't (yet) walgit-registered. Different contributors may pick
///    different basenames, so memory won't merge across machines — but it's
///    the best we can do without extra signal.
fn default_namespace() -> Option<String> {
    if let Ok((_, _, local)) = crate::commands::find_repo() {
        if !local.id.is_empty() && local.id != "pending" {
            return Some(local.id);
        }
    }
    let root = current_repo_root().ok()?;
    let dir = root.file_name()?.to_str()?.to_string();
    Some(dir)
}

// ─── install / uninstall ────────────────────────────────────────────────────

/// Status of a known agent adapter. `Planned` adapters are listed in the
/// picker so users can see the roadmap, but selecting one errors out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentStatus {
    Available,
    Planned,
}

/// Static description of a supported agent adapter.
#[derive(Clone, Copy, Debug)]
pub struct AgentDef {
    /// Stable key accepted by `--agent <key>`. Lowercase, kebab-case.
    pub key: &'static str,
    /// Human-readable name shown in the picker and confirmation lines.
    pub label: &'static str,
    pub status: AgentStatus,
    /// Aliases accepted by `--agent` for ergonomics (e.g. `claude` → `claude-code`).
    pub aliases: &'static [&'static str],
}

/// Registry of supported agents. Order is the order shown in the picker.
/// Adding a new agent: append here, add a hook-installer arm in
/// [`install_one`] and [`uninstall_one`], flip status to `Available`.
pub const AGENTS: &[AgentDef] = &[
    AgentDef {
        key: "claude-code",
        label: "Claude Code",
        status: AgentStatus::Available,
        aliases: &["claude"],
    },
    AgentDef {
        key: "codex",
        label: "Codex (OpenAI)",
        status: AgentStatus::Planned,
        aliases: &[],
    },
    AgentDef {
        key: "cursor",
        label: "Cursor",
        status: AgentStatus::Planned,
        aliases: &[],
    },
    AgentDef {
        key: "gemini",
        label: "Gemini CLI",
        status: AgentStatus::Planned,
        aliases: &["gemini-cli"],
    },
    AgentDef {
        key: "copilot",
        label: "GitHub Copilot CLI",
        status: AgentStatus::Planned,
        aliases: &["copilot-cli", "gh-copilot"],
    },
    AgentDef {
        key: "factory",
        label: "Factory AI Droid",
        status: AgentStatus::Planned,
        aliases: &["factory-ai", "droid"],
    },
    AgentDef {
        key: "opencode",
        label: "OpenCode",
        status: AgentStatus::Planned,
        aliases: &[],
    },
];

/// Resolve a user-supplied `--agent` string to one or more known agents.
/// Accepts:
///   - `"all"` → every `Available` agent
///   - a comma-separated list of keys/aliases → those agents (any status)
///   - a single key/alias → that one agent
pub fn resolve_agent_arg(arg: &str) -> Result<Vec<&'static AgentDef>> {
    let arg = arg.trim();
    if arg.eq_ignore_ascii_case("all") {
        return Ok(AGENTS
            .iter()
            .filter(|a| a.status == AgentStatus::Available)
            .collect());
    }
    let mut out = Vec::new();
    for part in arg.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let found = AGENTS.iter().find(|a| {
            a.key.eq_ignore_ascii_case(part)
                || a.aliases.iter().any(|al| al.eq_ignore_ascii_case(part))
        });
        match found {
            Some(a) => out.push(a),
            None => {
                let keys: Vec<&str> = AGENTS.iter().map(|a| a.key).collect();
                return Err(WalGitError::other(format!(
                    "unknown agent '{}' (known: {})",
                    part,
                    keys.join(", ")
                )));
            }
        }
    }
    if out.is_empty() {
        return Err(WalGitError::other("empty --agent value".to_string()));
    }
    Ok(out)
}

/// Interactive multi-select picker. Available agents are pre-checked;
/// planned ones are listed unchecked so users can see the roadmap. Falls
/// back to all Available adapters when stdin is not a TTY (so CI / scripts
/// behave deterministically).
fn pick_agents_interactive() -> Result<Vec<&'static AgentDef>> {
    use console::Term;
    use dialoguer::{MultiSelect, theme::ColorfulTheme};

    let stdout_tty = Term::stdout().is_term();
    let stdin_tty = Term::stderr().is_term(); // stderr is a decent proxy when piping
    if !stdout_tty || !stdin_tty {
        // Non-interactive: install everything we have an adapter for.
        return Ok(AGENTS
            .iter()
            .filter(|a| a.status == AgentStatus::Available)
            .collect());
    }

    // The picker only includes Available agents so users can't pick a
    // not-yet-implemented one and end up with a confusing "planned, skipped"
    // warning. Planned agents are listed separately above the picker so the
    // roadmap is still visible without being selectable.
    let available: Vec<&AgentDef> = AGENTS
        .iter()
        .filter(|a| a.status == AgentStatus::Available)
        .collect();
    let planned: Vec<&AgentDef> = AGENTS
        .iter()
        .filter(|a| a.status == AgentStatus::Planned)
        .collect();

    if !planned.is_empty() {
        let names: Vec<&str> = planned.iter().map(|a| a.label).collect();
        println!(
            "  {} {}: {}",
            style("·").cyan(),
            style("planned (not yet selectable)").dim(),
            style(names.join(", ")).dim(),
        );
    }

    // dialoguer doesn't render its own key-hint line, so print one ourselves.
    // Without this, first-time users hit Enter immediately and get whatever
    // defaults we chose for them — usually fine but surprising.
    println!(
        "{} {}",
        style("?").yellow().bold(),
        style("↑/↓ move · space toggle · enter confirm · esc cancel").dim(),
    );

    // Resolve installation status per agent so the picker can flag what's
    // already there. We default these entries to *unchecked* so a user who
    // hits enter without thinking doesn't trigger pointless re-installs.
    // "Installed" here means present in either scope (global or local), so
    // a user who only ever installed globally still gets the badge.
    let installed: Vec<bool> = available.iter().map(|a| agent_is_installed(a)).collect();

    let items: Vec<String> = available
        .iter()
        .zip(&installed)
        .map(|(a, &inst)| {
            if inst {
                format!("{}  (already installed)", a.label)
            } else {
                a.label.to_string()
            }
        })
        .collect();
    let defaults: Vec<bool> = installed.iter().map(|&i| !i).collect();

    let chosen = MultiSelect::with_theme(&ColorfulTheme::default())
        .with_prompt("Which agents should walgit record traces for?")
        .items(&items)
        .defaults(&defaults)
        .interact_opt()
        .map_err(|e| WalGitError::other(format!("picker failed: {}", e)))?;

    let chosen = chosen.ok_or_else(|| WalGitError::other("install cancelled".to_string()))?;
    let selected: Vec<&'static AgentDef> = chosen.iter().map(|&i| available[i]).collect();

    // If any selection would re-install an already-installed adapter, ask
    // for a single Y/n confirmation listing them. Default Yes — the user
    // explicitly checked the box, this is just a tripwire against fat-finger.
    let to_reinstall: Vec<&str> = chosen
        .iter()
        .filter(|&&i| installed[i])
        .map(|&i| available[i].label)
        .collect();
    if !to_reinstall.is_empty() {
        let question = format!("re-install {}?", to_reinstall.join(", "));
        let confirm = ui::prompt_yes_no(&question, true)
            .map_err(|e| WalGitError::other(format!("confirmation failed: {}", e)))?;
        if !confirm {
            // Drop the already-installed ones; keep the rest of the selection.
            return Ok(chosen
                .into_iter()
                .filter(|&i| !installed[i])
                .map(|i| available[i])
                .collect());
        }
    }

    Ok(selected)
}

/// Where Claude Code hooks live. Both scopes are touched by default install
/// because Cursor's Claude Code extension reads only `Global`, while the
/// `claude` CLI in a terminal also picks up `Local` — and we want one
/// install command to cover both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Global,
    Local,
}

pub struct InstallOpts {
    pub agent_arg: Option<String>,
    pub no_global: bool,
    pub global_only: bool,
}

pub struct UninstallOpts {
    pub agent_arg: Option<String>,
    pub purge_global: bool,
}

pub async fn install(opts: InstallOpts) -> Result<()> {
    let agents = match opts.agent_arg {
        Some(s) => resolve_agent_arg(&s)?,
        None => pick_agents_interactive()?,
    };
    if agents.is_empty() {
        ui::warn("no agents selected — nothing to install");
        return Ok(());
    }

    // Decide which scopes to write Claude settings into.
    let write_global = !opts.no_global;
    let write_local = !opts.global_only;

    // Git hook + opt-in marker live per-repo.
    let git_dir = current_git_dir().ok();
    match &git_dir {
        Some(g) => {
            let hook_path = hooks::install_git_hook(g)?;
            ui::success(format!(
                "git hook installed: {}",
                ui::dim(&hook_path.display().to_string())
            ));
            trace_pending::mark_enabled(g)?;
            ui::success(format!(
                "marker written: {}",
                ui::dim(&trace_pending::enabled_path(g).display().to_string())
            ));
        }
        None => {
            ui::warn(
                "not inside a git repository — skipping git hook and opt-in marker (Claude Code hooks still go to user-global if --no-global isn't set)",
            );
        }
    }

    let mut planned_skipped = Vec::new();
    for a in &agents {
        if a.status == AgentStatus::Planned {
            planned_skipped.push(a.label);
            continue;
        }
        if write_global {
            install_one(a, Scope::Global)?;
        }
        if write_local && git_dir.is_some() {
            install_one(a, Scope::Local)?;
        }
    }

    for label in planned_skipped {
        ui::warn(format!("{} adapter is not implemented yet", label));
    }
    Ok(())
}

pub async fn uninstall(opts: UninstallOpts) -> Result<()> {
    let agents = match opts.agent_arg {
        Some(s) => resolve_agent_arg(&s)?,
        // For uninstall, default to ALL known agents (Planned too — uninstall
        // is a no-op for never-installed adapters and we want a clean
        // sweep when the user just runs `walgit trace uninstall`).
        None => AGENTS.iter().collect(),
    };

    if let Ok(git_dir) = current_git_dir() {
        if hooks::uninstall_git_hook(&git_dir)? {
            ui::success("git hook removed");
        }
        if trace_pending::is_enabled(&git_dir) {
            trace_pending::unmark_enabled(&git_dir)?;
            ui::success("opt-in marker removed");
        }
    }
    for a in &agents {
        uninstall_one(a, Scope::Local)?;
        if opts.purge_global {
            uninstall_one(a, Scope::Global)?;
        }
    }
    if !opts.purge_global {
        ui::info(
            "user-global Claude Code hooks left in place — they're gated by per-repo markers and harmless. Pass --purge-global to remove them too.",
        );
    }
    Ok(())
}

/// Best-effort check whether an adapter is installed in ANY scope. Used to
/// decorate the picker; not a hard guarantee.
fn agent_is_installed(a: &AgentDef) -> bool {
    match a.key {
        "claude-code" => {
            let g = claude_settings_path(Scope::Global)
                .map(|p| hooks::is_claude_installed(&p))
                .unwrap_or(false);
            let l = claude_settings_path(Scope::Local)
                .map(|p| hooks::is_claude_installed(&p))
                .unwrap_or(false);
            g || l
        }
        _ => false,
    }
}

/// Per-agent install dispatch. New adapters get an arm here.
fn install_one(a: &AgentDef, scope: Scope) -> Result<()> {
    match a.key {
        "claude-code" => {
            let settings = claude_settings_path(scope)?;
            // Global hooks must be gated so they don't fire in non-walgit
            // repos; local ones don't need gating because the project is
            // itself the opt-in signal.
            let gated = scope == Scope::Global;
            hooks::install_claude_settings(&settings, gated)?;
            ui::success(format!(
                "Claude Code hooks installed ({:?}): {}",
                scope,
                ui::dim(&settings.display().to_string())
            ));
            Ok(())
        }
        other => Err(WalGitError::other(format!(
            "adapter '{}' has no installer wired up yet — please file an issue",
            other
        ))),
    }
}

/// Per-agent uninstall dispatch. Tolerant: missing files are not an error.
fn uninstall_one(a: &AgentDef, scope: Scope) -> Result<()> {
    match a.key {
        "claude-code" => {
            let settings = claude_settings_path(scope)?;
            if hooks::uninstall_claude_settings(&settings)? {
                ui::success(format!(
                    "Claude Code hooks removed ({:?}): {}",
                    scope,
                    ui::dim(&settings.display().to_string())
                ));
            }
            Ok(())
        }
        _ => Ok(()), // planned adapters have nothing to remove yet
    }
}

fn claude_settings_path(scope: Scope) -> Result<PathBuf> {
    match scope {
        Scope::Global => {
            let home = dirs::home_dir()
                .ok_or_else(|| WalGitError::other("cannot resolve home directory"))?;
            Ok(home.join(".claude").join("settings.json"))
        }
        Scope::Local => {
            // Project-local: <repo-root>/.claude/settings.json. Falls back to CWD
            // if not inside a git repo (caller already warned).
            let root = current_repo_root().unwrap_or_else(|_| std::env::current_dir().unwrap());
            Ok(root.join(".claude").join("settings.json"))
        }
    }
}

// ─── existing diff command (preserved) ──────────────────────────────────────

pub async fn diff(sha_a: String, sha_b: String) -> Result<()> {
    let (repo_dir, _walgit_dir, _local) = find_repo()?;
    let a = load_trace(&repo_dir, &sha_a)?;
    let b = load_trace(&repo_dir, &sha_b)?;

    ui::header(&format!(
        "trace diff: {} → {}",
        ui::short_hash(&a.sha),
        ui::short_hash(&b.sha)
    ));

    diff_str("agent_id", &a.trace.agent_id, &b.trace.agent_id);
    diff_str("task", &a.trace.task, &b.trace.task);
    diff_optional_str(
        "parent_run_id",
        a.trace.parent_run_id.as_deref(),
        b.trace.parent_run_id.as_deref(),
    );

    println!();
    diff_tools(&a.trace, &b.trace);

    println!();
    diff_str_block("decision", &a.trace.decision, &b.trace.decision);

    println!();
    diff_string_set(
        "alternatives_considered",
        &a.trace.alternatives_considered,
        &b.trace.alternatives_considered,
    );

    println!();
    diff_optional_f32("confidence", a.trace.confidence, b.trace.confidence);
    Ok(())
}

struct Loaded {
    sha: String,
    trace: Trace,
}

fn load_trace(repo_dir: &Path, sha: &str) -> Result<Loaded> {
    let full_sha = git::rev_parse(repo_dir, sha)?;
    let msg = git::read_commit_message(repo_dir, &full_sha)?;
    let Some(json) = trace::extract_trace_json(&msg) else {
        return Err(WalGitError::other(format!(
            "commit {} has no reasoning trace",
            full_sha
        )));
    };
    let trace = Trace::parse(&json)?;
    Ok(Loaded {
        sha: full_sha,
        trace,
    })
}

fn diff_str(field: &str, a: &str, b: &str) {
    if a == b {
        println!(
            "  {} {} {}",
            style("=").dim(),
            ui::label(&format!("{:<16}", field)),
            ui::dim(a),
        );
    } else {
        println!(
            "  {} {} {} → {}",
            style("~").yellow().bold(),
            ui::label(&format!("{:<16}", field)),
            style(a).red(),
            style(b).green(),
        );
    }
}

fn diff_optional_str(field: &str, a: Option<&str>, b: Option<&str>) {
    let a = a.unwrap_or("(none)");
    let b = b.unwrap_or("(none)");
    diff_str(field, a, b);
}

fn diff_optional_f32(field: &str, a: Option<f32>, b: Option<f32>) {
    let a_s = a.map(|n| format!("{:.2}", n)).unwrap_or("(none)".into());
    let b_s = b.map(|n| format!("{:.2}", n)).unwrap_or("(none)".into());
    diff_str(field, &a_s, &b_s);
}

fn diff_str_block(field: &str, a: &str, b: &str) {
    println!("  {}", ui::label(field));
    if a == b {
        for line in a.lines() {
            println!("    {} {}", style("=").dim(), ui::dim(line));
        }
    } else {
        for line in a.lines() {
            println!("    {} {}", style("-").red().bold(), style(line).red());
        }
        for line in b.lines() {
            println!("    {} {}", style("+").green().bold(), style(line).green());
        }
    }
}

fn diff_tools(a: &Trace, b: &Trace) {
    let names_a: Vec<&str> = a.tools_called.iter().map(|t| t.name.as_str()).collect();
    let names_b: Vec<&str> = b.tools_called.iter().map(|t| t.name.as_str()).collect();
    println!("  {}", ui::label("tools_called"));
    if names_a == names_b {
        for tc in &a.tools_called {
            println!(
                "    {} {} {}",
                style("=").dim(),
                ui::highlight(&tc.name),
                ui::dim(&tc.input_summary),
            );
        }
    } else {
        let set_a: HashSet<&str> = names_a.iter().copied().collect();
        let set_b: HashSet<&str> = names_b.iter().copied().collect();
        for tc in &a.tools_called {
            let marker = if set_b.contains(tc.name.as_str()) {
                style("=").dim().to_string()
            } else {
                style("-").red().bold().to_string()
            };
            println!(
                "    {} {} {}",
                marker,
                style(&tc.name).red(),
                ui::dim(&tc.input_summary),
            );
        }
        for tc in &b.tools_called {
            if set_a.contains(tc.name.as_str()) {
                continue;
            }
            println!(
                "    {} {} {}",
                style("+").green().bold(),
                style(&tc.name).green(),
                ui::dim(&tc.input_summary),
            );
        }
    }
}

fn diff_string_set(field: &str, a: &[String], b: &[String]) {
    let set_a: HashSet<&String> = a.iter().collect();
    let set_b: HashSet<&String> = b.iter().collect();
    println!("  {}", ui::label(field));
    if set_a == set_b {
        if a.is_empty() {
            println!("    {}", ui::dim("(empty)"));
        } else {
            for v in a {
                println!("    {} {}", style("=").dim(), ui::dim(v));
            }
        }
    } else {
        for v in a {
            let marker = if set_b.contains(v) {
                style("=").dim().to_string()
            } else {
                style("-").red().bold().to_string()
            };
            println!("    {} {}", marker, style(v).red());
        }
        for v in b {
            if set_a.contains(v) {
                continue;
            }
            println!("    {} {}", style("+").green().bold(), style(v).green());
        }
    }
}

// ─── helpers ───────────────────────────────────────────────────────────────

fn current_git_dir() -> Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    git::git_dir(&cwd)
}

/// Resolve the current repo's git dir, honouring `--only-if-enabled`:
///
/// - If `gated` is `false`: same as `current_git_dir()` but errors are
///   propagated.
/// - If `gated` is `true`: returns `Ok(None)` when either (a) we're not
///   inside a git repo or (b) the repo lacks the opt-in marker file. The
///   caller treats `None` as "exit 0 silently". This is what lets one
///   user-global Claude Code hook coexist with non-walgit repos.
fn resolve_git_dir_or_skip(gated: bool) -> Result<Option<PathBuf>> {
    if !gated {
        return Ok(Some(current_git_dir()?));
    }
    let git_dir = match current_git_dir() {
        Ok(d) => d,
        Err(_) => return Ok(None),
    };
    if !trace_pending::is_enabled(&git_dir) {
        return Ok(None);
    }
    Ok(Some(git_dir))
}

fn current_repo_root() -> Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    // `git rev-parse --show-toplevel` is the simplest way and works in
    // worktrees. We don't expose it from git.rs because no other caller
    // needs it yet.
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(&cwd)
        .output()
        .map_err(|e| WalGitError::git(format!("git rev-parse: {}", e)))?;
    if !out.status.success() {
        return Err(WalGitError::git("not in a git repo"));
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    ))
}

fn generate_run_id() -> String {
    use rand::RngCore;
    let mut buf = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut buf);
    format!("{}-{}", chrono::Utc::now().timestamp(), hex::encode(buf))
}

/// Read stdin to a JSON value. Returns `Value::Null` (rather than erroring)
/// if stdin is empty or not JSON — Claude Code hooks must never block the
/// agent on a malformed payload from us.
fn read_stdin_json_silent() -> Result<Value> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    if buf.trim().is_empty() {
        return Ok(Value::Null);
    }
    Ok(serde_json::from_str(&buf).unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn summarize_value_picks_meaningful_key() {
        let v = json!({"file_path": "src/main.rs", "limit": 100});
        assert_eq!(summarize_value(Some(&v)), "src/main.rs");
    }

    #[test]
    fn summarize_value_falls_back_to_json() {
        let v = json!({"x": 1, "y": 2});
        let s = summarize_value(Some(&v));
        assert!(s.contains("\"x\""));
    }

    #[test]
    fn apply_post_tool_use_appends_call() {
        let mut pt = PendingTrace::new("a".into(), "r".into(), None);
        let payload = json!({
            "tool_name": "Read",
            "tool_input": {"file_path": "src/lib.rs"},
            "tool_response": {"file": {"content": "fn main() {}\n"}}
        });
        apply_claude_event(&mut pt, ClaudeEvent::PostToolUse, &payload);
        assert_eq!(pt.tools_called.len(), 1);
        assert_eq!(pt.tools_called[0].name, "Read");
        assert_eq!(pt.tools_called[0].input_summary, "src/lib.rs");
        assert!(pt.tools_called[0].output_summary.contains("lines"));
    }

    #[test]
    fn bash_summary_strips_json_envelope() {
        let resp = json!({
            "stdout": "line one\nline two\n",
            "stderr": "",
            "interrupted": false
        });
        assert_eq!(summarize_tool_output("Bash", Some(&resp)), "line one");
    }

    #[test]
    fn read_summary_uses_content_length() {
        let resp = json!({"file": {"content": "a\nb\nc\n"}});
        assert_eq!(
            summarize_tool_output("Read", Some(&resp)),
            "3 lines, 6 bytes"
        );
    }

    #[test]
    fn edit_summary_is_ok() {
        let resp = json!({"oldString": "x", "newString": "y", "filePath": "a.rs"});
        assert_eq!(summarize_tool_output("Edit", Some(&resp)), "ok");
    }

    #[test]
    fn bash_input_uses_command_field() {
        let inp = json!({"command": "ls -la", "description": "list files"});
        assert_eq!(summarize_tool_input("Bash", Some(&inp)), "ls -la");
    }

    #[test]
    fn resolve_agent_arg_all_returns_only_available() {
        let v = resolve_agent_arg("all").unwrap();
        assert!(v.iter().all(|a| a.status == AgentStatus::Available));
        assert!(v.iter().any(|a| a.key == "claude-code"));
        assert!(!v.iter().any(|a| a.key == "codex")); // planned, excluded
    }

    #[test]
    fn resolve_agent_arg_accepts_alias() {
        let v = resolve_agent_arg("claude").unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].key, "claude-code");
    }

    #[test]
    fn resolve_agent_arg_accepts_comma_list() {
        let v = resolve_agent_arg("claude-code,codex").unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].key, "claude-code");
        assert_eq!(v[1].key, "codex");
    }

    #[test]
    fn resolve_agent_arg_rejects_unknown() {
        let e = resolve_agent_arg("emacs-doctor").unwrap_err();
        assert!(format!("{}", e).contains("unknown agent"));
    }

    #[test]
    fn apply_user_prompt_sets_task_only_first_time() {
        let mut pt = PendingTrace::new("a".into(), "r".into(), None);
        apply_claude_event(
            &mut pt,
            ClaudeEvent::UserPrompt,
            &json!({"prompt": "fix the flaky test"}),
        );
        assert_eq!(pt.task, "fix the flaky test");
        apply_claude_event(
            &mut pt,
            ClaudeEvent::UserPrompt,
            &json!({"prompt": "also rename foo to bar"}),
        );
        // Second prompt does NOT overwrite the task — first user prompt wins.
        assert_eq!(pt.task, "fix the flaky test");
    }
}
