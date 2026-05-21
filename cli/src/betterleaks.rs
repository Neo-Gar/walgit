// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

//! Betterleaks integration — secret scanning before content reaches the
//! blockchain or other shared storage.
//!
//! Scan surfaces:
//! - [`scan_path`] — `betterleaks dir <path>`: working tree scan for push/PR.
//! - [`scan_text`] — `betterleaks stdin`: inline scan for MemWal payloads.
//!
//! Unavailability policy:
//! Call [`confirm_continue_without_scan`] when the scan returns
//! [`ScanOutcome::Unavailable`]. It prints a prominent warning and requires the
//! user to type `y` to proceed; anything else (including just Enter) aborts.
//! The prompt reads from `/dev/tty` so it works even inside git-remote helpers
//! where stdin is occupied by the git protocol.

use std::io::{BufRead as _, Write as _};
use std::path::Path;
use std::process::{Command, Stdio};

// ─── Outcome ─────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum ScanOutcome {
    Clean,
    /// betterleaks exited non-zero; `output` carries its stdout+stderr.
    SecretsFound {
        output: String,
    },
    /// `betterleaks` binary not found on PATH — tool is not installed.
    Unavailable,
}

// ─── Config gate ─────────────────────────────────────────────────────────────

/// Return `true` when betterleaks is disabled via `[betterleaks] skip = true`
/// in `~/.walgit/config.toml`. When skipped, ALL scans and warnings are
/// suppressed — the caller should short-circuit before calling any other fn.
pub fn is_skipped() -> bool {
    crate::config::load()
        .map(|c| c.betterleaks.skip)
        .unwrap_or(false)
}

// ─── Availability check ───────────────────────────────────────────────────────

/// Return `true` if the `betterleaks` binary is reachable on PATH.
pub fn is_available() -> bool {
    Command::new("betterleaks")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|_| true)
        .unwrap_or(false)
}

// ─── Scan functions ───────────────────────────────────────────────────────────

/// Scan git history at `repo_dir` with `betterleaks git`.
///
/// This is the correct mode for push and PR: it inspects git objects directly
/// so it catches secrets in committed files regardless of `.gitignore`.
/// `betterleaks dir` would miss a `.env` that is committed but `.gitignore`d.
///
/// Blocking — runs a child process and waits.
pub fn scan_git(repo_dir: &Path) -> ScanOutcome {
    let path_str = match repo_dir.to_str() {
        Some(s) => s,
        None => return ScanOutcome::Unavailable,
    };
    match Command::new("betterleaks")
        .args(["git", path_str, "-v"])
        .output()
    {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ScanOutcome::Unavailable,
        Err(_) => ScanOutcome::Unavailable,
        Ok(out) if out.status.success() => ScanOutcome::Clean,
        Ok(out) => ScanOutcome::SecretsFound {
            output: combined_output(&out.stdout, &out.stderr),
        },
    }
}

/// Scan a directory or file with `betterleaks dir`.
/// NOTE: respects `.gitignore` — use [`scan_git`] when scanning committed
/// content, and this only for untracked files or non-git directories.
pub fn scan_path(path: &Path) -> ScanOutcome {
    let path_str = match path.to_str() {
        Some(s) => s,
        None => return ScanOutcome::Unavailable,
    };
    match Command::new("betterleaks")
        .args(["dir", path_str, "-v"])
        .output()
    {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ScanOutcome::Unavailable,
        Err(_) => ScanOutcome::Unavailable,
        Ok(out) if out.status.success() => ScanOutcome::Clean,
        Ok(out) => ScanOutcome::SecretsFound {
            output: combined_output(&out.stdout, &out.stderr),
        },
    }
}

