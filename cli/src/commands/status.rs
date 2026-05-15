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

    println!();
    println!("  {}", repo.name);
    if !repo.description.is_empty() {
        println!("  {}", ui::dim(&repo.description));
    }
    println!();
    ui::info(format!("repository: {}", ui::short_id(&repo.id)));
    ui::info(format!("owner:      {}", ui::short_id(&repo.owner)));
    ui::info(format!(
        "visibility: {}",
        if repo.is_private { "private" } else { "public" }
    ));
    ui::info(format!("network:    {}", ctx.config.network));
    ui::info(format!("branches:   {}", repo.branches.len()));
    for (branch, commit_id) in &repo.branches {
        println!(
            "    {} {} → {}",
            ui::dim("·"),
            branch,
            ui::short_id(commit_id)
        );
    }

    if !local.pushes.is_empty() {
        println!();
        ui::info(format!("local pushes: {}", local.pushes.len()));
    }

    Ok(())
}
