// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

//! Git CLI subprocess wrappers.

use crate::error::{Result, WalGitError};
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Output, Stdio};

/// Lowest git that supports `--end-of-options` universally across the
/// subcommands we touch (log, diff, rev-parse). Released 2020-12.
pub const MIN_GIT_VERSION: (u32, u32, u32) = (2, 30, 0);

/// Parse `git --version` and ensure it meets [`MIN_GIT_VERSION`]. Called from
/// preflight so misconfigured machines fail before doing any work.
pub fn check_version() -> Result<(u32, u32, u32)> {
    let out = Command::new("git").arg("--version").output().map_err(|e| {
        match e.kind() {
            std::io::ErrorKind::NotFound => WalGitError::GitNotInstalled,
            _ => WalGitError::git(format!("failed to run git --version: {}", e)),
        }
    })?;
    if !out.status.success() {
        return Err(WalGitError::git("`git --version` exited non-zero"));
    }
    let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let v = parse_git_version(&line).ok_or_else(|| {
        WalGitError::git(format!("could not parse git version from: {}", line))
    })?;
    if v < MIN_GIT_VERSION {
        return Err(WalGitError::git(format!(
            "git {}.{}.{} is too old (need ≥ {}.{}.{}).\n\
             macOS:  brew install git   then ensure `brew --prefix`/bin is on PATH\n\
             Debian: sudo apt-get install git\n\
             Other:  https://git-scm.com/downloads",
            v.0, v.1, v.2, MIN_GIT_VERSION.0, MIN_GIT_VERSION.1, MIN_GIT_VERSION.2,
        )));
    }
    Ok(v)
}

/// Extract `(major, minor, patch)` from strings like
/// `git version 2.50.1 (Apple Git-155)` or `git version 2.30.2`.
fn parse_git_version(s: &str) -> Option<(u32, u32, u32)> {
    let body = s.strip_prefix("git version ")?;
    let token = body.split_whitespace().next()?;
    let mut parts = token.split('.');
    let major: u32 = parts.next()?.parse().ok()?;
    let minor: u32 = parts.next()?.parse().ok()?;
    let patch_str = parts.next().unwrap_or("0");
    // Some distros append `-<release>`; trim that.
    let patch_str = patch_str.split('-').next().unwrap_or("0");
    let patch: u32 = patch_str.parse().unwrap_or(0);
    Some((major, minor, patch))
}

#[cfg(test)]
mod version_tests {
    use super::*;

    #[test]
    fn parses_apple_git() {
        assert_eq!(
            parse_git_version("git version 2.50.1 (Apple Git-155)"),
            Some((2, 50, 1))
        );
    }
    #[test]
    fn parses_plain() {
        assert_eq!(parse_git_version("git version 2.30.2"), Some((2, 30, 2)));
    }
    #[test]
    fn parses_distro_suffix() {
        assert_eq!(
            parse_git_version("git version 2.39.5-debian-1"),
            Some((2, 39, 5))
        );
    }
    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_git_version("hg version 2.30.0"), None);
        assert_eq!(parse_git_version("git version"), None);
    }
}

fn run(cmd: &mut Command, label: &str) -> Result<Output> {
    let out = cmd.output().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => WalGitError::GitNotInstalled,
        _ => WalGitError::git(format!("failed to run {}: {}", label, e)),
    })?;
    Ok(out)
}

