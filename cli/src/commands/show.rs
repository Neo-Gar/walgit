// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

//! `walgit show [<commit>] [--trace]` — print a single commit. With `--trace`,
//! render the reasoning trace. Lookup order:
//!
//! 1. **Local snapshot** at `<git-dir>/walgit/traces/<sha>.json` — set on
//!    every commit by the `post-commit` hook. Fast, no network.
//! 2. **MemWal recall** keyed by commit SHA against the namespace = repo's
//!    Sui id. Lets reviewers / future-self read traces from commits made on
//!    other machines, after a push has shipped them upstream.
//! 3. **Commit message footer** — only present on commits made through
//!    `walgit agent commit` (the standalone trace-in-message path).

use crate::commands::find_repo;
use crate::commands::trace::parse_memwal_payload;
use crate::config;
use crate::error::{Result, WalGitError};
use crate::memwal::MemWalClient;
use crate::trace::{self, Trace};
use crate::trace_pending;
use crate::{git, ui};
use console::style;

pub async fn run(commit: String, show_trace: bool) -> Result<()> {
    let (repo_dir, _walgit_dir, _local) = find_repo()?;
    let sha = git::rev_parse(&repo_dir, &commit)?;
    let full = git::read_commit_message(&repo_dir, &sha)?;

    let plain = trace::strip_trace_block(&full);
    let plain = plain.trim_end();

    ui::header(&format!("commit {}", ui::short_hash(&sha)));
    for (i, line) in plain.lines().enumerate() {
        if i == 0 {
            println!("  {}", ui::highlight(line));
        } else if line.is_empty() {
            println!();
        } else {
            println!("  {}", line);
        }
    }

    if !show_trace {
        return Ok(());
    }

    ui::header("trace");
    match resolve_trace(&sha, &full).await? {
        Some((t, src)) => {
            ui::info(format!("source: {}", src));
            println!();
            render_trace(&t);
        }
        None => ui::info("no reasoning trace found (local cache, MemWal, or commit footer)"),
    }
    Ok(())
}

/// Try every resolution path in order. Returns the trace plus a human-readable
/// label for where it came from.
async fn resolve_trace(sha: &str, full_message: &str) -> Result<Option<(Trace, String)>> {
    // 1) Local snapshot in `.git/walgit/traces/<sha>.json` — fast, offline.
    let cwd = std::env::current_dir()?;
    if let Ok(git_dir) = git::git_dir(&cwd) {
        let p = trace_pending::trace_path(&git_dir, sha);
        if p.exists() {
            let pt = trace_pending::load_snapshot(&p)?;
            let (t, _warnings) = pt.into_trace();
            return Ok(Some((t, format!("local cache: {}", p.display()))));
        }
    }

    // 2) MemWal recall. Namespace = repo's Sui id (matches how push uploads).
    if let Ok((_, _, local)) = find_repo() {
        if !local.id.is_empty() && local.id != "pending" {
            if let Some(t) = try_memwal_resolve(sha, &local.id).await? {
                return Ok(Some((t, format!("MemWal namespace {}", &local.id[..10]))));
            }
        }
    }

    // 3) Commit message footer — only `walgit agent commit` writes this.
    if let Some(raw_json) = trace::extract_trace_json(full_message) {
        match Trace::parse(&raw_json) {
            Ok(t) => return Ok(Some((t, "commit message footer".to_string()))),
            Err(e) => {
                ui::warn(format!("commit-message trace present but unparseable: {}", e));
            }
        }
    }

    Ok(None)
}

async fn try_memwal_resolve(sha: &str, namespace: &str) -> Result<Option<Trace>> {
    let cfg = config::load()?;
    let Some(mw) = cfg.memwal.as_ref() else {
        return Ok(None);
    };
    let priv_bytes = mw.load_delegate_key()?;
    let client = MemWalClient::new(mw.relayer_url.clone(), mw.account_id.clone(), priv_bytes);
    let resp = client.recall(sha, Some(3), Some(namespace)).await?;

    // The top result is our most likely hit — the SHA is in the header line
    // so it should rank #1 for a query equal to the SHA. Scan all returned
    // hits and take the one whose parsed header matches the requested SHA.
    for m in resp.results {
        let Some(text) = m.text else { continue };
        if let Some((stored_sha, pt)) = parse_memwal_payload(&text) {
            if stored_sha == sha {
                let (t, _warnings) = pt.into_trace();
                return Ok(Some(t));
            }
        }
    }
    // If MemWal returned matches but we filtered them out by SHA, surface that
    // — it's diagnostic info for "wrong namespace?" debugging.
    if let Some(dropped) = resp.dropped_count {
        if dropped > 0 {
            ui::info(format!(
                "MemWal returned {} item(s) below similarity threshold",
                dropped
            ));
        }
    }
    Ok(None)
}

pub fn render_trace(t: &Trace) {
    println!(
        "  {} {} {}",
        ui::label("agent   "),
        ui::highlight(&t.agent_id),
        ui::dim(&format!("(run {})", short_run(&t.run_id))),
    );
    if let Some(parent) = &t.parent_run_id {
        println!(
            "  {} {}",
            ui::label("parent  "),
            ui::dim(&short_run(parent))
        );
    }
    println!("  {} {}", ui::label("task    "), ui::highlight(&t.task));
    if let Some(c) = t.confidence {
        let painted = if c >= 0.7 {
            style(format!("{:.2}", c)).green()
        } else if c >= 0.4 {
            style(format!("{:.2}", c)).yellow()
        } else {
            style(format!("{:.2}", c)).red()
        };
        println!("  {} {}", ui::label("confid. "), painted);
    }
    println!();
    if !t.tools_called.is_empty() {
        println!("  {}", ui::label("tools called"));
        for call in &t.tools_called {
            println!(
                "    {} {} {} → {}",
                ui::dim("·"),
                ui::highlight(&call.name),
                ui::dim(&call.input_summary),
                ui::dim(&call.output_summary),
            );
        }
        println!();
    }
    println!("  {}", ui::label("decision"));
    for line in t.decision.lines() {
        println!("    {}", line);
    }
    if !t.alternatives_considered.is_empty() {
        println!();
        println!("  {}", ui::label("alternatives considered"));
        for alt in &t.alternatives_considered {
            println!("    {} {}", ui::dim("✗"), alt);
        }
    }
}

fn short_run(id: &str) -> String {
    if id.len() > 12 {
        format!("{}…", &id[..12])
    } else {
        id.to_string()
    }
}

// Surface a typed error so callers can match if needed.
#[allow(dead_code)]
fn _typed_err() -> WalGitError {
    WalGitError::other("placeholder".to_string())
}
