// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

//! Spinner and styled output for user-facing CLI commands.

use console::{Term, style};
use indicatif::{ProgressBar, ProgressStyle};
use std::io::{BufRead, Write};
use std::sync::OnceLock;
use std::time::Duration;

/// Global flag, set once at program start from `Config.display.short_ids`.
/// When `false` (default), `short_id` returns the full address.
static SHORT_IDS: OnceLock<bool> = OnceLock::new();

/// Configure ID rendering. Called from `main.rs` after config is loaded.
/// Subsequent calls are no-ops (OnceLock).
pub fn set_short_ids(enabled: bool) {
    let _ = SHORT_IDS.set(enabled);
}

fn short_ids_enabled() -> bool {
    *SHORT_IDS.get().unwrap_or(&false)
}

/// Tagline shown under the banner.
const TAGLINE: &str = "decentralized git on walrus + sui";

/// The six rows of the WalGit wordmark.
const BANNER_LINES: [&str; 6] = [
    " ██╗    ██╗ █████╗ ██╗      ██████╗ ██╗████████╗",
    " ██║    ██║██╔══██╗██║     ██╔════╝ ██║╚══██╔══╝",
    " ██║ █╗ ██║███████║██║     ██║  ███╗██║   ██║",
    " ██║███╗██║██╔══██║██║     ██║   ██║██║   ██║",
    " ╚███╔███╔╝██║  ██║███████╗╚██████╔╝██║   ██║",
    "  ╚══╝╚══╝ ╚═╝  ╚═╝╚══════╝ ╚═════╝ ╚═╝   ╚═╝",
];

/// Purple→teal gradient, one RGB triple per banner row. Matches the colours
/// the `install.sh` bootstrap prints, so the installer and CLI share a look.
const BANNER_GRADIENT: [(u8, u8, u8); 6] = [
    (163, 113, 247),
    (139, 124, 248),
    (116, 150, 240),
    (96, 178, 232),
    (86, 200, 222),
    (88, 217, 214),
];

/// Print the WalGit banner with a tagline. Renders as a 24-bit purple→teal
/// gradient on truecolor terminals, falls back to a single cyan wordmark
/// otherwise, and to plain text when colours are disabled (NO_COLOR / no tty).
pub fn banner() {
    println!();
    if console::colors_enabled() && truecolor_supported() {
        for (line, (r, g, b)) in BANNER_LINES.iter().zip(BANNER_GRADIENT.iter()) {
            // Raw ANSI: console's Color enum has no 24-bit variant.
            println!("\x1b[1;38;2;{};{};{}m{}\x1b[0m", r, g, b, line);
        }
    } else {
        for line in BANNER_LINES.iter() {
            println!("{}", style(line).cyan().bold());
        }
    }
    println!("  {}", style(TAGLINE).dim());
    println!();
}

/// True when the terminal advertises 24-bit colour via `COLORTERM`.
fn truecolor_supported() -> bool {
    std::env::var("COLORTERM")
        .map(|v| v == "truecolor" || v == "24bit")
        .unwrap_or(false)
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

pub fn ewarn(msg: impl AsRef<str>) {
    eprintln!(
        "  {} {}",
        style("!").yellow().bold(),
        style(msg.as_ref()).yellow()
    );
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

/// Render a Sui object ID for display. Honours the global short-id setting.
/// In short mode the format is `0xabcde…12345` (5 chars after 0x + last 5).
pub fn short_id(id: &str) -> String {
    if !short_ids_enabled() {
        return id.to_string();
    }
    let s = id.trim_start_matches("0x");
    if s.len() < 12 {
        return id.to_string();
    }
    format!("0x{}…{}", &s[..5], &s[s.len() - 5..])
}

pub fn short_hash(h: &str) -> String {
    let take = 8.min(h.len());
    h[..take].to_string()
}
