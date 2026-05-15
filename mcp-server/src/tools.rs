// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

//! Tool registry + dispatcher. Every tool here is a thin shim that builds an
//! argv for the `walgit` binary, runs it, and returns its stdout/stderr to
//! the MCP client. We intentionally do NOT depend on the walgit library —
//! shelling out keeps the contract identical to what a human would type,
//! and the MCP server stays tiny.

use crate::protocol::{Content, ToolCallResult, ToolDescriptor};
use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Path to the `walgit` binary. Resolved once at startup so missing binaries
/// fail fast with a clear message rather than per-tool-call.
pub fn resolve_walgit_binary() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("WALGIT_BIN") {
        let path = PathBuf::from(p);
        if path.exists() {
            return Ok(path);
        }
        return Err(anyhow!(
            "WALGIT_BIN is set to '{}' but no such file exists",
            path.display()
        ));
    }
    // Try `walgit` next to this binary (workspace target/ layout).
    if let Ok(self_exe) = std::env::current_exe() {
        if let Some(dir) = self_exe.parent() {
            let candidate = dir.join("walgit");
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }
    // Fall back to PATH lookup at call time. Return a bare name; tokio will
    // do `execvp`-style resolution against `PATH`.
    Ok(PathBuf::from("walgit"))
}

/// Static tool catalogue. Names use `walgit_` prefix so agents that flatten
/// tool lists across multiple servers don't collide.
pub fn list_tools() -> Vec<ToolDescriptor> {
    vec![
        ToolDescriptor {
            name: "walgit_init",
            description:
                "Create a new WalGit repository on chain and locally. By default a new \
                 directory `<name>/` is created in `cwd`; pass `here=true` to use `cwd` itself.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "cwd": { "type": "string", "description": "Working directory. Default: server cwd." },
                    "name": { "type": "string", "description": "Repository name (alphanumeric, -, _, ., max 64 chars)" },
                    "private": { "type": "boolean", "default": false },
                    "here": { "type": "boolean", "default": false, "description": "Init inside cwd instead of cwd/<name>" }
                },
                "required": ["name"]
            }),
        },
        ToolDescriptor {
            name: "walgit_fork",
            description:
                "Fork another user's public repository. The fork is created on chain and a \
                 local clone is set up next to cwd. Use this when an agent wants its own copy \
                 to make changes and propose back via a PR.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "cwd": { "type": "string" },
                    "url": { "type": "string", "description": "walgit://<owner>/<repo>" }
                },
                "required": ["url"]
            }),
        },
        ToolDescriptor {
            name: "walgit_status",
            description: "Show the current repository status: id, owner, branches.",
            input_schema: json!({
                "type": "object",
                "properties": { "cwd": { "type": "string" } }
            }),
        },
        ToolDescriptor {
            name: "walgit_log",
            description:
                "Show recent commits on the active branch. Pass `traces=true` for an \
                 agent-readable view that surfaces `agent_id` and the `task` line of each \
                 commit's reasoning trace.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "cwd": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "default": 20 },
                    "traces": { "type": "boolean", "default": false }
                }
            }),
        },
        ToolDescriptor {
            name: "walgit_show",
            description:
                "Show a single commit. Pass `trace=true` to also render the embedded \
                 reasoning trace (decision, tools called, alternatives considered).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "cwd": { "type": "string" },
                    "commit": { "type": "string", "default": "HEAD" },
                    "trace": { "type": "boolean", "default": false }
                }
            }),
        },
        ToolDescriptor {
            name: "walgit_agent_commit",
            description:
                "Stage the given paths, commit, and embed a reasoning trace JSON into the \
                 commit message footer. The trace becomes part of the commit SHA — \
                 permanent, tamper-evident provenance. Use this instead of plain `git commit` \
                 whenever an agent makes a change so downstream agents can review *why*.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "cwd": { "type": "string" },
                    "paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Files / directories to stage. Use ['.'] for everything."
                    },
                    "message": {
                        "type": "string",
                        "description": "Short subject line, like a normal git commit message"
                    },
                    "trace": {
                        "type": "object",
                        "description": "Schema v0 reasoning trace. Required fields: version='1', agent_id, run_id, task, decision. See .agents/TRACE_SCHEMA.md."
                    }
                },
                "required": ["paths", "message", "trace"]
            }),
        },
        ToolDescriptor {
            name: "walgit_pr_create",
            description:
                "Open a pull request. When run inside a fork, the PR defaults to going to \
                 the upstream's main branch. The source branch defaults to current HEAD. \
                 The PR's blob is computed incrementally — only the commits new to the \
                 target are uploaded.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "cwd": { "type": "string" },
                    "source_branch": { "type": "string", "description": "Default: current HEAD" },
                    "target_branch": { "type": "string", "description": "Default: target's main / sole branch" }
                }
            }),
        },
        ToolDescriptor {
            name: "walgit_pr_show",
            description:
                "Show full PR metadata: status, flow, author, blob, approved-by, source git \
                 head. Pair with `walgit_pr_diff` to see actual code changes.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "cwd": { "type": "string" },
                    "pr_id": { "type": "string" }
                },
                "required": ["pr_id"]
            }),
        },
        ToolDescriptor {
            name: "walgit_pr_diff",
            description:
                "Render the diff a PR would apply. Auto-clones the target repository into \
                 `~/.walgit/work/<id>/` if needed. Use `stat=true` for just the file summary.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "cwd": { "type": "string" },
                    "pr_id": { "type": "string" },
                    "stat": { "type": "boolean", "default": false }
                },
                "required": ["pr_id"]
            }),
        },
        ToolDescriptor {
            name: "walgit_pr_approve",
            description:
                "Approve a pull request. Requires write access to the target repository \
                 and that the caller is not the PR author (self-approval is forbidden \
                 on chain).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "cwd": { "type": "string" },
                    "pr_id": { "type": "string" }
                },
                "required": ["pr_id"]
            }),
        },
        ToolDescriptor {
            name: "walgit_pr_merge",
            description:
                "Merge an approved PR into the target branch. Auto-clones the target if the \
                 caller isn't already inside it.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "cwd": { "type": "string" },
                    "pr_id": { "type": "string" }
                },
                "required": ["pr_id"]
            }),
        },
        ToolDescriptor {
            name: "walgit_pr_close",
            description:
                "Close a PR without merging. Callable by the PR author or the target \
                 repository owner.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "cwd": { "type": "string" },
                    "pr_id": { "type": "string" }
                },
                "required": ["pr_id"]
            }),
        },
        ToolDescriptor {
            name: "walgit_pr_list",
            description:
                "List pull requests. Default: PRs of the current repository. Pass \
                 `mine=true` to list PRs you authored across all repos on the active \
                 network.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "cwd": { "type": "string" },
                    "mine": { "type": "boolean", "default": false }
                }
            }),
        },
        ToolDescriptor {
            name: "walgit_trace_diff",
            description:
                "Compare the reasoning traces of two commits. Designed for regression \
                 debugging: 'what changed in the agent's reasoning between the good \
                 commit and the bad one?'",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "cwd": { "type": "string" },
                    "sha_a": { "type": "string" },
                    "sha_b": { "type": "string" }
                },
                "required": ["sha_a", "sha_b"]
            }),
        },
    ]
}

