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
        if let crate::betterleaks::ScanOutcome::SecretsFound { output } =
            crate::betterleaks::scan_text(&text)
        {
            return Err(WalGitError::other(format!(
                "betterleaks: secrets detected in trace for {} — upload aborted\n{}",
                sha, output
            )));
        }
        let job = client
            .remember(&text, Some(namespace))
            .await
            .map_err(|e| WalGitError::other(format!("MemWal upload for {}: {}", sha, e)))?;

        // Write the job_id into the marker file so we can check status later
        // and know whether the async Walrus upload actually succeeded.
        let job_id = job.job_id.as_deref().unwrap_or("unknown");
        std::fs::write(&marker, job_id)?;
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

    // Gate: require betterleaks before any trace leaves the machine.
    let scan_enabled = if !crate::betterleaks::is_available() {
        if !crate::betterleaks::confirm_continue_without_scan() {
            return Err(WalGitError::other(
                "upload aborted — install betterleaks and retry".to_string(),
            ));
        }
        false
    } else {
        true
    };

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
        if scan_enabled {
            if let crate::betterleaks::ScanOutcome::SecretsFound { output } =
                crate::betterleaks::scan_text(&text)
            {
                failed.push(format!("{} (betterleaks: secrets detected — {})", sha, output));
                ui::warn(format!(
                    "{} skipped: betterleaks detected secrets in trace",
                    ui::short_hash(&sha)
                ));
                continue;
            }
        }
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

/// Pull matches for `query`. Tries MemWal semantic search first; falls back
/// to local keyword search when MemWal has no results (e.g. Walrus upload
/// jobs are still pending or the Enoki rate limit is exhausted).
pub async fn recall(
    query: String,
    limit: u32,
    namespace_override: Option<String>,
    threshold: Option<f32>,
) -> Result<()> {
    use crate::memwal::MemWalClient;

    ui::header(&format!("recall: \"{}\"", query));

    // ── MemWal semantic search ────────────────────────────────────────
    let memwal_found = if let Ok(cfg) = crate::config::load() {
        if let (Some(mw), Ok(priv_bytes)) =
            (cfg.memwal.as_ref(), cfg.memwal.as_ref().map(|m| m.load_delegate_key()).unwrap_or(Err(crate::error::WalGitError::other(""))))
        {
            let client = MemWalClient::new(mw.relayer_url.clone(), mw.account_id.clone(), priv_bytes);
            let namespace = namespace_override
                .clone()
                .unwrap_or_else(|| default_namespace().unwrap_or_else(|| "default".into()));
            match client.recall(&query, Some(limit), Some(&namespace), threshold).await {
                Ok(resp) if !resp.results.is_empty() => {
                    for (i, m) in resp.results.iter().enumerate() {
                        let dist_str = m
                            .distance
                            .map(|d| format!("dist={:.3}", d))
                            .unwrap_or_else(|| "-".into());
                        println!("  {}", ui::dim(&format!("#{} {} [MemWal]", i + 1, dist_str)));
                        if let Some(t) = &m.text {
                            let display = t.split("---json---").next().unwrap_or(t.as_str());
                            for line in display.lines().take(6) {
                                println!("    {}", line);
                            }
                            if display.lines().count() > 6 {
                                println!("    {}", ui::dim("…"));
                            }
                        }
                        println!();
                    }
                    true
                }
                Ok(resp) => {
                    let total = resp.total.unwrap_or(0);
                    let dropped = resp.dropped_count.unwrap_or(0);
                    if total == 0 && dropped == 0 {
                        ui::info(
                            "MemWal: namespace is empty — traces may not have uploaded yet",
                        );
                    } else if dropped > 0 {
                        ui::info(format!(
                            "MemWal: {dropped} trace(s) found but could not be decrypted (SEAL error)"
                        ));
                    }
                    false
                }
                Err(e) => {
                    ui::warn(format!("MemWal: {}", e));
                    false
                }
            }
        } else { false }
    } else { false };

    if memwal_found {
        return Ok(());
    }

    // ── Local keyword fallback ────────────────────────────────────────
    let git_dir = match current_git_dir() {
        Ok(d) => d,
        Err(_) => {
            ui::info("not inside a git repository — no local traces to search");
            return Ok(());
        }
    };

    let snapshots = trace_pending::list_snapshots(&git_dir).unwrap_or_default();
    if snapshots.is_empty() {
        ui::info("no local trace snapshots found in this repository");
        return Ok(());
    }

    let q_lower = query.to_lowercase();
    let mut hits: Vec<(String, String)> = snapshots
        .iter()
        .filter_map(|(sha, path)| {
            let pt = trace_pending::load_snapshot(path).ok()?;
            let text = format_for_memwal(sha, &pt);
            if text.to_lowercase().contains(&q_lower) {
                Some((sha.clone(), text))
            } else {
                None
            }
        })
        .take(limit as usize)
        .collect();
    hits.sort_by(|(a, _), (b, _)| b.cmp(a)); // newest SHA first

    if hits.is_empty() {
        ui::info(format!("no local traces contain \"{}\"", query));
    } else {
        println!("  {}", ui::dim("local search:"));
        println!();
        for (i, (sha, text)) in hits.iter().enumerate() {
            println!(
                "  {}",
                ui::dim(&format!("#{} {} [local]", i + 1, ui::short_hash(sha)))
            );
            for line in text.lines().take(6) {
                println!("    {}", line);
            }
            if text.lines().count() > 6 {
                println!("    {}", ui::dim("…"));
            }
            println!();
        }
    }

    Ok(())
}

