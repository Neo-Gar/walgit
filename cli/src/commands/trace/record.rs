// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

use super::helpers::{read_stdin_json_silent, resolve_git_dir_or_skip};
use crate::error::{Result, WalGitError};
use crate::trace::ToolCall;
use crate::trace_pending::{self, PendingTrace};
use serde_json::Value;
use std::path::Path;

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
                PendingTrace::new(
                    "claude-code".to_string(),
                    format!("claude-{}", session_id),
                    Some("claude-code".to_string()),
                )
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
        // `Stop` is special: it's where a session becomes a memory, so it owns
        // its own persistence (finalize / ask-for-summary / discard) and
        // returns directly instead of falling through to the plain save below.
        RecordKind::ClaudeHook {
            event: ClaudeEvent::Stop,
        } => {
            return handle_stop(&git_dir, pt, &payload);
        }
        RecordKind::ClaudeHook { event } => {
            apply_claude_event(&mut pt, event, &payload);
        }
    }

    trace_pending::save(&git_dir, &pt)?;
    Ok(())
}

/// What a `Stop` event should do, decided purely from the trace state. Kept
/// separate from the I/O so the branching logic is unit-testable in isolation.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum StopOutcome {
    /// Read-only session — discard the pending trace, store nothing.
    Discard,
    /// Changed but not committed — keep pending, wait for the commit.
    KeepPending,
    /// Committed without a summary — ask the agent to write its `decision`.
    RequestSummary,
    /// Ready to crystallise into a stored memory.
    Finalize,
}

/// Decide what a `Stop` means for this session. Pure: no disk, no stdout.
///
/// 1. **Read-only** (no edits, no commits) → [`Discard`](StopOutcome::Discard).
///    Keeps MemWal free of "agent just looked around" entries.
/// 2. **Changed but not committed yet** →
///    [`KeepPending`](StopOutcome::KeepPending); there's no SHA to key on.
/// 3. **Committed, no decision yet, first Stop** →
///    [`RequestSummary`](StopOutcome::RequestSummary).
/// 4. **Committed, decision present OR we already asked (`stop_active`)** →
///    [`Finalize`](StopOutcome::Finalize). The `stop_active` arm is the loop
///    guard: even if the agent ignores the request we finalize on the second
///    Stop rather than asking forever.
pub(super) fn decide_stop(pt: &PendingTrace, stop_active: bool) -> StopOutcome {
    if !pt.has_changes() {
        return StopOutcome::Discard;
    }
    if pt.commits.is_empty() {
        return StopOutcome::KeepPending;
    }
    if pt.decision.trim().is_empty() && !stop_active {
        return StopOutcome::RequestSummary;
    }
    StopOutcome::Finalize
}

/// Handle Claude Code's `Stop` hook — the agent has finished its turn. Applies
/// the I/O for the outcome [`decide_stop`] computes.
fn handle_stop(git_dir: &Path, mut pt: PendingTrace, payload: &Value) -> Result<()> {
    let stop_active = payload
        .get("stop_hook_active")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    match decide_stop(&pt, stop_active) {
        StopOutcome::Discard => trace_pending::delete(git_dir),
        // Edited but not committed — persist as-is and wait for the commit.
        StopOutcome::KeepPending => trace_pending::save(git_dir, &pt),
        // Ask the agent to write its own summary; leave pending untouched so
        // the agent's `trace set --decision` fills it before the next Stop.
        StopOutcome::RequestSummary => {
            emit_summary_request();
            Ok(())
        }
        StopOutcome::Finalize => finalize_session(git_dir, &mut pt),
    }
}

/// Emit the Stop-hook control object that asks the agent to summarise its work.
///
/// Claude Code reads a Stop hook's stdout as a JSON control object; `decision:
/// block` prevents the agent from stopping and surfaces `reason` to it as a new
/// prompt. We use that one round-trip to have the agent — which still holds the
/// full session context — write a concise, genuine "what & why" into the trace.
fn emit_summary_request() {
    let reason = "Before you finish: record a short reasoning trace for this \
        repository's memory. Run `walgit trace set --decision \"<what you \
        changed and WHY, 2-3 sentences>\"` (add one or more `--alternative \
        \"<option you rejected and why>\"` if there were real forks). Keep the \
        decision under ~300 characters — summarise the intent, don't relist \
        every step.";
    let control = serde_json::json!({
        "decision": "block",
        "reason": reason,
    });
    // A single line of JSON on stdout is the hook's control channel.
    println!("{}", control);
}

