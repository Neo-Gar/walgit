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
/// 2. **Changed, no decision yet, not already asked** →
///    [`RequestSummary`](StopOutcome::RequestSummary). We ask as soon as the
///    session has edits, *without waiting for a commit* — in a typical "vibe
///    coding" flow the user commits and pushes by hand, so the agent's turn
///    ends before any commit exists. Asking here is the only moment we're still
///    inside the agent loop and can have it write a real "why" hands-free.
/// 3. **Changed but not committed yet** (decision captured, or we already
///    asked) → [`KeepPending`](StopOutcome::KeepPending); there's no SHA to key
///    a snapshot on, so the decision waits in the pending file until a commit
///    lands and push-time finalize ships it.
/// 4. **Committed** → [`Finalize`](StopOutcome::Finalize).
///
/// The `stop_active` flag is the loop guard: Claude sets it on the Stop that
/// fires right after a blocked Stop, so we ask at most once per stop-chain and
/// never trap the agent in an ask-forever loop.
pub(super) fn decide_stop(pt: &PendingTrace, stop_active: bool) -> StopOutcome {
    if !pt.has_changes() {
        return StopOutcome::Discard;
    }
    if pt.decision.trim().is_empty() && !stop_active {
        return StopOutcome::RequestSummary;
    }
    if pt.commits.is_empty() {
        return StopOutcome::KeepPending;
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

/// Finalize a still-pending session at push time, so a trace is never lost when
/// the agent's `Stop` hook didn't get to run a clean finalize — e.g. the user
/// committed and pushed by hand, or commits were made by a GUI/IDE client whose
/// minimal PATH hid `walgit` from the post-commit hook.
///
/// `pushed_shas` are the commits this push is making canonical (newest first).
/// If the pending trace recorded no commits of its own (hook never fired), we
/// adopt the pushed commits so the session can be keyed and uploaded. The
/// `decision` may be empty here — by design we'd rather ship a trace with
/// task + files + commits than drop it. Returns the finalized snapshot SHA, or
/// `None` when there's nothing to finalize (no pending trace, or read-only).
pub(super) fn finalize_pending_for_push(
    git_dir: &Path,
    pushed_shas: &[String],
) -> Result<Option<String>> {
    let Some(mut pt) = trace_pending::load(git_dir)? else {
        return Ok(None); // no in-progress session
    };

    // Only adopt the pushed commits when the session actually edited something
    // but the post-commit hook never recorded a SHA. A read-only session must
    // NOT claim the user's commits — that would fabricate a memory for work the
    // agent didn't do.
    if pt.commits.is_empty() {
        let edited = pt
            .tools_called
            .iter()
            .any(|t| matches!(t.name.as_str(), "Write" | "Edit" | "MultiEdit" | "NotebookEdit"));
        if !edited {
            return Ok(None);
        }
        for sha in pushed_shas.iter().rev() {
            pt.mark_commit(sha.clone());
        }
    }

    // Still nothing to key on (e.g. push carried no commits) → leave pending.
    if pt.commits.is_empty() {
        return Ok(None);
    }

    let sha = pt.commits.last().cloned();
    finalize_session(git_dir, &mut pt)?;
    Ok(sha)
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
            // First *real* user prompt of a session is a strong signal for the
            // task field; later prompts shouldn't clobber it.
            if pt.task.trim().is_empty() {
                if let Some(p) = payload.get("prompt").and_then(Value::as_str) {
                    if let Some(task) = clean_prompt_for_task(p) {
                        pt.task = truncate(&task, 200);
                    }
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

/// Extract the real user request from a Claude Code prompt for use as the
/// `task`. Claude often injects context wrappers — `<ide_opened_file>`,
/// `<ide_selection>`, `<system-reminder>`, etc. — ahead of (or instead of) the
/// user's text. Taking the literal first line then captures noise like
/// `<ide_opened_file>The user opened …`, which is what showed up as a garbage
/// task in the field. We drop any line that is wholly inside such an injected
/// tag and return the first genuine line of prose. Returns `None` when the
/// prompt is *only* injected context (no real ask yet) so a later prompt can
/// fill the task instead.
fn clean_prompt_for_task(prompt: &str) -> Option<String> {
    let mut in_block = false; // inside a multi-line injected tag block
    for raw in prompt.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if in_block {
            // Wait for the block to close, then keep scanning for real prose.
            if line.starts_with("</") || line.ends_with("/>") {
                in_block = false;
            }
            continue;
        }
        if injected_tag_line(line) {
            // Self-contained `<tag>…</tag>` (or `<tag/>`) → skip just this line.
            // A bare opening `<tag>` with no close → skip the whole block.
            if !(line.contains("</") || line.ends_with("/>")) {
                in_block = true;
            }
            continue;
        }
        // First line that isn't injected context — this is the real request.
        return Some(line.to_string());
    }
    None
}

/// True if `line` begins with an injected context tag we should ignore — the
/// wrappers Claude Code prepends (`<ide_opened_file>`, `<ide_selection>`,
/// `<system-reminder>`, `<command-name>`, …): a `<` followed by a lowercase tag
/// name of `[a-z0-9_-]`, then `>`, space, or `/`. Real prose rarely starts that
/// way, and never with these tag names.
fn injected_tag_line(line: &str) -> bool {
    let Some(rest) = line.strip_prefix('<') else {
        return false;
    };
    if rest.starts_with('/') {
        return false;
    }
    let name: String = rest
        .chars()
        .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_' || *c == '-')
        .collect();
    !name.is_empty()
        && rest[name.len()..]
            .chars()
            .next()
            .is_some_and(|c| c == '>' || c == ' ' || c == '/')
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

    #[test]
    fn user_prompt_skips_injected_ide_context() {
        // The field bug: the first "prompt" is a Claude-injected IDE context
        // tag, not the user's words. Task must not become "<ide_opened_file>…".
        let mut pt = PendingTrace::new("a".into(), "r".into(), None);
        let injected = "<ide_opened_file>The user opened /src/Calculator.tsx in the IDE.</ide_opened_file>";
        apply_claude_event(&mut pt, ClaudeEvent::UserPrompt, &json!({"prompt": injected}));
        // Pure injected context → task stays empty, ready for the real prompt.
        assert_eq!(pt.task, "");
        apply_claude_event(
            &mut pt,
            ClaudeEvent::UserPrompt,
            &json!({"prompt": "add a calculator to the home page"}),
        );
        assert_eq!(pt.task, "add a calculator to the home page");
    }

    #[test]
    fn clean_prompt_strips_leading_injected_block_keeps_prose() {
        // Multi-line injected block followed by the real ask on a later line.
        let p = "<ide_selection>\n  lines 1-5 of foo.rs\n</ide_selection>\nrefactor the parser";
        assert_eq!(clean_prompt_for_task(p).as_deref(), Some("refactor the parser"));
    }

    #[test]
    fn clean_prompt_single_line_injected_then_prose() {
        let p = "<system-reminder>be concise</system-reminder>\nship the fix";
        assert_eq!(clean_prompt_for_task(p).as_deref(), Some("ship the fix"));
    }

    #[test]
    fn clean_prompt_plain_text_unchanged() {
        assert_eq!(
            clean_prompt_for_task("just do the thing").as_deref(),
            Some("just do the thing")
        );
    }

    #[test]
    fn clean_prompt_only_injected_returns_none() {
        let p = "<ide_opened_file>opened x.rs</ide_opened_file>";
        assert_eq!(clean_prompt_for_task(p), None);
    }

    #[test]
    fn clean_prompt_does_not_eat_real_angle_bracket_prose() {
        // A real ask that happens to use `<` (e.g. code/comparison) is not an
        // injected tag and must be preserved.
        assert_eq!(
            clean_prompt_for_task("make sure a < b in the guard").as_deref(),
            Some("make sure a < b in the guard")
        );
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
    fn decide_stop_edited_uncommitted_requests_summary() {
        // The core fix: ask for the "why" as soon as there are edits, even
        // before any commit exists — the common vibe-coding flow where the user
        // commits/pushes by hand after the agent's turn ends.
        let pt = pt_with(true, false, "");
        assert_eq!(decide_stop(&pt, false), StopOutcome::RequestSummary);
    }

    #[test]
    fn decide_stop_edited_uncommitted_with_decision_keeps_pending() {
        // Decision already captured but no commit yet → hold the trace until a
        // commit lands (push-time finalize ships it). Don't ask again.
        let pt = pt_with(true, false, "did the thing because reasons");
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
    fn decide_stop_loop_guard_keeps_pending_when_uncommitted() {
        // Already asked (stop_active) but no commit and no decision → don't ask
        // again, just hold pending for the eventual commit.
        let pt = pt_with(true, false, "");
        assert_eq!(decide_stop(&pt, true), StopOutcome::KeepPending);
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

    // ─── finalize_pending_for_push (push-time safety net) ─────────────────

    #[test]
    fn push_finalize_adopts_pushed_commits_when_hook_never_recorded() {
        // The exact failure we hit in the field: edits were made, but the
        // post-commit hook never recorded a SHA (walgit not on the hook's PATH),
        // so pending.commits is empty. On push we adopt the pushed commits.
        crate::betterleaks::set_skip(true);
        let td = tempfile::TempDir::new().unwrap();
        let git_dir = td.path();

        let mut pt = PendingTrace::new("claude-code".into(), "run-1".into(), None);
        pt.push_tool(ToolCall {
            name: "Edit".into(),
            input_summary: "src/App.tsx".into(),
            output_summary: "ok".into(),
        });
        assert!(pt.commits.is_empty());
        trace_pending::save(git_dir, &pt).unwrap();

        // `git rev-list` order is newest-first (`[newest, …, oldest]`). We store
        // commits oldest-first and key the session on the newest one, matching
        // the post-commit hook path.
        let newest = "b".repeat(40);
        let oldest = "a".repeat(40);
        let pushed = vec![newest.clone(), oldest.clone()];
        let keyed = finalize_pending_for_push(git_dir, &pushed).unwrap();

        assert_eq!(keyed.as_deref(), Some(newest.as_str()));
        assert!(trace_pending::load(git_dir).unwrap().is_none()); // pending cleared
        let snaps = trace_pending::list_snapshots(git_dir).unwrap();
        assert_eq!(snaps.len(), 1);
        let saved = trace_pending::load_snapshot(&snaps[0].1).unwrap();
        assert_eq!(saved.files, vec!["App.tsx"]);
        assert_eq!(saved.commits, vec![oldest, newest]); // oldest-first
        assert!(saved.decision.is_empty()); // shipped without a summary, by design
    }

    #[test]
    fn push_finalize_keeps_own_commits_over_pushed() {
        // If the hook DID record commits, keep them — don't clobber with the
        // push set (which could include unrelated older commits).
        crate::betterleaks::set_skip(true);
        let td = tempfile::TempDir::new().unwrap();
        let git_dir = td.path();

        let mut pt = PendingTrace::new("claude-code".into(), "run-1".into(), None);
        pt.push_tool(ToolCall {
            name: "Write".into(),
            input_summary: "x.rs".into(),
            output_summary: "ok".into(),
        });
        pt.mark_commit("c".repeat(40));
        trace_pending::save(git_dir, &pt).unwrap();

        let pushed = vec!["b".repeat(40), "a".repeat(40)];
        let keyed = finalize_pending_for_push(git_dir, &pushed).unwrap();

        assert_eq!(keyed.as_deref(), Some("c".repeat(40).as_str()));
        let saved =
            trace_pending::load_snapshot(&trace_pending::list_snapshots(git_dir).unwrap()[0].1)
                .unwrap();
        assert_eq!(saved.commits, vec!["c".repeat(40)]);
    }

    #[test]
    fn push_finalize_noop_when_no_pending() {
        let td = tempfile::TempDir::new().unwrap();
        let keyed = finalize_pending_for_push(td.path(), &["a".repeat(40)]).unwrap();
        assert!(keyed.is_none());
    }

    #[test]
    fn push_finalize_noop_for_readonly_session() {
        // Read-only pending (no edits) + commits being pushed that aren't the
        // agent's work → don't fabricate a memory.
        let td = tempfile::TempDir::new().unwrap();
        let git_dir = td.path();
        let mut pt = PendingTrace::new("claude-code".into(), "run-1".into(), None);
        pt.push_tool(ToolCall {
            name: "Read".into(),
            input_summary: "src/lib.rs".into(),
            output_summary: "10 lines".into(),
        });
        trace_pending::save(git_dir, &pt).unwrap();

        // No commits of its own; has_changes() is false → nothing to store even
        // though commits are being pushed (they're the user's, not the agent's).
        let keyed = finalize_pending_for_push(git_dir, &["a".repeat(40)]).unwrap();
        assert!(keyed.is_none());
        // Pending is left intact (not deleted) — it wasn't ours to finalize.
        assert!(trace_pending::load(git_dir).unwrap().is_some());
    }
}