// ─── serialization helpers ───────────────────────────────────────────────────

/// Separator used in older v2 payloads that included a JSON block.
/// Kept only for [`parse_memwal_payload`] backward-compat parsing.
const JSON_SEPARATOR: &str = "---json---";

/// Pack a snapshot for MemWal indexing.
///
/// We send **only the natural-language block** — no JSON. Embedding a raw JSON
/// blob alongside the NL text destroys cosine-similarity scores because the
/// tokeniser treats `{}[]":` as noise with no semantic content. The full
/// structured trace lives in the local `.git/walgit/traces/<sha>.json` file
/// and is not needed for search.
pub fn format_for_memwal(commit_sha: &str, pt: &PendingTrace) -> String {
    natural_language_block(commit_sha, pt)
}

/// Return the last component of a path string (file or directory name).
/// Falls back to the whole string if there's no separator.
fn basename(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path)
        .to_string()
}

/// Strip `git -C /some/absolute/path ` prefixes from every `&&`-chained
/// segment. E.g. `git -C /abs add f && git -C /abs commit -m "msg"` →
/// `git add f && git commit -m "msg"`.
fn strip_git_dash_c(cmd: &str) -> String {
    cmd.split("&&")
        .map(|seg| strip_one_git_dash_c(seg.trim()))
        .collect::<Vec<_>>()
        .join(" && ")
}

fn strip_one_git_dash_c(seg: &str) -> String {
    if let Some(rest) = seg.strip_prefix("git -C ") {
        if let Some(space) = rest.find(' ') {
            let path_candidate = &rest[..space];
            if path_candidate.starts_with('/') {
                return format!("git {}", rest[space + 1..].trim());
            }
        }
    }
    seg.to_string()
}

/// Build the human-readable block that gets embedded by MemWal.
fn natural_language_block(commit_sha: &str, pt: &PendingTrace) -> String {
    let mut lines = vec![
        format!("{}{}", MEMWAL_HEADER_PREFIX, commit_sha),
        format!("agent: {}", pt.agent_id),
    ];

    // Task: use the stored value or fall back to a hint derived from tools.
    let task = pt.task.trim();
    if !task.is_empty() {
        lines.push(format!("task: {}", task));
    } else if let Some(hint) = derive_task_hint(pt) {
        lines.push(format!("task: {}", hint));
    }

    // Files touched by Edit/Write/Read operations. Use only the final
    // component of the path so absolute paths don't add noise to embeddings.
    let files: Vec<String> = pt
        .tools_called
        .iter()
        .filter(|t| {
            matches!(
                t.name.as_str(),
                "Read" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit"
            )
        })
        .map(|t| basename(t.input_summary.trim()))
        .filter(|s| !s.is_empty())
        .collect();
    if !files.is_empty() {
        lines.push(format!("files modified: {}", files.join(", ")));
    }

    // Bash commands — strip `git -C /absolute/path` prefixes so the actual
    // intent (e.g. "git add test.txt") is what the embedding model sees.
    let cmds: Vec<String> = pt
        .tools_called
        .iter()
        .filter(|t| t.name == "Bash")
        .map(|t| strip_git_dash_c(t.input_summary.trim()))
        .filter(|s| !s.is_empty())
        .collect();
    if !cmds.is_empty() {
        lines.push(format!("commands: {}", cmds.join("; ")));
    }

    // Deduplicated list of tools (order-preserving).
    if !pt.tools_called.is_empty() {
        let mut seen = std::collections::HashSet::new();
        let unique: Vec<&str> = pt
            .tools_called
            .iter()
            .map(|t| t.name.as_str())
            .filter(|&n| seen.insert(n))
            .collect();
        lines.push(format!("tools used: {}", unique.join(", ")));
    }

    if !pt.decision.trim().is_empty() {
        lines.push(format!("decision: {}", pt.decision.trim()));
    }
    if !pt.alternatives_considered.is_empty() {
        lines.push(format!(
            "alternatives: {}",
            pt.alternatives_considered.join("; ")
        ));
    }

    lines.join("\n")
}

