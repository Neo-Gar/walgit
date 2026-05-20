// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

//! Installation and templating of trace-recording hooks.
//!
//! Two kinds of hooks are managed here:
//!
//! 1. **Git hooks** in `<git-dir>/hooks/`. The `prepare-commit-msg` hook calls
//!    `walgit trace flush` so every git commit in the repo picks up a pending
//!    trace footer if one exists. We chain into any pre-existing hook script
//!    so users with their own `prepare-commit-msg` keep working.
//!
//! 2. **Per-agent runtime hooks** — currently Claude Code's
//!    `.claude/settings.json`. The installer merges our entries into the
//!    user's existing settings without touching anything else.
//!
//! Identity. Every walgit-managed entry is marked with a sentinel
//! (`# walgit-trace` in shell, `"_walgit": true` in JSON) so `install` is
//! idempotent and `uninstall` removes only our additions.

use crate::error::{Result, WalGitError};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

/// Sentinel marker stamped into every walgit-managed hook so we can find and
/// remove our own entries without disturbing user-authored ones.
pub const SHELL_SENTINEL: &str = "# walgit-trace v1 — managed by `walgit trace install`. Do not edit between BEGIN/END.";
pub const SHELL_BEGIN: &str = "# >>> walgit-trace BEGIN >>>";
pub const SHELL_END: &str = "# <<< walgit-trace END <<<";

/// Sentinel string embedded in the `command` field of each managed Claude Code
/// hook entry. The user's own entries never contain this substring, so we can
/// identify ours unambiguously when re-installing or uninstalling.
pub const CLAUDE_HOOK_TAG: &str = "walgit-trace-managed";

// ─── Git hooks ──────────────────────────────────────────────────────────────

/// Body of the `prepare-commit-msg` hook. It is intentionally tiny: all real
/// work lives in `walgit trace flush`, which is responsible for being a no-op
/// when there is no pending trace (so plain `git commit` keeps working).
///
/// The hook receives the message file path as `$1`. We pass it through and
/// ignore $2/$3 (source/sha) — flushing semantics don't depend on those.
fn prepare_commit_msg_body() -> String {
    format!(
        r#"{begin}
{sentinel}
# Appends the pending reasoning trace (if any) into the commit message.
# Safe to keep enabled — exits 0 silently when there's no pending trace.
if command -v walgit >/dev/null 2>&1; then
    walgit trace flush --message-file "$1" || exit $?
fi
{end}
"#,
        begin = SHELL_BEGIN,
        sentinel = SHELL_SENTINEL,
        end = SHELL_END,
    )
}

/// Install (or re-install) walgit's `prepare-commit-msg` block into the git
/// hook for `git_dir`. If a hook script already exists with non-walgit
/// content, we preserve it and inject our managed block at the end so both
/// run. Idempotent.
pub fn install_git_hook(git_dir: &Path) -> Result<PathBuf> {
    let hooks_dir = git_dir.join("hooks");
    std::fs::create_dir_all(&hooks_dir)?;
    let hook = hooks_dir.join("prepare-commit-msg");

    let existing = if hook.exists() {
        std::fs::read_to_string(&hook)?
    } else {
        String::new()
    };

    let cleaned = strip_managed_block(&existing);
    let mut combined = if cleaned.trim().is_empty() {
        "#!/usr/bin/env sh\n".to_string()
    } else if cleaned.starts_with("#!") {
        cleaned
    } else {
        // Pre-existing hook without a shebang — extremely rare, but inject one
        // so the chained script is executable.
        format!("#!/usr/bin/env sh\n{}", cleaned)
    };
    if !combined.ends_with('\n') {
        combined.push('\n');
    }
    combined.push_str(&prepare_commit_msg_body());

    std::fs::write(&hook, &combined)?;
    make_executable(&hook)?;
    Ok(hook)
}