/// Write the compact, MemWal-bound session record and clear the pending trace.
///
/// The snapshot is keyed by the session's last commit SHA so the existing
/// push-time uploader (which enumerates pushed commits) ships it unchanged. We
/// drop `tools_called` here — only the derived `files` list survives — so
/// neither the local snapshot nor MemWal carries the bulky per-call log.
pub(super) fn finalize_session(git_dir: &Path, pt: &mut PendingTrace) -> Result<()> {
    let Some(sha) = pt.commits.last().cloned() else {
        return Ok(()); // defensive: callers only finalize committed sessions
    };

    // Fill the task from the commit message if the agent never set one.
    if pt.task.trim().is_empty() {
        if let Some(hint) = super::memwal::derive_task_hint(pt) {
            pt.task = hint;
        }
    }
    // Derive the durable file list, then drop the bulky tool log.
    pt.files = super::memwal::files_modified(pt);
    pt.tools_called.clear();

    // Betterleaks gate before anything reaches disk. Same policy the old
    // post-commit snapshot used: this hook is non-interactive, so we warn and
    // refuse to save rather than blocking. Push-time scanning runs again.
    if !crate::betterleaks::is_skipped() {
        let text = super::memwal::format_for_memwal(&sha, pt);
        match crate::betterleaks::scan_text(&text) {
            crate::betterleaks::ScanOutcome::SecretsFound { output } => {
                eprintln!(
                    "  ! betterleaks: secrets detected in trace for {} — snapshot NOT saved",
                    sha
                );
                for line in output.lines() {
                    eprintln!("  {}", line);
                }
                // Discard so a secret never lingers in the pending file either.
                trace_pending::delete(git_dir)?;
                return Ok(());
            }
            crate::betterleaks::ScanOutcome::Unavailable => {
                eprintln!(
                    "  ! betterleaks not installed — trace saved without secret scan \
                     (re-checked at push time)"
                );
            }
            crate::betterleaks::ScanOutcome::Clean => {}
        }
    }

    trace_pending::save_snapshot(git_dir, &sha, pt)?;
    trace_pending::delete(git_dir)?;
    Ok(())
}