/// Try to extract a short task description from Bash commands when the
/// `task` field is empty.
///
/// Handles two patterns from Claude Code commits:
/// - Inline:  `git commit -m "fix the bug"`
/// - Heredoc: `git commit -m "$(cat <<'EOF'\nfix the bug\n\nCo-Authored-By…\nEOF\n)"`
fn derive_task_hint(pt: &PendingTrace) -> Option<String> {
    for tc in &pt.tools_called {
        if tc.name != "Bash" {
            continue;
        }
        let cmd = &tc.input_summary;
        let pos = cmd.find("commit -m")?;
        let after = cmd[pos + 9..].trim();

        // Heredoc: $(cat <<'EOF'\n<message subject>\n...)
        // The actual subject line is the first non-empty line after the EOF marker.
        for marker in &["<<'EOF'", "<<EOF", "<<'eof'", "<<eof"] {
            if let Some(eof_pos) = after.find(marker) {
                let body = &after[eof_pos + marker.len()..];
                let subject = body
                    .lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty() && *l != "EOF" && *l != "eof");
                if let Some(s) = subject {
                    return Some(s.to_string());
                }
            }
        }

        // Inline: git commit -m "message" or git commit -m 'message'
        let q = after.chars().next()?;
        if q == '"' || q == '\'' {
            let inner = &after[1..];
            // Don't expand heredoc expansions $(cat ...)
            if inner.starts_with("$(") {
                continue;
            }
            let end = inner.find(q).unwrap_or(inner.len());
            let msg = inner[..end].trim();
            if !msg.is_empty() {
                return Some(msg.lines().next().unwrap_or(msg).to_string());
            }
        }
    }
    None
}

/// Inverse of [`format_for_memwal`]. Returns `Some((commit_sha, PendingTrace))`
/// when `text` is something we wrote; `None` if it doesn't match our shape.
///
/// Handles both v2 (natural language + `---json---` + JSON) and v1 (JSON
/// immediately after the header) so old MemWal entries stay readable.
pub fn parse_memwal_payload(text: &str) -> Option<(String, PendingTrace)> {
    let mut lines = text.lines();
    let header = lines.next()?;
    let sha = header
        .strip_prefix(MEMWAL_HEADER_PREFIX)?
        .trim()
        .to_string();

    let rest: String = lines.collect::<Vec<_>>().join("\n");

    // v2: find the JSON block after the separator.
    let json_body = if let Some(sep_pos) = rest.find(JSON_SEPARATOR) {
        rest[sep_pos + JSON_SEPARATOR.len()..].trim().to_string()
    } else {
        // v1 fallback: everything after the header is JSON.
        rest.trim().to_string()
    };

    let pt: PendingTrace = serde_json::from_str(&json_body).ok()?;
    Some((sha, pt))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::ToolCall;

    fn make_pt_with_bash(cmd: &str) -> PendingTrace {
        let mut pt = PendingTrace::new("claude-code".into(), "run-1".into(), None);
        pt.tools_called.push(ToolCall {
            name: "Bash".into(),
            input_summary: cmd.into(),
            output_summary: "ok".into(),
        });
        pt
    }

    #[test]
    fn derive_task_hint_heredoc() {
        let cmd = "git -C /abs/path add test.txt && git -C /abs/path commit -m \"$(cat <<'EOF'\nupdate test.txt content to HELLO BOYZ!\n\nCo-Authored-By: Claude\nEOF\n)\"";
        let pt = make_pt_with_bash(cmd);
        let hint = derive_task_hint(&pt);
        assert_eq!(hint.as_deref(), Some("update test.txt content to HELLO BOYZ!"));
    }

    #[test]
    fn derive_task_hint_inline() {
        let pt = make_pt_with_bash("git commit -m \"fix the bug\"");
        assert_eq!(derive_task_hint(&pt).as_deref(), Some("fix the bug"));
    }

    #[test]
    fn strip_git_dash_c_chained() {
        let cmd = "git -C /abs/path add test.txt && git -C /abs/path commit -m \"msg\"";
        let result = strip_git_dash_c(cmd);
        assert_eq!(result, "git add test.txt && git commit -m \"msg\"");
    }

    #[test]
    fn format_for_memwal_no_json() {
        let pt = make_pt_with_bash("git -C /abs/path add test.txt && git -C /abs/path commit -m \"$(cat <<'EOF'\nadd test file\nEOF\n)\"");
        let text = format_for_memwal("abc123", &pt);
        assert!(!text.contains("---json---"), "should not contain JSON separator");
        assert!(!text.contains('{'), "should not contain raw JSON");
        assert!(text.contains("task: add test file"));
        assert!(text.contains("git add test.txt"));
    }
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