/// Run a tool by name with the given JSON arguments. Returns the MCP-style
/// `ToolCallResult` so the dispatcher can surface even tool failures as a
/// (structured) success — agents read `isError` and the text content.
pub async fn dispatch(walgit: &Path, name: &str, args: &Value) -> Result<ToolCallResult> {
    let invocation = build_invocation(name, args)?;
    let output = run_walgit(walgit, &invocation).await?;
    Ok(text_result(output))
}

struct Invocation {
    cwd: Option<String>,
    argv: Vec<String>,
    /// JSON to feed into stdin (used by `agent_commit` to pass the trace).
    stdin_payload: Option<String>,
}

fn build_invocation(name: &str, args: &Value) -> Result<Invocation> {
    let cwd = args.get("cwd").and_then(Value::as_str).map(str::to_string);
    let mut argv: Vec<String> = vec![];
    let mut stdin_payload: Option<String> = None;

    match name {
        "walgit_init" => {
            let name_v = req_str(args, "name")?;
            argv.push("init".into());
            argv.push(name_v.into());
            if get_bool(args, "private", false) {
                argv.push("--private".into());
            }
            if get_bool(args, "here", false) {
                argv.push("--here".into());
            }
        }
        "walgit_fork" => {
            argv.push("fork".into());
            argv.push(req_str(args, "url")?.into());
            argv.push("--yes".into()); // agents always non-interactive
        }
        "walgit_status" => {
            argv.push("status".into());
        }
        "walgit_log" => {
            argv.push("log".into());
            if let Some(limit) = args.get("limit").and_then(Value::as_u64) {
                argv.push("--limit".into());
                argv.push(limit.to_string());
            }
            if get_bool(args, "traces", false) {
                argv.push("--traces".into());
            }
        }
        "walgit_show" => {
            argv.push("show".into());
            let commit = args
                .get("commit")
                .and_then(Value::as_str)
                .unwrap_or("HEAD");
            argv.push(commit.into());
            if get_bool(args, "trace", false) {
                argv.push("--trace".into());
            }
        }
        "walgit_agent_commit" => {
            // Trace is fed via stdin to avoid leaving JSON on disk.
            let trace = args
                .get("trace")
                .ok_or_else(|| anyhow!("missing required parameter 'trace'"))?;
            let trace_json = serde_json::to_string(trace)
                .context("'trace' must be valid JSON")?;
            let paths = args
                .get("paths")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("'paths' must be an array"))?;
            if paths.is_empty() {
                return Err(anyhow!("'paths' must not be empty"));
            }
            argv.push("agent".into());
            argv.push("commit".into());
            for p in paths {
                let s = p
                    .as_str()
                    .ok_or_else(|| anyhow!("each path must be a string"))?;
                argv.push(s.into());
            }
            argv.push("--message".into());
            argv.push(req_str(args, "message")?.into());
            argv.push("--trace".into());
            argv.push("-".into()); // read trace from stdin
            stdin_payload = Some(trace_json);
        }
        "walgit_pr_create" => {
            argv.push("pr".into());
            argv.push("create".into());
            if let Some(s) = args.get("source_branch").and_then(Value::as_str) {
                argv.push("--source-branch".into());
                argv.push(s.into());
            }
            if let Some(t) = args.get("target_branch").and_then(Value::as_str) {
                argv.push("--target-branch".into());
                argv.push(t.into());
            }
            argv.push("--yes".into()); // agents always non-interactive
        }
        "walgit_pr_show" => {
            argv.push("pr".into());
            argv.push("show".into());
            argv.push(req_str(args, "pr_id")?.into());
        }
        "walgit_pr_diff" => {
            argv.push("pr".into());
            argv.push("diff".into());
            argv.push(req_str(args, "pr_id")?.into());
            if get_bool(args, "stat", false) {
                argv.push("--stat".into());
            }
        }
        "walgit_pr_approve" => {
            argv.push("pr".into());
            argv.push("approve".into());
            argv.push(req_str(args, "pr_id")?.into());
        }
        "walgit_pr_merge" => {
            argv.push("pr".into());
            argv.push("merge".into());
            argv.push(req_str(args, "pr_id")?.into());
        }
        "walgit_pr_close" => {
            argv.push("pr".into());
            argv.push("close".into());
            argv.push(req_str(args, "pr_id")?.into());
        }
        "walgit_pr_list" => {
            argv.push("pr".into());
            argv.push("list".into());
            if get_bool(args, "mine", false) {
                argv.push("--mine".into());
            }
        }
        "walgit_trace_diff" => {
            argv.push("trace".into());
            argv.push("diff".into());
            argv.push(req_str(args, "sha_a")?.into());
            argv.push(req_str(args, "sha_b")?.into());
        }
        other => return Err(anyhow!("unknown tool '{}'", other)),
    }

    Ok(Invocation {
        cwd,
        argv,
        stdin_payload,
    })
}

