// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

use crate::error::{Result, WalGitError};
use crate::{git, trace_pending};
use serde_json::Value;
use std::io::Read;
use std::path::PathBuf;

pub(super) fn current_git_dir() -> Result<PathBuf> {
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
pub(super) fn resolve_git_dir_or_skip(gated: bool) -> Result<Option<PathBuf>> {
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

pub(super) fn current_repo_root() -> Result<PathBuf> {
    let cwd = std::env::current_dir()?;
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

pub(super) fn generate_run_id() -> String {
    use rand::RngCore;
    let mut buf = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut buf);
    format!("{}-{}", chrono::Utc::now().timestamp(), hex::encode(buf))
}

/// Read stdin to a JSON value. Returns `Value::Null` (rather than erroring)
/// if stdin is empty or not JSON — Claude Code hooks must never block the
/// agent on a malformed payload from us.
pub(super) fn read_stdin_json_silent() -> Result<Value> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    if buf.trim().is_empty() {
        return Ok(Value::Null);
    }
    Ok(serde_json::from_str(&buf).unwrap_or(Value::Null))
}
