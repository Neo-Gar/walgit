// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

use super::helpers::{current_git_dir, current_repo_root};
use crate::commands::find_repo;
use crate::error::{Result, WalGitError};
use crate::trace_pending::{self, PendingTrace};
use crate::ui;
use std::path::{Path, PathBuf};

/// Header line stamped at the top of every MemWal payload so that `walgit
/// show --trace <sha>` can recall by exact commit SHA and recognise our own
/// uploads (vs. arbitrary entries someone put into the namespace).
const MEMWAL_HEADER_PREFIX: &str = "walgit-trace commit:";

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
    let candidates: Vec<(String, PathBuf)> = shas
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

// ─── manual upload / recall ──────────────────────────────────────────────────

/// Push one or all local trace snapshots to MemWal. The trace JSON itself is
/// uploaded as the memory `text` — the relayer handles embedding + Seal.
///
/// `namespace` defaults to the basename of the repo root, which is a stable
/// per-repo bucket for the user's MemWal account.
pub async fn upload(commit: Option<String>, namespace_override: Option<String>) -> Result<()> {
    use crate::memwal::MemWalClient;

    let git_dir = current_git_dir()?;

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

    let entries: Vec<(String, PathBuf)> = match commit {
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

// ─── serialization helpers ───────────────────────────────────────────────────

/// Pack a snapshot for MemWal indexing.
///
/// Format: one-line header (`walgit-trace commit:<sha>`) followed by the
/// raw `PendingTrace` JSON body. Both parts are embedded in `text` for the
/// relayer to embed; on read we strip the header and reparse the JSON to
/// reconstruct a typed [`Trace`]. The header is also a strong semantic
/// signal — a recall query containing `<sha>` will preferentially match.
pub fn format_for_memwal(commit_sha: &str, pt: &PendingTrace) -> String {
    let body = serde_json::to_string_pretty(pt).unwrap_or_else(|_| "{}".to_string());
    format!("{}{}\n{}", MEMWAL_HEADER_PREFIX, commit_sha, body)
}

/// Inverse of [`format_for_memwal`]. Returns `Some((commit_sha, PendingTrace))`
/// when `text` is something we wrote; `None` if it doesn't match our shape
/// (e.g., a user manually put a different memory in the same namespace).
pub fn parse_memwal_payload(text: &str) -> Option<(String, PendingTrace)> {
    let mut lines = text.lines();
    let header = lines.next()?;
    let sha = header
        .strip_prefix(MEMWAL_HEADER_PREFIX)?
        .trim()
        .to_string();
    let body: String = lines.collect::<Vec<_>>().join("\n");
    let pt: PendingTrace = serde_json::from_str(body.trim()).ok()?;
    Some((sha, pt))
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
    if let Ok((_, _, local)) = find_repo() {
        if !local.id.is_empty() && local.id != "pending" {
            return Some(local.id);
        }
    }
    let root = current_repo_root().ok()?;
    let dir = root.file_name()?.to_str()?.to_string();
    Some(dir)
}