fn ensure_ok(out: &Output, label: &str) -> Result<()> {
    if out.status.success() {
        Ok(())
    } else {
        Err(WalGitError::git(format!(
            "{} failed: {}",
            label,
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

/// Pack only the objects reachable from `tip` but NOT reachable from any of
/// `exclude_tips`. Used for PR packfiles: the maintainer already has the
/// upstream history, so the fork only uploads the delta.
///
/// Returns `Ok((pack_bytes, included_commit_count))`. If no new commits exist
/// (e.g., source is already at or behind upstream), returns an empty pack and
/// count = 0 so the caller can short-circuit.
pub fn pack_objects_incremental(
    repo_path: &Path,
    tip: &str,
    exclude_tips: &[String],
) -> Result<(Vec<u8>, usize)> {
    let mut args: Vec<String> = vec!["rev-list".into(), "--objects".into(), tip.to_string()];
    for ex in exclude_tips {
        if ex.is_empty() {
            continue;
        }
        args.push(format!("^{}", ex));
    }
    let rev_list = run(
        Command::new("git")
            .args(args.iter().map(String::as_str))
            .current_dir(repo_path),
        "git rev-list",
    )?;
    ensure_ok(&rev_list, "git rev-list")?;

    let stdout = String::from_utf8_lossy(&rev_list.stdout);
    let objects: Vec<&str> = stdout
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .collect();

    if objects.is_empty() {
        return Ok((Vec::new(), 0));
    }

    let commit_count = count_new_commits(repo_path, tip, exclude_tips)?;

    let mut child = Command::new("git")
        .args(["pack-objects", "--stdout"])
        .current_dir(repo_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => WalGitError::GitNotInstalled,
            _ => WalGitError::git(format!("failed to spawn git pack-objects: {}", e)),
        })?;
    if let Some(mut stdin) = child.stdin.take() {
        for obj in &objects {
            writeln!(stdin, "{}", obj)?;
        }
    }
    let out = child
        .wait_with_output()
        .map_err(|e| WalGitError::git(format!("git pack-objects wait failed: {}", e)))?;
    ensure_ok(&out, "git pack-objects")?;
    Ok((out.stdout, commit_count))
}

fn count_new_commits(repo_path: &Path, tip: &str, exclude: &[String]) -> Result<usize> {
    let mut args: Vec<String> = vec!["rev-list".into(), "--count".into(), tip.to_string()];
    for ex in exclude {
        if ex.is_empty() {
            continue;
        }
        args.push(format!("^{}", ex));
    }
    let out = run(
        Command::new("git")
            .args(args.iter().map(String::as_str))
            .current_dir(repo_path),
        "git rev-list --count",
    )?;
    ensure_ok(&out, "git rev-list --count")?;
    Ok(String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(0))
}

/// Pack all reachable git objects in the repo into a single packfile.
pub fn pack_objects(repo_path: &Path) -> Result<Vec<u8>> {
    let rev_list = run(
        Command::new("git")
            .args(["rev-list", "--objects", "--all"])
            .current_dir(repo_path),
        "git rev-list",
    )?;
    ensure_ok(&rev_list, "git rev-list")?;

    let objects: Vec<String> = String::from_utf8_lossy(&rev_list.stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().next().map(String::from))
        .collect();

    if objects.is_empty() {
        return Err(WalGitError::git(
            "repository has no commits — make at least one commit before pushing",
        ));
    }

    let mut child = Command::new("git")
        .args(["pack-objects", "--stdout"])
        .current_dir(repo_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => WalGitError::GitNotInstalled,
            _ => WalGitError::git(format!("failed to spawn git pack-objects: {}", e)),
        })?;

    if let Some(mut stdin) = child.stdin.take() {
        for obj in &objects {
            writeln!(stdin, "{}", obj)?;
        }
    }

    let out = child
        .wait_with_output()
        .map_err(|e| WalGitError::git(format!("git pack-objects wait failed: {}", e)))?;
    ensure_ok(&out, "git pack-objects")?;
    Ok(out.stdout)
}

/// Unpack a packfile into the git object store at `repo_path`.
pub fn unpack_objects(repo_path: &Path, pack_data: &[u8]) -> Result<()> {
    let mut child = Command::new("git")
        .args(["unpack-objects"])
        .current_dir(repo_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => WalGitError::GitNotInstalled,
            _ => WalGitError::git(format!("failed to spawn git unpack-objects: {}", e)),
        })?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(pack_data)?;
    }

    let out = child
        .wait_with_output()
        .map_err(|e| WalGitError::git(format!("git unpack-objects wait failed: {}", e)))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        // Exit 1 + "already exists" is a benign reunpack — every object already in the store.
        if out.status.code() == Some(1) && stderr.contains("already") {
            return Ok(());
        }
        return Err(WalGitError::git(format!(
            "git unpack-objects failed: {}",
            stderr.trim()
        )));
    }
    Ok(())
}

/// Resolve the absolute path to the `.git/` directory for `repo_path`.
/// Honours git worktrees (where `.git` is a file pointing elsewhere) and
/// submodules, so `.git/walgit/` always lands in the right place.
pub fn git_dir(repo_path: &Path) -> Result<std::path::PathBuf> {
    let out = run(
        Command::new("git")
            .args(["rev-parse", "--absolute-git-dir"])
            .current_dir(repo_path),
        "git rev-parse --absolute-git-dir",
    )?;
    ensure_ok(&out, "git rev-parse --absolute-git-dir")?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        return Err(WalGitError::git("empty git-dir from rev-parse"));
    }
    Ok(std::path::PathBuf::from(s))
}

