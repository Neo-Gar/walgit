// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

//! Input validators applied at every entry where user-supplied strings reach
//! the filesystem or a subprocess. Without these, names like `../etc` or
//! `--upload-pack=evil` would slip through into `Path::join` or git argv.

use crate::error::{Result, WalGitError};

/// Repository names appear in URLs (`walgit://owner/<name>`), in filesystem
/// paths (`cwd.join(&name)`), and in git refspecs. Whitelist what's allowed so
/// path-traversal and shell-meaningful characters can't sneak through.
///
/// Rules:
/// - 1..=64 characters
/// - ASCII alphanumeric, `-`, `_`, `.` allowed
/// - Must NOT be `.` or `..`
/// - Must NOT start with `-` (would look like a git flag)
pub fn repo_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(WalGitError::other("repository name is empty".to_string()));
    }
    if name.len() > 64 {
        return Err(WalGitError::other(format!(
            "repository name too long ({} > 64 chars)",
            name.len()
        )));
    }
    if name == "." || name == ".." {
        return Err(WalGitError::other(format!(
            "repository name '{}' is reserved",
            name
        )));
    }
    if name.starts_with('-') {
        return Err(WalGitError::other(format!(
            "repository name '{}' may not start with '-' (would be parsed as a flag)",
            name
        )));
    }
    for c in name.chars() {
        let ok = c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.';
        if !ok {
            return Err(WalGitError::other(format!(
                "repository name '{}' contains forbidden character '{}' (allowed: a-z, A-Z, 0-9, '-', '_', '.')",
                name, c
            )));
        }
    }
    Ok(())
}

/// Sui addresses and object IDs are 32-byte hex strings, optionally prefixed
/// with `0x`. We use this for `cache clean <repo_id>` so a malicious value
/// can't escape `~/.walgit/work/` and remove an unrelated directory.
pub fn sui_object_id(id: &str) -> Result<()> {
    let stripped = id.strip_prefix("0x").unwrap_or(id);
    if stripped.is_empty() {
        return Err(WalGitError::other("object id is empty".to_string()));
    }
    // Up to 64 hex chars; Sui addresses are 32 bytes = 64 hex but leading
    // zeros are sometimes stripped, so we accept any length 1..=64.
    if stripped.len() > 64 {
        return Err(WalGitError::other(format!(
            "object id too long ({} > 64 hex chars)",
            stripped.len()
        )));
    }
    if !stripped.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(WalGitError::other(format!(
            "object id '{}' contains non-hex characters",
            id
        )));
    }
    Ok(())
}

/// Git branch and refnames passed to git subprocesses or stored on-chain.
/// Rejects values that could be parsed as flags or enable path traversal.
///
/// Rules:
/// - 1..=200 characters
/// - Must not start with `-` (would look like a git flag)
/// - Must not start with `.` (relative path component)
/// - Must not contain `..` (path traversal)
/// - Must not contain control characters, NUL, space, `~`, `^`, `:`, `?`,
///   `*`, `[`, or `\` (disallowed by git's refname spec)
/// - Must not end with `.lock` (git lock files)
/// - Must not be the bare `@` (git HEAD shorthand)
pub fn branch_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(WalGitError::other("branch name is empty".to_string()));
    }
    if name.len() > 200 {
        return Err(WalGitError::other(format!(
            "branch name too long ({} > 200 chars)",
            name.len()
        )));
    }
    if name == "@" {
        return Err(WalGitError::other(
            "branch name '@' is reserved by git".to_string(),
        ));
    }
    if name.starts_with('-') {
        return Err(WalGitError::other(format!(
            "branch name '{}' may not start with '-' (would be parsed as a flag)",
            name
        )));
    }
    if name.starts_with('.') {
        return Err(WalGitError::other(format!(
            "branch name '{}' may not start with '.'",
            name
        )));
    }
    if name.contains("..") {
        return Err(WalGitError::other(format!(
            "branch name '{}' contains '..' (path traversal)",
            name
        )));
    }
    if name.ends_with(".lock") {
        return Err(WalGitError::other(format!(
            "branch name '{}' ends with '.lock' (reserved by git)",
            name
        )));
    }
    const FORBIDDEN: &[char] = &[
        '\0', ' ', '\t', '\n', '\r', '~', '^', ':', '?', '*', '[', '\\',
    ];
    for c in name.chars() {
        if (c as u32) < 32 || FORBIDDEN.contains(&c) {
            return Err(WalGitError::other(format!(
                "branch name '{}' contains forbidden character '{}'",
                name, c
            )));
        }
    }
    Ok(())
}