/// Scan `text` by piping it to `betterleaks stdin`.
/// Blocking — runs a child process and waits.
pub fn scan_text(text: &str) -> ScanOutcome {
    let mut child = match Command::new("betterleaks")
        .args(["stdin", "-v"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ScanOutcome::Unavailable,
        Err(_) => return ScanOutcome::Unavailable,
        Ok(c) => c,
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(text.as_bytes());
    }
    match child.wait_with_output() {
        Err(_) => ScanOutcome::Unavailable,
        Ok(out) if out.status.success() => ScanOutcome::Clean,
        Ok(out) => ScanOutcome::SecretsFound {
            output: combined_output(&out.stdout, &out.stderr),
        },
    }
}

// ─── Unavailability prompt ────────────────────────────────────────────────────

/// Print a prominent warning and ask the user to confirm they want to continue
/// without secret scanning. Default answer is **No** — pressing Enter aborts.
///
/// Returns `true` if the user explicitly typed `y` or `yes`.
/// Returns `false` for any other input (including empty / `n` / `N`).
///
/// Reads from `/dev/tty` on Unix so it works inside `git-remote-walgit` where
/// stdin is consumed by the git remote-helper protocol.
pub fn confirm_continue_without_scan() -> bool {
    let warning = concat!(
        "\n",
        "  ╔══════════════════════════════════════════════════════════════════╗\n",
        "  ║  ⚠  BETTERLEAKS IS NOT INSTALLED — SECRET SCANNING DISABLED  ⚠   ║\n",
        "  ╠══════════════════════════════════════════════════════════════════╣\n",
        "  ║  Without betterleaks, WalGit CANNOT check whether your code,     ║\n",
        "  ║  commit messages, or AI traces contain secrets (API keys,        ║\n",
        "  ║  private keys, passwords, tokens).                               ║\n",
        "  ║                                                                  ║\n",
        "  ║  Data pushed to Walrus / Sui is IMMUTABLE and PUBLIC.            ║\n",
        "  ║  A leaked secret cannot be revoked by deleting a file.           ║\n",
        "  ║                                                                  ║\n",
        "  ║  Install betterleaks:                                            ║\n",
        "  ║    macOS : brew install betterleaks                              ║\n",
        "  ║    Linux : go install github.com/betterleaks/betterleaks@latest  ║\n",
        "  ║    Docs  : https://github.com/betterleaks/betterleaks            ║\n",
        "  ╚══════════════════════════════════════════════════════════════════╝\n",
    );

    eprint!("{warning}");
    eprint!("  Continue WITHOUT secret scanning? [N/y] ");
    let _ = std::io::stderr().flush();

    let answer = read_from_tty_or_stdin();
    let answer = answer.trim().to_lowercase();
    answer == "y" || answer == "yes"
}

/// Read one line from `/dev/tty` (Unix) so the prompt works even when the
/// process stdin is piped (e.g. inside git-remote-walgit). Falls back to
/// stdin on non-Unix platforms.
fn read_from_tty_or_stdin() -> String {
    #[cfg(unix)]
    {
        if let Ok(tty) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
        {
            let mut line = String::new();
            let mut reader = std::io::BufReader::new(tty);
            let _ = reader.read_line(&mut line);
            return line;
        }
    }
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    line
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn combined_output(stdout: &[u8], stderr: &[u8]) -> String {
    let out = String::from_utf8_lossy(stdout);
    let err = String::from_utf8_lossy(stderr);
    let out = out.trim();
    let err = err.trim();
    match (out.is_empty(), err.is_empty()) {
        (false, false) => format!("{out}\n{err}"),
        (false, true) => out.to_string(),
        (true, false) => err.to_string(),
        (true, true) => String::new(),
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combined_output_both() {
        let r = combined_output(b"some output", b"some error");
        assert!(r.contains("some output"));
        assert!(r.contains("some error"));
    }

    #[test]
    fn combined_output_stdout_only() {
        assert_eq!(combined_output(b"  found secrets  ", b""), "found secrets");
    }

    #[test]
    fn combined_output_stderr_only() {
        assert_eq!(combined_output(b"", b"  error msg  "), "error msg");
    }

    #[test]
    fn combined_output_both_empty() {
        assert_eq!(combined_output(b"", b""), "");
    }
}
