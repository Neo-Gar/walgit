// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

use crate::commands::{CommandContext, find_repo, require_registered};
use crate::error::Result;
use crate::ui;

pub async fn run() -> Result<()> {
    let (_, _, local) = find_repo()?;
    require_registered(&local)?;

    let ctx = CommandContext::load().await?;
    let repo = ctx
        .sui
        .get_repo_by_id(&local.id, &ctx.active_address)
        .await?;

    ui::header("repository");
    println!("  {}", ui::highlight(&repo.name));
    if !repo.description.is_empty() {
        println!("  {}", ui::dim(&repo.description));
    }
    println!();
    println!("  {} {}", ui::label("id        "), &repo.id);
    println!("  {} {}", ui::label("owner     "), &repo.owner);
    println!(
        "  {} {}",
        ui::label("visibility"),
        if repo.is_private {
            ui::highlight("private")
        } else {
            "public".to_string()
        }
    );
    println!("  {} {}", ui::label("network   "), ctx.config.network);

    ui::header("branches");
    if repo.branches.is_empty() {
        ui::info("no branches yet — run `git push walgit://…`");
    } else {
        for (branch, commit_id) in &repo.branches {
            println!(
                "  {} {:<20} → {}",
                ui::dim("·"),
                ui::highlight(branch),
                commit_id
            );
        }
    }

    if !local.pushes.is_empty() {
        ui::header("local cache");
        ui::info(format!("recorded pushes: {}", local.pushes.len()));
    }

    Ok(())
}
