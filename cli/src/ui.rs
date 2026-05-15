// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

//! Spinner and styled output for user-facing CLI commands.

use console::{Term, style};
use indicatif::{ProgressBar, ProgressStyle};
use std::io::{BufRead, Write};
use std::time::Duration;

/// Tagline shown under the banner.
const TAGLINE: &str = "decentralized git on walrus + sui";

const BANNER: &str = r"
 ██╗    ██╗ █████╗ ██╗      ██████╗ ██╗████████╗
 ██║    ██║██╔══██╗██║     ██╔════╝ ██║╚══██╔══╝
 ██║ █╗ ██║███████║██║     ██║  ███╗██║   ██║
 ██║███╗██║██╔══██║██║     ██║   ██║██║   ██║
 ╚███╔███╔╝██║  ██║███████╗╚██████╔╝██║   ██║
  ╚══╝╚══╝ ╚═╝  ╚═╝╚══════╝ ╚═════╝ ╚═╝   ╚═╝";

/// Print the WalGit banner in cyan with a tagline.
pub fn banner() {
    println!("{}", style(BANNER).cyan().bold());
    println!("  {}", style(TAGLINE).dim());
    println!();
}

/// Print a styled section header like `── init ──────────────────────`.
pub fn header(title: &str) {
    let width = Term::stdout().size().1 as usize;
    let title_part = format!(" {} ", title);
    let dashes = width.saturating_sub(title_part.len() + 4).max(8);
    println!(
        "{}{}{}",
        style("── ").cyan().dim(),
        style(title).cyan().bold(),
        style(format!(" {}", "─".repeat(dashes))).cyan().dim()
    );
}

pub fn divider() {
    let width = Term::stdout().size().1 as usize;
    println!("{}", style("─".repeat(width.min(60))).dim());
}

pub fn spinner(msg: impl Into<String>) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::with_template("{spinner:.cyan} {msg}")
            .unwrap()
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
    );
    pb.set_message(msg.into());
    pb.enable_steady_tick(Duration::from_millis(80));
    pb
}

pub fn success(msg: impl AsRef<str>) {
    println!("  {} {}", style("✓").green().bold(), msg.as_ref());
}

pub fn info(msg: impl AsRef<str>) {
    println!("  {} {}", style("·").cyan(), msg.as_ref());
}

pub fn step(msg: impl AsRef<str>) {
    println!("  {} {}", style("→").cyan().bold(), msg.as_ref());
}

pub fn warn(msg: impl AsRef<str>) {
    eprintln!("  {} {}", style("!").yellow().bold(), style(msg.as_ref()).yellow());
}

pub fn error(msg: impl AsRef<str>) {
    eprintln!("  {} {}", style("✗").red().bold(), style(msg.as_ref()).red());
}

pub fn dim(msg: impl AsRef<str>) -> String {
    format!("{}", style(msg.as_ref()).dim())
}

// ─── stderr variants for git-remote helper, where stdout is reserved for the
//     git protocol. Functionally identical to the println-based ones above,
//     just routed to stderr.

pub fn eheader(title: &str) {
    let width = Term::stderr().size().1 as usize;
    let title_part = format!(" {} ", title);
    let dashes = width.saturating_sub(title_part.len() + 4).max(8);
    eprintln!(
        "{}{}{}",
        style("── ").cyan().dim(),
        style(title).cyan().bold(),
        style(format!(" {}", "─".repeat(dashes))).cyan().dim()
    );
}

pub fn esuccess(msg: impl AsRef<str>) {
    eprintln!("  {} {}", style("✓").green().bold(), msg.as_ref());
}

pub fn einfo(msg: impl AsRef<str>) {
    eprintln!("  {} {}", style("·").cyan(), msg.as_ref());
}

pub fn estep(msg: impl AsRef<str>) {
    eprintln!("  {} {}", style("→").cyan().bold(), msg.as_ref());
}

pub fn label(s: &str) -> String {
    format!("{}", style(s).cyan())
}

pub fn highlight(s: &str) -> String {
    format!("{}", style(s).bold())
}

/// Interactively ask a yes/no question on stdin. `default_yes = true` means
/// pressing enter accepts.
pub fn prompt_yes_no(question: &str, default_yes: bool) -> std::io::Result<bool> {
    let hint = if default_yes { "[Y/n]" } else { "[y/N]" };
    print!(
        "  {} {} {} ",
        style("?").yellow().bold(),
        question,
        style(hint).dim()
    );
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let trimmed = line.trim().to_lowercase();
    Ok(match trimmed.as_str() {
        "" => default_yes,
        "y" | "yes" => true,
        _ => false,
    })
}

pub fn fmt_bytes(n: usize) -> String {
    if n < 1_024 {
        format!("{} B", n)
    } else if n < 1_024 * 1_024 {
        format!("{:.1} KB", n as f64 / 1_024.0)
    } else {
        format!("{:.2} MB", n as f64 / (1_024.0 * 1_024.0))
    }
}

pub fn short_id(id: &str) -> String {
    let s = id.trim_start_matches("0x");
    let take = 10.min(s.len());
    format!("0x{}…", &s[..take])
}

pub fn short_hash(h: &str) -> String {
    let take = 8.min(h.len());
    h[..take].to_string()
}