/// Git commit SHAs (short or full) used as filesystem path components and
/// git argument values. Allows only hex characters, 7–64 chars.
pub fn git_sha(sha: &str) -> Result<()> {
    if sha.len() < 7 || sha.len() > 64 {
        return Err(WalGitError::other(format!(
            "commit SHA '{}' must be 7–64 characters (got {})",
            sha,
            sha.len()
        )));
    }
    if !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(WalGitError::other(format!(
            "commit SHA '{}' contains non-hex characters",
            sha
        )));
    }
    Ok(())
}

/// Owner addresses parsed from `walgit://<owner>/<repo>` URLs. Stricter than
/// `sui_object_id`: must be exactly 64 hex chars (with or without `0x`) so we
/// don't accept fragments that might glob into something else later.
pub fn sui_address(addr: &str) -> Result<()> {
    let stripped = addr.strip_prefix("0x").unwrap_or(addr);
    if stripped.len() != 64 {
        return Err(WalGitError::other(format!(
            "Sui address must be 64 hex chars (got {})",
            stripped.len()
        )));
    }
    if !stripped.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(WalGitError::other(format!(
            "Sui address '{}' contains non-hex characters",
            addr
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_name_accepts_normal() {
        assert!(repo_name("walgit").is_ok());
        assert!(repo_name("my-repo_2").is_ok());
        assert!(repo_name("a.b.c").is_ok());
        assert!(repo_name("X").is_ok());
    }

    #[test]
    fn repo_name_rejects_traversal() {
        assert!(repo_name("..").is_err());
        assert!(repo_name(".").is_err());
        assert!(repo_name("a/b").is_err());
        assert!(repo_name("a\\b").is_err());
    }

    #[test]
    fn repo_name_rejects_flag_lookalike() {
        assert!(repo_name("-evil").is_err());
        assert!(repo_name("--help").is_err());
    }

    #[test]
    fn repo_name_rejects_spaces_and_unicode() {
        assert!(repo_name("hello world").is_err());
        assert!(repo_name("café").is_err());
    }

    #[test]
    fn repo_name_rejects_empty_and_too_long() {
        assert!(repo_name("").is_err());
        assert!(repo_name(&"x".repeat(65)).is_err());
    }

    #[test]
    fn object_id_validation() {
        assert!(sui_object_id("0xabc123").is_ok());
        assert!(sui_object_id("abc123").is_ok());
        assert!(sui_object_id("0x").is_err()); // empty after strip
        assert!(sui_object_id("../etc").is_err());
        assert!(sui_object_id(&format!("0x{}", "f".repeat(65))).is_err());
    }

    #[test]
    fn address_strict_64() {
        let good = format!("0x{}", "a".repeat(64));
        assert!(sui_address(&good).is_ok());
        assert!(sui_address("0xabc").is_err());
        let too_long = format!("0x{}", "a".repeat(65));
        assert!(sui_address(&too_long).is_err());
    }

    #[test]
    fn branch_name_accepts_normal() {
        assert!(branch_name("main").is_ok());
        assert!(branch_name("feature/my-branch").is_ok());
        assert!(branch_name("fix-123").is_ok());
        assert!(branch_name("v1.2.3").is_ok());
    }

    #[test]
    fn branch_name_rejects_flag_lookalike() {
        assert!(branch_name("-x").is_err());
        assert!(branch_name("--evil").is_err());
    }

    #[test]
    fn branch_name_rejects_traversal() {
        assert!(branch_name("..").is_err());
        assert!(branch_name("a/../b").is_err());
    }

    #[test]
    fn branch_name_rejects_forbidden_chars() {
        assert!(branch_name("a b").is_err());
        assert!(branch_name("a~b").is_err());
        assert!(branch_name("a^b").is_err());
        assert!(branch_name("a:b").is_err());
        assert!(branch_name("a*b").is_err());
        assert!(branch_name("a[b").is_err());
        assert!(branch_name("a\\b").is_err());
    }

    #[test]
    fn branch_name_rejects_reserved() {
        assert!(branch_name("@").is_err());
        assert!(branch_name(".hidden").is_err());
        assert!(branch_name("locked.lock").is_err());
    }

    #[test]
    fn git_sha_accepts_valid() {
        assert!(git_sha("abcdef1").is_ok());
        assert!(git_sha(&"a".repeat(40)).is_ok());
        assert!(git_sha(&"f".repeat(64)).is_ok());
    }

    #[test]
    fn git_sha_rejects_invalid() {
        assert!(git_sha("abc").is_err());  // too short
        assert!(git_sha("abcdefg").is_err());  // non-hex 'g'
        assert!(git_sha("../evil").is_err());
        assert!(git_sha(&"a".repeat(65)).is_err());  // too long
    }
}
