// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

//! Pending reasoning trace — an in-progress accumulator that lives inside
//! `.git/walgit/pending-trace.json` between `walgit trace start` and the next
//! commit. Agent hooks (Claude Code, Codex, …) append tool calls and decision
//! fields here as the session progresses; `walgit trace flush` (invoked by the
//! `prepare-commit-msg` git hook) consumes it into the commit message footer.
//!
//! Design notes:
//!
//! - Required fields (`task`, `decision`) may be empty during the session and
//!   are only enforced at flush time, with a permissive fallback so a missing
//!   decision never blocks the user's commit — it surfaces as a warning and a
//!   placeholder. This keeps the system out of the user's way.
//! - Writes are atomic (`write tmp → rename`) so a crashed hook never leaves a
//!   half-written file that breaks the next read.
//! - File format is JSON; the on-disk shape is intentionally close to
//!   [`crate::trace::Trace`] so flushing is a near-identity transform.

use crate::error::{Result, WalGitError};
use crate::trace::{ToolCall, Trace};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// On-disk layout of `.git/walgit/pending-trace.json`. Mirrors [`Trace`] but
/// every field is optional during accumulation.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct PendingTrace {
    #[serde(default = "default_version")]
    pub version: String,
    pub agent_id: String,
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    #[serde(default)]
    pub task: String,
    #[serde(default)]
    pub tools_called: Vec<ToolCall>,
    #[serde(default)]
    pub decision: String,
    #[serde(default)]
    pub alternatives_considered: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub extensions: Value,

    /// Source adapter that opened this pending trace (e.g. `"claude-code"`,
    /// `"codex"`, `"manual"`). Used for diagnostics only; not part of the
    /// emitted footer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Unix timestamp (seconds) when `walgit trace start` ran. Surface in
    /// `walgit trace status` so users can spot stale pending traces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
}

fn default_version() -> String {
    "1".to_string()
}

impl PendingTrace {
    pub fn new(agent_id: String, run_id: String, source: Option<String>) -> Self {
        Self {
            version: "1".into(),
            agent_id,
            run_id,
            parent_run_id: None,
            task: String::new(),
            tools_called: Vec::new(),
            decision: String::new(),
            alternatives_considered: Vec::new(),
            confidence: None,
            extensions: Value::Null,
            source,
            started_at: Some(chrono::Utc::now().timestamp()),
        }
    }

    /// Convert into a strict [`Trace`]. Used at flush time. Fills sensible
    /// placeholders for required schema fields if the agent never set them,
    /// surfacing a list of warnings the caller may print.
    pub fn into_trace(self) -> (Trace, Vec<String>) {
        let mut warnings = Vec::new();
        let task = if self.task.trim().is_empty() {
            warnings.push("trace.task was empty at flush — using placeholder".into());
            "(task not recorded)".into()
        } else {
            self.task
        };
        let decision = if self.decision.trim().is_empty() {
            warnings.push("trace.decision was empty at flush — using placeholder".into());
            "(no decision recorded by agent)".into()
        } else {
            self.decision
        };
        let trace = Trace {
            version: "1".into(),
            agent_id: self.agent_id,
            run_id: self.run_id,
            parent_run_id: self.parent_run_id,
            task,
            tools_called: self.tools_called,
            decision,
            alternatives_considered: self.alternatives_considered,
            confidence: self.confidence,
            extensions: self.extensions,
        };
        (trace, warnings)
    }

