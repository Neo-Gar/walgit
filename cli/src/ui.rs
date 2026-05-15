// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

//! Spinner and styled output for user-facing CLI commands.

use console::style;
use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;

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

pub fn warn(msg: impl AsRef<str>) {
    eprintln!("  {} {}", style("!").yellow().bold(), msg.as_ref());
}

pub fn error(msg: impl AsRef<str>) {
    eprintln!("  {} {}", style("✗").red().bold(), style(msg.as_ref()).red());
}

pub fn dim(msg: impl AsRef<str>) -> String {
    format!("{}", style(msg.as_ref()).dim())
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