pub(super) fn apply_claude_event(pt: &mut PendingTrace, event: ClaudeEvent, payload: &Value) {
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
            // Unreachable in practice: `record` intercepts Stop and routes it
            // to `handle_stop`. Kept only for match exhaustiveness.
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

    // ─── Stop-hook decision logic ────────────────────────────────────────

    /// Build a pending trace and optionally give it an edit and/or a commit,
    /// so each `decide_stop` case is one readable line at the call site.
    fn pt_with(edited: bool, committed: bool, decision: &str) -> PendingTrace {
        let mut pt = PendingTrace::new("claude-code".into(), "run-x".into(), None);
        if edited {
            pt.push_tool(ToolCall {
                name: "Edit".into(),
                input_summary: "src/lib.rs".into(),
                output_summary: "ok".into(),
            });
        }
        if committed {
            pt.mark_commit("a".repeat(40));
        }
        pt.decision = decision.into();
        pt
    }

    #[test]
    fn decide_stop_read_only_is_discard() {
        // Only a Read tool call, no commit → nothing worth remembering.
        let mut pt = PendingTrace::new("a".into(), "r".into(), None);
        pt.push_tool(ToolCall {
            name: "Read".into(),
            input_summary: "src/lib.rs".into(),
            output_summary: "10 lines".into(),
        });
        assert_eq!(decide_stop(&pt, false), StopOutcome::Discard);
    }

    #[test]
    fn decide_stop_edited_but_uncommitted_keeps_pending() {
        let pt = pt_with(true, false, "");
        assert_eq!(decide_stop(&pt, false), StopOutcome::KeepPending);
    }

    #[test]
    fn decide_stop_committed_without_decision_requests_summary() {
        let pt = pt_with(true, true, "");
        assert_eq!(decide_stop(&pt, false), StopOutcome::RequestSummary);
    }

    #[test]
    fn decide_stop_committed_with_decision_finalizes() {
        let pt = pt_with(true, true, "did the thing because reasons");
        assert_eq!(decide_stop(&pt, false), StopOutcome::Finalize);
    }

    #[test]
    fn decide_stop_loop_guard_finalizes_even_without_decision() {
        // stop_hook_active=true means we already asked once. Finalize rather
        // than asking forever, even though the agent left decision empty.
        let pt = pt_with(true, true, "");
        assert_eq!(decide_stop(&pt, true), StopOutcome::Finalize);
    }

    #[test]
    fn decide_stop_commit_without_edit_still_counts_as_changed() {
        // A commit with no recorded Edit (e.g. agent committed via Bash `git
        // commit`) is still a real change worth remembering.
        let pt = pt_with(false, true, "");
        assert_eq!(decide_stop(&pt, false), StopOutcome::RequestSummary);
    }

    // ─── finalize_session integration ────────────────────────────────────

    #[test]
    fn finalize_session_writes_compact_snapshot_and_clears_pending() {
        crate::betterleaks::set_skip(true); // no scanning in the unit test
        let td = tempfile::TempDir::new().unwrap();
        let git_dir = td.path();

        let mut pt = PendingTrace::new("claude-code".into(), "run-1".into(), None);
        pt.decision = "tightened the check because tokens leaked".into();
        pt.mark_commit("a".repeat(40));
        pt.mark_commit("b".repeat(40));
        // Two file touches (one duplicated by basename) + a Read.
        for (name, path) in [
            ("Edit", "/abs/src/auth.rs"),
            ("Write", "auth.rs"),
            ("Read", "/abs/src/other.rs"),
        ] {
            pt.push_tool(ToolCall {
                name: name.into(),
                input_summary: path.into(),
                output_summary: "ok".into(),
            });
        }
        trace_pending::save(git_dir, &pt).unwrap();

        finalize_session(git_dir, &mut pt).unwrap();

        // Pending is cleared.
        assert!(trace_pending::load(git_dir).unwrap().is_none());

        // Exactly one snapshot, keyed by the LAST commit.
        let snaps = trace_pending::list_snapshots(git_dir).unwrap();
        assert_eq!(snaps.len(), 1);
        let (sha, path) = &snaps[0];
        assert_eq!(sha, &"b".repeat(40));

        let saved = trace_pending::load_snapshot(path).unwrap();
        // Bulky tool log dropped...
        assert!(saved.tools_called.is_empty());
        // ...but the derived, deduped basenames survive.
        assert_eq!(saved.files, vec!["auth.rs", "other.rs"]);
        assert_eq!(saved.commits.len(), 2);
        assert_eq!(saved.decision, "tightened the check because tokens leaked");
    }

    #[test]
    fn finalize_session_fills_task_from_commit_message() {
        crate::betterleaks::set_skip(true);
        let td = tempfile::TempDir::new().unwrap();
        let git_dir = td.path();

        let mut pt = PendingTrace::new("claude-code".into(), "run-2".into(), None);
        pt.mark_commit("c".repeat(40));
        // Task is empty; a Bash `git commit -m` should seed it.
        pt.push_tool(ToolCall {
            name: "Bash".into(),
            input_summary: "git commit -m \"add rate limiter\"".into(),
            output_summary: "ok".into(),
        });
        assert!(pt.task.is_empty());

        finalize_session(git_dir, &mut pt).unwrap();

        let snaps = trace_pending::list_snapshots(git_dir).unwrap();
        let saved = trace_pending::load_snapshot(&snaps[0].1).unwrap();
        assert_eq!(saved.task, "add rate limiter");
    }
}