pub fn rev_parse(repo_path: &Path, refname: &str) -> Result<String> {
    // NOTE: `git rev-parse` is unusual — it literally echoes any unknown arg
    // (including `--end-of-options`) instead of treating it as a separator.
    // We rely on upstream validators (`crate::validate::repo_name` etc.) to
    // ensure `refname` never starts with `-`.
    let out = run(
        Command::new("git")
            .args(["rev-parse", refname])
            .current_dir(repo_path),
        "git rev-parse",
    )?;
    ensure_ok(&out, "git rev-parse")?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub fn get_head_commit(repo_path: &Path) -> Result<String> {
    rev_parse(repo_path, "HEAD")
}

pub fn get_commit_message(repo_path: &Path, commit_hash: &str) -> Result<String> {
    let out = run(
        Command::new("git")
            .args(["log", "-1", "--format=%s", "--end-of-options", commit_hash])
            .current_dir(repo_path),
        "git log",
    )?;
    ensure_ok(&out, "git log")?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub struct CommitInfo {
    pub message: String,
    pub author_name: String,
    pub author_email: String,
    pub timestamp: i64,
}

pub fn get_commit_info(repo_path: &Path, commit_hash: &str) -> Result<CommitInfo> {
    let out = run(
        Command::new("git")
            .args(["log", "-1", "--format=%s|%an|%ae|%at", commit_hash])
            .current_dir(repo_path),
        "git log",
    )?;
    ensure_ok(&out, "git log")?;
    let s = String::from_utf8_lossy(&out.stdout);
    let mut parts = s.trim().splitn(4, '|');
    Ok(CommitInfo {
        message: parts.next().unwrap_or("").to_string(),
        author_name: parts.next().unwrap_or("").to_string(),
        author_email: parts.next().unwrap_or("").to_string(),
        timestamp: parts.next().and_then(|t| t.parse().ok()).unwrap_or(0),
    })
}

pub fn init(path: &Path) -> Result<()> {
    let out = run(
        Command::new("git").args(["init"]).current_dir(path),
        "git init",
    )?;
    ensure_ok(&out, "git init")
}

pub fn add(repo_path: &Path, paths: &[&str]) -> Result<()> {
    let mut cmd = Command::new("git");
    cmd.arg("add").args(paths).current_dir(repo_path);
    let out = run(&mut cmd, "git add")?;
    ensure_ok(&out, "git add")
}

/// Returns true if the repository has at least one staged change ready to commit.
pub fn has_staged_changes(repo_path: &Path) -> bool {
    Command::new("git")
        .args(["diff", "--cached", "--quiet"])
        .current_dir(repo_path)
        .status()
        .map(|s| !s.success()) // exit code 1 = changes present
        .unwrap_or(false)
}

pub fn commit(repo_path: &Path, message: &str) -> Result<()> {
    let out = run(
        Command::new("git")
            .args(["commit", "-m", message])
            .current_dir(repo_path),
        "git commit",
    )?;
    ensure_ok(&out, "git commit")
}

/// Same as `commit` but feeds the message via `-F -` (stdin) so it can hold
/// arbitrary bytes including multi-paragraph JSON footers without the shell
/// quoting hell of `-m`.
pub fn commit_with_long_message(repo_path: &Path, message: &str) -> Result<()> {
    let mut child = Command::new("git")
        .args(["commit", "-F", "-"])
        .current_dir(repo_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => WalGitError::GitNotInstalled,
            _ => WalGitError::git(format!("failed to spawn git commit: {}", e)),
        })?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(message.as_bytes())?;
    }
    let out = child
        .wait_with_output()
        .map_err(|e| WalGitError::git(format!("git commit wait failed: {}", e)))?;
    ensure_ok(&out, "git commit")
}