async fn run_walgit(walgit: &Path, inv: &Invocation) -> Result<CapturedOutput> {
    let mut cmd = Command::new(walgit);
    cmd.args(&inv.argv)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Make output deterministic for agent parsing: no spinners, no colors.
        .env("CLICOLOR", "0")
        .env("NO_COLOR", "1")
        .env("TERM", "dumb");
    if let Some(cwd) = &inv.cwd {
        cmd.current_dir(cwd);
    }
    let mut child = cmd.spawn().with_context(|| {
        format!(
            "failed to spawn walgit subprocess at {}",
            walgit.display()
        )
    })?;

    if let Some(payload) = &inv.stdin_payload {
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(payload.as_bytes()).await?;
            stdin.shutdown().await?;
        }
    } else if let Some(stdin) = child.stdin.take() {
        drop(stdin); // close so commands that read stdin don't hang
    }

    let output = child.wait_with_output().await?;
    Ok(CapturedOutput {
        exit_code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

struct CapturedOutput {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

fn text_result(out: CapturedOutput) -> ToolCallResult {
    let is_error = out.exit_code != 0;
    // Compose a single agent-readable text. Agents read sequentially, so
    // stdout first (the "answer") then stderr (which on walgit is the styled
    // status feed) keeps the structure predictable.
    let mut text = String::new();
    if !out.stdout.is_empty() {
        text.push_str(out.stdout.trim_end());
        text.push('\n');
    }
    if !out.stderr.is_empty() {
        // Strip ANSI codes anyway — even with NO_COLOR set, indicatif may emit
        // cursor-control sequences. Keep output paste-safe.
        let stripped = strip_ansi(&out.stderr);
        if !stripped.trim().is_empty() {
            text.push_str(stripped.trim_end());
            text.push('\n');
        }
    }
    if is_error {
        text.push_str(&format!("\n[walgit exited with code {}]\n", out.exit_code));
    }
    ToolCallResult {
        content: vec![Content::Text { text }],
        is_error,
    }
}

/// Drop CSI escape sequences. Hand-rolled to avoid pulling in a strip-ansi
/// dependency; matches `ESC '[' ... <letter>` and `ESC ']' ... BEL`.
fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Drop until the sequence terminator.
            if let Some(&next) = chars.peek() {
                if next == '[' {
                    chars.next();
                    while let Some(c2) = chars.next() {
                        if c2.is_ascii_alphabetic() {
                            break;
                        }
                    }
                    continue;
                } else if next == ']' {
                    chars.next();
                    for c2 in chars.by_ref() {
                        if c2 == '\x07' {
                            break;
                        }
                    }
                    continue;
                }
            }
            // Lone ESC — skip the next char.
            chars.next();
            continue;
        }
        out.push(c);
    }
    out
}

// ─── Small helpers ───────────────────────────────────────────────────────────

fn req_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing required parameter '{}'", key))
}

fn get_bool(args: &Value, key: &str, default: bool) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(default)
}

