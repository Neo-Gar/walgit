// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

use crate::commands::{CommandContext, find_repo, require_registered};
use crate::error::Result;
use crate::ui;
use chrono::{DateTime, Utc};

pub async fn run(limit: usize) -> Result<()> {
    let (_, _, local) = find_repo()?;
    require_registered(&local)?;

    let ctx = CommandContext::load().await?;
    let repo = ctx.sui.get_repo_by_id(&local.id, &ctx.active_address).await?;

    let main_branch = repo
        .branches
        .keys()
        .find(|k| k.as_str() == "main")
        .cloned()
        .or_else(|| repo.branches.keys().next().cloned());

    let Some(branch) = main_branch else {
        ui::warn("no branches yet — run `git push walgit://…`");
        return Ok(());
    };

    let head_id = repo
        .branches
        .get(&branch)
        .expect("branch key must exist")
        .clone();
    let commits = ctx.sui.get_commit_chain(&head_id, limit).await?;

    println!();
    println!("  {} ({} commits)", branch, commits.len());
    println!();
    for c in commits {
        let dt = DateTime::<Utc>::from_timestamp_millis(c.timestamp as i64)
            .map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string())
            .unwrap_or_default();
        println!(
            "  {} {}",
            ui::short_hash(&c.git_head),
            ui::dim(&c.message),
        );
        println!(
            "    {} {} · {} · blob {}",
            ui::dim("·"),
            ui::short_id(&c.author),
            ui::dim(&dt),
            &c.blob_id[..12.min(c.blob_id.len())]
        );
    }
    Ok(())
}