/// Read the full raw commit message for a given SHA. Used by `walgit show`,
/// `walgit log --traces`, and the trace extractor.
pub fn read_commit_message(repo_path: &Path, commit_hash: &str) -> Result<String> {
    let out = run(
        Command::new("git")
            .args(["log", "-1", "--format=%B", "--end-of-options", commit_hash])
            .current_dir(repo_path),
        "git log -1 --format=%B",
    )?;
    ensure_ok(&out, "git log")?;
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// One-line iteration of recent commits on a branch. Returns
/// `[(sha, raw_message)]` in newest-first order.
pub fn recent_commits(repo_path: &Path, refname: &str, limit: usize) -> Result<Vec<(String, String)>> {
    // Format: `<sha>\x1F<full message>\x1E` — record separator + group sep.
    let limit_arg = format!("-{}", limit);
    let out = run(
        Command::new("git")
            .args([
                "log",
                &limit_arg,
                "--format=%H%x1F%B%x1E",
                refname,
            ])
            .current_dir(repo_path),
        "git log",
    )?;
    ensure_ok(&out, "git log")?;
    let s = String::from_utf8_lossy(&out.stdout);
    let mut out_vec = Vec::new();
    for record in s.split('\x1E') {
        let trimmed = record.trim_start_matches('\n');
        if trimmed.is_empty() {
            continue;
        }
        if let Some((sha, body)) = trimmed.split_once('\x1F') {
            out_vec.push((sha.trim().to_string(), body.to_string()));
        }
    }
    Ok(out_vec)
}

/// True if `git rev-parse HEAD` resolves — i.e. there is at least one commit.
pub fn has_any_commits(repo_path: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--verify", "HEAD"])
        .current_dir(repo_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Look up a configured remote URL. Returns Ok(None) if the remote doesn't
/// exist, Err only on subprocess failure.
pub fn get_remote_url(repo_path: &Path, name: &str) -> Result<Option<String>> {
    let out = Command::new("git")
        .args(["remote", "get-url", name])
        .current_dir(repo_path)
        .output()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => WalGitError::GitNotInstalled,
            _ => WalGitError::git(format!("git remote get-url failed: {}", e)),
        })?;
    if out.status.success() {
        Ok(Some(String::from_utf8_lossy(&out.stdout).trim().to_string()))
    } else {
        // exit 2 = remote doesn't exist; treat any non-success as "absent".
        Ok(None)
    }
}

/// Stream `git diff base..head` (and optional `--stat`) directly to the
/// process's stdout/stderr — git's own pager kicks in automatically when
/// stdout is a TTY.
pub fn stream_diff(repo_path: &Path, base: &str, head: &str, stat_only: bool) -> Result<()> {
    let range = format!("{}..{}", base, head);

    // Summary stat first, always — it's cheap and gives the user a quick map.
    let summary_status = Command::new("git")
        .args(["diff", "--stat", "--color=always", "--end-of-options", &range])
        .current_dir(repo_path)
        .status()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => WalGitError::GitNotInstalled,
            _ => WalGitError::git(format!("git diff --stat failed to start: {}", e)),
        })?;
    if !summary_status.success() {
        return Err(WalGitError::git("git diff --stat exited non-zero"));
    }

    if stat_only {
        return Ok(());
    }

    let body_status = Command::new("git")
        .args(["diff", "--color=always", "--end-of-options", &range])
        .current_dir(repo_path)
        .status()
        .map_err(|e| WalGitError::git(format!("git diff failed to start: {}", e)))?;
    if !body_status.success() {
        return Err(WalGitError::git("git diff exited non-zero"));
    }
    Ok(())
}

/// Idempotently set a git remote: adds it if missing, otherwise updates the URL.
pub fn set_remote(repo_path: &Path, name: &str, url: &str) -> Result<()> {
    if get_remote_url(repo_path, name)?.is_some() {
        let out = run(
            Command::new("git")
                .args(["remote", "set-url", name, url])
                .current_dir(repo_path),
            "git remote set-url",
        )?;
        ensure_ok(&out, "git remote set-url")
    } else {
        let out = run(
            Command::new("git")
                .args(["remote", "add", name, url])
                .current_dir(repo_path),
            "git remote add",
        )?;
        ensure_ok(&out, "git remote add")
    }
}

pub fn checkout(repo_path: &Path, commit_hash: &str) -> Result<()> {
    let head_path = repo_path.join(".git").join("HEAD");
    std::fs::write(&head_path, format!("{}\n", commit_hash))?;
    let out = run(
        Command::new("git")
            .args(["checkout", "-f", commit_hash])
            .current_dir(repo_path),
        "git checkout",
    )?;
    ensure_ok(&out, "git checkout")
}

/// True when `hash` exists locally as a git object.
pub fn object_exists(repo_path: &Path, hash: &str) -> bool {
    if hash.is_empty() {
        return false;
    }
    Command::new("git")
        .args(["cat-file", "-e", hash])
        .current_dir(repo_path)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Find the tip commit reachable in the repo but not from `target_branch`.
/// Used after unpacking a source packfile to identify the PR source tip.
pub fn find_foreign_tip(repo_path: &Path, target_branch: &str) -> Result<String> {
    // Use `^<branch>` exclusion syntax — `--not=<branch>` is not a valid
    // `git log` flag (the old code happened to slip through on older gits but
    // modern git rejects it).
    let exclude = format!("^{}", target_branch);
    let out = run(
        Command::new("git")
            .args([
                "log",
                "--all",
                &exclude,
                "--format=%H",
                "--max-count=1",
                "--topo-order",
            ])
            .current_dir(repo_path),
        "git log",
    )?;
    ensure_ok(&out, "git log")?;
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if sha.is_empty() {
        return Err(WalGitError::git(
            "no new commits found after unpacking source — source already merged?",
        ));
    }
    Ok(sha)
}

pub fn merge_fast_forward(repo_path: &Path, target_branch: &str, source_sha: &str) -> Result<()> {
    let out = run(
        Command::new("git")
            .args(["checkout", target_branch])
            .current_dir(repo_path),
        "git checkout",
    )?;
    ensure_ok(&out, "git checkout")?;

    let out = run(
        Command::new("git")
            .args(["merge", "--ff-only", source_sha])
            .current_dir(repo_path),
        "git merge",
    )?;
    if !out.status.success() {
        return Err(WalGitError::git(format!(
            "git merge --ff-only failed (branches diverged): {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}
