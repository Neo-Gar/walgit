// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

//! Git CLI subprocess wrappers.

use crate::error::{Result, WalGitError};
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Output, Stdio};

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

pub fn rev_parse(repo_path: &Path, refname: &str) -> Result<String> {
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
            .args(["log", "-1", "--format=%s", commit_hash])
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
    let out = run(
        Command::new("git")
            .args([
                "log",
                "--all",
                &format!("--not={}", target_branch),
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