/// Remove walgit's block from `prepare-commit-msg`. If the result is just a
/// bare shebang (or empty), delete the file entirely so we don't leave a
/// useless stub behind. Idempotent.
pub fn uninstall_git_hook(git_dir: &Path) -> Result<bool> {
    let hook = git_dir.join("hooks").join("prepare-commit-msg");
    if !hook.exists() {
        return Ok(false);
    }
    let existing = std::fs::read_to_string(&hook)?;
    let cleaned = strip_managed_block(&existing);
    let trimmed_is_empty = cleaned
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with("#!"))
        .next()
        .is_none();
    if trimmed_is_empty {
        std::fs::remove_file(&hook)?;
    } else {
        std::fs::write(&hook, cleaned)?;
    }
    Ok(true)
}

/// Remove the BEGIN/END block (and any trailing blank line) from a shell
/// script body. Leaves everything outside the markers untouched.
fn strip_managed_block(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut in_block = false;
    for line in body.lines() {
        if line.trim() == SHELL_BEGIN {
            in_block = true;
            continue;
        }
        if line.trim() == SHELL_END {
            in_block = false;
            continue;
        }
        if !in_block {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

#[cfg(unix)]
fn make_executable(p: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(p)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(p, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_p: &Path) -> Result<()> {
    // Windows: git looks at the file content, not the executable bit.
    Ok(())
}

// ─── Claude Code adapter ────────────────────────────────────────────────────

/// Build the Claude Code settings.json fragment we manage. Returns a `Value`
/// with only the `hooks` we own — caller is responsible for merging this into
/// the user's settings via [`merge_claude_settings`].
///
/// `gated` controls whether each command carries the `--only-if-enabled`
/// flag. We want `true` for the user-global settings (one global install
/// must not record traces for every repo the user opens — only opted-in
/// ones, marked by `<git-dir>/walgit/enabled`) and `false` for the
/// project-local form (the project itself is the opt-in signal there).
fn claude_managed_hooks(gated: bool) -> Value {
    let tag = CLAUDE_HOOK_TAG;
    let gate = if gated { " --only-if-enabled" } else { "" };
    json!({
        "SessionStart": [{
            "hooks": [{
                "type": "command",
                "command": format!("walgit trace start --from-claude-hook --tag {}{} || true", tag, gate),
            }]
        }],
        "UserPromptSubmit": [{
            "hooks": [{
                "type": "command",
                "command": format!("walgit trace record --from-claude-hook --event user-prompt --tag {}{} || true", tag, gate),
            }]
        }],
        "PostToolUse": [{
            "matcher": ".*",
            "hooks": [{
                "type": "command",
                "command": format!("walgit trace record --from-claude-hook --event post-tool-use --tag {}{} || true", tag, gate),
            }]
        }],
        "Stop": [{
            "hooks": [{
                "type": "command",
                "command": format!("walgit trace record --from-claude-hook --event stop --tag {}{} || true", tag, gate),
            }]
        }]
    })
}

/// Merge our managed hook entries into `existing` settings, returning the
/// updated JSON. Strategy:
///
/// - Walk every `hooks.<event>` array.
/// - Drop any entry whose any `hooks[*].command` contains [`CLAUDE_HOOK_TAG`]
///   (those are ours from a previous install).
/// - Append our fresh entries.
/// - Leave every other key untouched.
pub fn merge_claude_settings(existing: Value, gated: bool) -> Value {
    let mut root = match existing {
        Value::Object(m) => m,
        _ => Map::new(),
    };

    let mut hooks = match root.remove("hooks") {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    };

    let managed = claude_managed_hooks(gated);
    let managed_obj = managed.as_object().expect("managed hooks are an object");

    for (event, our_entries) in managed_obj {
        let user_entries: Vec<Value> = match hooks.remove(event) {
            Some(Value::Array(items)) => items
                .into_iter()
                .filter(|item| !entry_is_managed(item))
                .collect(),
            _ => Vec::new(),
        };
        let mut combined = user_entries;
        if let Some(arr) = our_entries.as_array() {
            combined.extend(arr.iter().cloned());
        }
        if !combined.is_empty() {
            hooks.insert(event.clone(), Value::Array(combined));
        }
    }

    if !hooks.is_empty() {
        root.insert("hooks".into(), Value::Object(hooks));
    }
    Value::Object(root)
}

/// Strip our managed entries from existing Claude settings, returning the
/// updated JSON. Inverse of [`merge_claude_settings`] minus the re-insertion
/// step.
pub fn unmerge_claude_settings(existing: Value) -> Value {
    let mut root = match existing {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    let mut hooks = match root.remove("hooks") {
        Some(Value::Object(m)) => m,
        _ => return Value::Object(root),
    };
    let event_names: Vec<String> = hooks.keys().cloned().collect();
    for event in event_names {
        let Some(Value::Array(items)) = hooks.remove(&event) else {
            continue;
        };
        let kept: Vec<Value> = items.into_iter().filter(|i| !entry_is_managed(i)).collect();
        if !kept.is_empty() {
            hooks.insert(event, Value::Array(kept));
        }
    }
    if !hooks.is_empty() {
        root.insert("hooks".into(), Value::Object(hooks));
    }
    Value::Object(root)
}

/// True if any of the `hooks[*].command` strings inside this entry contains
/// our tag. Used to recognise entries we wrote previously.
fn entry_is_managed(entry: &Value) -> bool {
    let Some(arr) = entry.get("hooks").and_then(Value::as_array) else {
        return false;
    };
    arr.iter().any(|h| {
        h.get("command")
            .and_then(Value::as_str)
            .map(|c| c.contains(CLAUDE_HOOK_TAG))
            .unwrap_or(false)
    })
}

/// Install or refresh the Claude Code hook block in `settings_path`. The file
/// is created if missing; otherwise we read, merge, and write back.
///
/// `gated` indicates whether commands should include `--only-if-enabled` —
/// pass `true` for the user-global location, `false` for project-local.
pub fn install_claude_settings(settings_path: &Path, gated: bool) -> Result<()> {
    let existing = read_json_or_empty(settings_path)?;
    let merged = merge_claude_settings(existing, gated);
    write_json_pretty(settings_path, &merged)
}

/// True if a Claude Code settings.json at `settings_path` already contains a
/// walgit-managed hook entry. Used by the install picker to surface an
/// "already installed" hint without consulting any external state.
pub fn is_claude_installed(settings_path: &Path) -> bool {
    if !settings_path.exists() {
        return false;
    }
    let Ok(raw) = std::fs::read_to_string(settings_path) else {
        return false;
    };
    raw.contains(CLAUDE_HOOK_TAG)
}

pub fn uninstall_claude_settings(settings_path: &Path) -> Result<bool> {
    if !settings_path.exists() {
        return Ok(false);
    }
    let existing = read_json_or_empty(settings_path)?;
    let cleaned = unmerge_claude_settings(existing);
    // If the file would be left with `{}`, remove it — but only if we created
    // it (heuristic: if there's nothing besides our removed hooks, it was
    // ours). Safer to just write `{}` back; user can rm if they want.
    write_json_pretty(settings_path, &cleaned)?;
    Ok(true)
}

fn read_json_or_empty(p: &Path) -> Result<Value> {
    if !p.exists() {
        return Ok(json!({}));
    }
    let raw = std::fs::read_to_string(p)?;
    if raw.trim().is_empty() {
        return Ok(json!({}));
    }
    let v: Value = serde_json::from_str(&raw).map_err(|e| {
        WalGitError::other(format!(
            "{} is not valid JSON: {} — refusing to overwrite",
            p.display(),
            e
        ))
    })?;
    Ok(v)
}

fn write_json_pretty(p: &Path, v: &Value) -> Result<()> {
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut s = serde_json::to_string_pretty(v)?;
    s.push('\n');
    std::fs::write(p, s)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn install_git_hook_creates_executable_script() {
        let td = TempDir::new().unwrap();
        let hook = install_git_hook(td.path()).unwrap();
        assert!(hook.exists());
        let body = std::fs::read_to_string(&hook).unwrap();
        assert!(body.contains(SHELL_BEGIN));
        assert!(body.contains(SHELL_END));
        assert!(body.contains("walgit trace flush"));
    }

    #[test]
    fn install_git_hook_preserves_user_script() {
        let td = TempDir::new().unwrap();
        let hooks_dir = td.path().join("hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        let user_body = "#!/bin/sh\necho 'user hook ran'\n";
        let hook = hooks_dir.join("prepare-commit-msg");
        std::fs::write(&hook, user_body).unwrap();

        install_git_hook(td.path()).unwrap();
        let combined = std::fs::read_to_string(&hook).unwrap();
        assert!(combined.contains("user hook ran"));
        assert!(combined.contains("walgit trace flush"));
    }

    #[test]
    fn install_git_hook_is_idempotent() {
        let td = TempDir::new().unwrap();
        install_git_hook(td.path()).unwrap();
        install_git_hook(td.path()).unwrap();
        let body = std::fs::read_to_string(td.path().join("hooks/prepare-commit-msg")).unwrap();
        // Exactly one BEGIN/END pair regardless of how many times we install.
        assert_eq!(body.matches(SHELL_BEGIN).count(), 1);
        assert_eq!(body.matches(SHELL_END).count(), 1);
    }

    #[test]
    fn uninstall_git_hook_preserves_user_script() {
        let td = TempDir::new().unwrap();
        let hooks_dir = td.path().join("hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        let user_body = "#!/bin/sh\necho 'mine'\n";
        let hook = hooks_dir.join("prepare-commit-msg");
        std::fs::write(&hook, user_body).unwrap();

        install_git_hook(td.path()).unwrap();
        uninstall_git_hook(td.path()).unwrap();

        let final_body = std::fs::read_to_string(&hook).unwrap();
        assert!(final_body.contains("echo 'mine'"));
        assert!(!final_body.contains("walgit trace"));
    }

    #[test]
    fn uninstall_git_hook_removes_solo_walgit_script() {
        let td = TempDir::new().unwrap();
        install_git_hook(td.path()).unwrap();
        uninstall_git_hook(td.path()).unwrap();
        // No user content → file should be gone, not left as `#!/usr/bin/env sh` stub.
        assert!(!td.path().join("hooks/prepare-commit-msg").exists());
    }

    #[test]
    fn merge_claude_settings_into_empty() {
        let merged = merge_claude_settings(json!({}), true);
        let hooks = &merged["hooks"];
        assert!(hooks.get("PostToolUse").is_some());
        assert!(hooks.get("Stop").is_some());
        assert!(hooks["PostToolUse"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains(CLAUDE_HOOK_TAG));
    }

    #[test]
    fn merge_claude_settings_preserves_user_hooks() {
        let user = json!({
            "permissions": {"allow": ["Bash(ls)"]},
            "hooks": {
                "PreToolUse": [{
                    "matcher": "Bash",
                    "hooks": [{"type": "command", "command": "echo user-pre"}]
                }],
                "Stop": [{
                    "hooks": [{"type": "command", "command": "echo user-stop"}]
                }]
            }
        });
        let merged = merge_claude_settings(user, true);

        // User's permissions block untouched.
        assert_eq!(merged["permissions"]["allow"][0], "Bash(ls)");

        // User's PreToolUse entry preserved (we never touch PreToolUse).
        assert_eq!(
            merged["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            "echo user-pre"
        );

        // User's Stop entry preserved AND ours appended.
        let stop = merged["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2);
        assert_eq!(stop[0]["hooks"][0]["command"], "echo user-stop");
        assert!(stop[1]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains(CLAUDE_HOOK_TAG));
    }

    #[test]
    fn merge_claude_settings_replaces_prior_walgit_entries() {
        let once = merge_claude_settings(json!({}), true);
        let twice = merge_claude_settings(once, true);
        // Each event ends up with exactly one of our entries, not two.
        let stop = twice["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 1);
        let post = twice["hooks"]["PostToolUse"].as_array().unwrap();
        assert_eq!(post.len(), 1);
    }

    #[test]
    fn unmerge_claude_settings_removes_only_ours() {
        let user_with_ours = merge_claude_settings(json!({
            "hooks": {
                "Stop": [{"hooks": [{"type": "command", "command": "echo user-stop"}]}]
            }
        }), true);
        let after = unmerge_claude_settings(user_with_ours);
        let stop = after["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 1);
        assert_eq!(stop[0]["hooks"][0]["command"], "echo user-stop");
        // Our PostToolUse entry should be entirely gone (no user content).
        assert!(after["hooks"].get("PostToolUse").is_none());
    }
}
