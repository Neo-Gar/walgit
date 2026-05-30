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

/// Handle Claude Code's `Stop` hook — the agent has finished its turn.
///
/// This is the moment a session crystallises into a stored memory. The states:
///
/// 1. **Read-only** (no edits, no commits) → discard the pending trace and
///    write nothing. Keeps MemWal free of "agent just looked around" entries.
/// 2. **Changed but not committed yet** → keep the pending trace alive so the
///    work folds into the eventual commit; there's no SHA to key a memory on.
/// 3. **Committed, no decision yet, first Stop** → ask the running agent to
///    summarise by emitting a `decision: block` control object. Claude feeds
///    the `reason` back as a prompt and the agent calls `walgit trace set
///    --decision …`, after which it stops again.
/// 4. **Committed, decision present OR we already asked (`stop_hook_active`)**
///    → finalize: write one compact session snapshot and clear the pending
///    trace. The `stop_hook_active` guard guarantees we never loop forever even
///    if the agent ignores the request.
fn handle_stop(git_dir: &Path, mut pt: PendingTrace, payload: &Value) -> Result<()> {
    if !pt.has_changes() {
        trace_pending::delete(git_dir)?;
        return Ok(());
    }
    if pt.commits.is_empty() {
        // Edited but not committed — persist as-is and wait for the commit.
        trace_pending::save(git_dir, &pt)?;
        return Ok(());
    }

    let stop_active = payload
        .get("stop_hook_active")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if pt.decision.trim().is_empty() && !stop_active {
        // Ask the agent to write its own summary; leave pending untouched so
        // the agent's `trace set --decision` fills it before the next Stop.
        emit_summary_request();
        return Ok(());
    }

    finalize_session(git_dir, &mut pt)
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
    use crate::trace_pending::PendingTrace;
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
}