    /// Append a tool call. Long summaries are truncated to the schema limit
    /// (200 chars) so an agent dumping a 20 KB stdout body doesn't blow the
    /// trace budget — they should summarize at the source, but we defend in
    /// depth here.
    pub fn push_tool(&mut self, mut call: ToolCall) {
        call.input_summary = truncate(&call.input_summary, 200);
        call.output_summary = truncate(&call.output_summary, 200);
        self.tools_called.push(call);
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

/// Path to `.git/walgit/`. Created on demand.
pub fn walgit_dir(git_dir: &Path) -> PathBuf {
    git_dir.join("walgit")
}

pub fn pending_path(git_dir: &Path) -> PathBuf {
    walgit_dir(git_dir).join("pending-trace.json")
}

pub fn last_path(git_dir: &Path) -> PathBuf {
    walgit_dir(git_dir).join("last-trace.json")
}

/// Marker file written by `walgit trace install` to opt this repo into the
/// global Claude Code hook. Without it, hooks installed at user-global level
/// no-op so other repos aren't silently polluted.
pub fn enabled_path(git_dir: &Path) -> PathBuf {
    walgit_dir(git_dir).join("enabled")
}

/// Per-commit trace snapshots live here. The `post-commit` hook drops one
/// JSON file per commit (`<sha>.json`); push-time upload reads from this
/// directory and ships entries to MemWal.
pub fn traces_dir(git_dir: &Path) -> PathBuf {
    walgit_dir(git_dir).join("traces")
}

pub fn trace_path(git_dir: &Path, commit_sha: &str) -> PathBuf {
    traces_dir(git_dir).join(format!("{}.json", commit_sha))
}

/// Assert that `sha` is safe to use as a filesystem path component.
/// Rejects path-traversal via `--commit ../../evil` before touching the disk.
fn validate_sha(sha: &str) -> Result<()> {
    if sha.len() < 7 || sha.len() > 64 {
        return Err(WalGitError::other(format!(
            "commit SHA '{}' must be 7–64 characters (got {})",
            sha,
            sha.len()
        )));
    }
    if !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(WalGitError::other(format!(
            "commit SHA '{}' contains non-hex characters — expected a git SHA",
            sha
        )));
    }
    Ok(())
}

/// Persist `pt` as `traces/<commit_sha>.json`. Atomic; creates dirs on demand.
pub fn save_snapshot(git_dir: &Path, commit_sha: &str, pt: &PendingTrace) -> Result<PathBuf> {
    validate_sha(commit_sha)?;
    let dir = traces_dir(git_dir);
    std::fs::create_dir_all(&dir)?;
    let final_path = trace_path(git_dir, commit_sha);
    let tmp_path = dir.join(format!("{}.json.tmp", commit_sha));
    std::fs::write(&tmp_path, serde_json::to_string_pretty(pt)?)?;
    std::fs::rename(&tmp_path, &final_path)?;
    Ok(final_path)
}

/// Enumerate snapshots present locally. Used by `walgit trace upload --all`
/// to figure out which commits still need to ship to MemWal.
pub fn list_snapshots(git_dir: &Path) -> Result<Vec<(String, PathBuf)>> {
    let dir = traces_dir(git_dir);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let p = entry.path();
        let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        // Skip the `.tmp` partials atomic-rename can leave on crash.
        if p.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        out.push((stem.to_string(), p));
    }
    Ok(out)
}

pub fn load_snapshot(path: &Path) -> Result<PendingTrace> {
    let raw = std::fs::read_to_string(path)?;
    serde_json::from_str(&raw)
        .map_err(|e| WalGitError::other(format!("snapshot {} parse: {}", path.display(), e)))
}

pub fn is_enabled(git_dir: &Path) -> bool {
    enabled_path(git_dir).exists()
}

pub fn mark_enabled(git_dir: &Path) -> Result<()> {
    let dir = walgit_dir(git_dir);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(enabled_path(git_dir), "")?;
    Ok(())
}

pub fn unmark_enabled(git_dir: &Path) -> Result<()> {
    let p = enabled_path(git_dir);
    if p.exists() {
        std::fs::remove_file(p)?;
    }
    Ok(())
}

/// Load the pending trace if present. Returns `Ok(None)` when the file does
/// not exist (the common no-pending case) so callers can distinguish that
/// from a parse error.
pub fn load(git_dir: &Path) -> Result<Option<PendingTrace>> {
    let p = pending_path(git_dir);
    if !p.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&p)?;
    let pt: PendingTrace = serde_json::from_str(&raw)
        .map_err(|e| WalGitError::other(format!("pending-trace.json parse: {}", e)))?;
    Ok(Some(pt))
}

