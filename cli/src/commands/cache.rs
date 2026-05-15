// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

//! Manage the auto-clone cache at `~/.walgit/work/`.
//!
//! `walgit pr merge` clones target repos here when the user isn't already
//! inside the target's working tree. For huge repos this can eat disk;
//! `walgit cache list` / `clean` lets the user reclaim it explicitly.

use crate::config::load_repo_config;
use crate::error::{Result, WalGitError};
use crate::ui;
use std::path::{Path, PathBuf};

fn work_root() -> Result<PathBuf> {
    let home = dirs::home_dir()
        .ok_or_else(|| WalGitError::config("cannot find home directory".to_string()))?;
    Ok(home.join(".walgit").join("work"))
}

pub async fn list() -> Result<()> {
    let root = work_root()?;
    ui::header("auto-clone cache");
    println!("  {} {}", ui::label("path"), ui::dim(&root.display().to_string()));

    if !root.exists() {
        ui::info("empty — nothing cached yet");
        return Ok(());
    }

    let mut total_bytes: u64 = 0;
    let mut entries = vec![];
    for ent in std::fs::read_dir(&root)? {
        let ent = ent?;
        if !ent.file_type()?.is_dir() {
            continue;
        }
        let path = ent.path();
        let bytes = dir_size(&path);
        total_bytes += bytes;

        let repo_id = format!(
            "0x{}",
            path.file_name().unwrap_or_default().to_string_lossy()
        );
        let name = load_repo_config(&path.join(".walgit"))
            .ok()
            .map(|c| c.name)
            .unwrap_or_else(|| "<unknown>".to_string());
        entries.push((repo_id, name, bytes));
    }

    if entries.is_empty() {
        ui::info("empty — nothing cached yet");
        return Ok(());
    }

    entries.sort_by_key(|(_, _, b)| std::cmp::Reverse(*b));

    println!();
    for (id, name, bytes) in &entries {
        println!(
            "  {} {} {} {}",
            ui::dim("·"),
            ui::short_id(id),
            ui::highlight(name),
            ui::dim(&human_bytes(*bytes)),
        );
    }
    println!();
    ui::info(format!(
        "{} total across {} clone(s)",
        human_bytes(total_bytes),
        entries.len()
    ));
    println!();
    ui::info("free a clone:");
    println!(
        "    {} {}",
        ui::dim("$"),
        ui::highlight("walgit cache clean <repo_id>")
    );
    println!(
        "    {} {}",
        ui::dim("$"),
        ui::highlight("walgit cache clean --all"),
    );
    Ok(())
}

pub async fn clean(repo_id: Option<String>, all: bool) -> Result<()> {
    let root = work_root()?;
    if !root.exists() {
        ui::info("cache is empty");
        return Ok(());
    }

    if all {
        let mut total = 0u64;
        let mut count = 0;
        for ent in std::fs::read_dir(&root)? {
            let ent = ent?;
            if ent.file_type()?.is_dir() {
                total += dir_size(&ent.path());
                std::fs::remove_dir_all(ent.path())?;
                count += 1;
            }
        }
        ui::success(format!(
            "removed {} clone(s), freed {}",
            count,
            human_bytes(total)
        ));
        return Ok(());
    }

    let Some(id) = repo_id else {
        return Err(WalGitError::other(
            "specify <repo_id> or pass --all".to_string(),
        ));
    };
    let slug = id.trim_start_matches("0x");
    let dir = root.join(slug);
    if !dir.exists() {
        return Err(WalGitError::other(format!(
            "no cached clone for {}",
            id
        )));
    }
    let bytes = dir_size(&dir);
    std::fs::remove_dir_all(&dir)?;
    ui::success(format!(
        "removed {} (freed {})",
        ui::short_id(&format!("0x{}", slug)),
        human_bytes(bytes)
    ));
    Ok(())
}

fn dir_size(p: &Path) -> u64 {
    let mut total = 0u64;
    let Ok(entries) = std::fs::read_dir(p) else {
        return 0;
    };
    for e in entries.flatten() {
        let path = e.path();
        match e.file_type() {
            Ok(ft) if ft.is_dir() => total += dir_size(&path),
            Ok(ft) if ft.is_file() => {
                if let Ok(meta) = e.metadata() {
                    total += meta.len();
                }
            }
            _ => {}
        }
    }
    total
}

fn human_bytes(n: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let n = n as f64;
    if n < KB {
        format!("{} B", n as u64)
    } else if n < MB {
        format!("{:.1} KB", n / KB)
    } else if n < GB {
        format!("{:.2} MB", n / MB)
    } else {
        format!("{:.2} GB", n / GB)
    }
}