/// Atomically write the pending trace. Creates parent dirs on demand.
pub fn save(git_dir: &Path, pt: &PendingTrace) -> Result<()> {
    let dir = walgit_dir(git_dir);
    std::fs::create_dir_all(&dir)?;
    let final_path = pending_path(git_dir);
    let tmp_path = dir.join("pending-trace.json.tmp");
    let json = serde_json::to_string_pretty(pt)?;
    std::fs::write(&tmp_path, json)?;
    std::fs::rename(&tmp_path, &final_path)?;
    Ok(())
}

/// Remove the pending trace if present. Idempotent.
pub fn delete(git_dir: &Path) -> Result<()> {
    let p = pending_path(git_dir);
    if p.exists() {
        std::fs::remove_file(p)?;
    }
    Ok(())
}

/// Move pending → last (audit copy). Returns the moved trace.
pub fn consume(git_dir: &Path) -> Result<Option<PendingTrace>> {
    let Some(pt) = load(git_dir)? else {
        return Ok(None);
    };
    let dir = walgit_dir(git_dir);
    std::fs::create_dir_all(&dir)?;
    let from = pending_path(git_dir);
    let to = last_path(git_dir);
    // rename may fail across filesystems on exotic setups; fall back to copy.
    if std::fs::rename(&from, &to).is_err() {
        std::fs::copy(&from, &to)?;
        std::fs::remove_file(&from)?;
    }
    Ok(Some(pt))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn save_load_round_trip() {
        let td = TempDir::new().unwrap();
        let pt = PendingTrace::new("writer-v1".into(), "run-1".into(), Some("test".into()));
        save(td.path(), &pt).unwrap();
        let loaded = load(td.path()).unwrap().unwrap();
        assert_eq!(loaded.agent_id, "writer-v1");
        assert_eq!(loaded.run_id, "run-1");
        assert!(loaded.tools_called.is_empty());
    }

    #[test]
    fn load_returns_none_when_absent() {
        let td = TempDir::new().unwrap();
        assert!(load(td.path()).unwrap().is_none());
    }

    #[test]
    fn into_trace_warns_on_empty_required_fields() {
        let pt = PendingTrace::new("agent".into(), "run".into(), None);
        let (trace, warnings) = pt.into_trace();
        assert_eq!(warnings.len(), 2);
        assert_eq!(trace.task, "(task not recorded)");
        assert!(trace.decision.starts_with("(no decision"));
    }

    #[test]
    fn into_trace_clean_when_filled() {
        let mut pt = PendingTrace::new("agent".into(), "run".into(), None);
        pt.task = "do the thing".into();
        pt.decision = "did the thing because reasons".into();
        let (trace, warnings) = pt.into_trace();
        assert!(warnings.is_empty());
        assert_eq!(trace.task, "do the thing");
    }

    #[test]
    fn push_tool_truncates_long_summary() {
        let mut pt = PendingTrace::new("a".into(), "r".into(), None);
        pt.push_tool(ToolCall {
            name: "read_file".into(),
            input_summary: "x".repeat(500),
            output_summary: "y".repeat(500),
        });
        assert_eq!(pt.tools_called[0].input_summary.chars().count(), 200);
        assert!(pt.tools_called[0].input_summary.ends_with('…'));
    }

    #[test]
    fn consume_moves_pending_to_last() {
        let td = TempDir::new().unwrap();
        let pt = PendingTrace::new("a".into(), "r".into(), None);
        save(td.path(), &pt).unwrap();
        let moved = consume(td.path()).unwrap();
        assert!(moved.is_some());
        assert!(!pending_path(td.path()).exists());
        assert!(last_path(td.path()).exists());
    }

    #[test]
    fn save_is_atomic_no_tmp_left() {
        let td = TempDir::new().unwrap();
        let pt = PendingTrace::new("a".into(), "r".into(), None);
        save(td.path(), &pt).unwrap();
        let tmp = walgit_dir(td.path()).join("pending-trace.json.tmp");
        assert!(!tmp.exists());
    }
}
