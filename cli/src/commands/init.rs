// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

use crate::commands::CommandContext;
use crate::config::{LocalRepoConfig, save_repo_config};
use crate::error::Result;
use crate::{git, ui};
use std::path::PathBuf;

pub async fn run(
    name: String,
    description: Option<String>,
    private: bool,
    epochs: Option<u32>,
) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let walgit_dir = cwd.join(".walgit");

    if walgit_dir.exists() {
        ui::warn(format!(
            "{} already exists — overwriting Sui binding",
            walgit_dir.display()
        ));
    }

    if !cwd.join(".git").exists() {
        git::init(&cwd)?;
        ui::info("git init");
    }

    let ctx = CommandContext::load().await?;
    let net = ctx.config.active_network()?;
    let epochs = epochs.unwrap_or(net.walrus.epochs);
    let description = description.unwrap_or_default();
    let kp = ctx.keypair()?;

    let pb = ui::spinner(format!(
        "Creating repository '{}' on Sui ({} network)…",
        name, ctx.config.network
    ));

    let (repo_id, acl_id, gas) = ctx
        .sui
        .create_repository(&kp, &ctx.package_id, &name, &description, private)
        .await?;

    pb.finish_and_clear();

    let cfg = LocalRepoConfig {
        name: name.clone(),
        id: repo_id.clone(),
        acl_id: Some(acl_id.clone()),
        network: Some(ctx.config.network.clone()),
        private,
        epochs,
        pushes: vec![],
        forked_from: None,
        forked_from_acl_id: None,
    };
    save_repo_config(&walgit_dir, &cfg)?;
    write_gitignore(&cwd)?;

    ui::success(format!(
        "Created '{}' ({})",
        name,
        if private { "private" } else { "public" }
    ));
    ui::info(format!("Repository ID: {}", ui::short_id(&repo_id)));
    ui::info(format!("ACL ID:        {}", ui::short_id(&acl_id)));
    ui::info(format!("Gas:           {}", gas.display()));
    ui::info(format!(
        "Push URL:      walgit://{}/{}",
        ctx.active_address, name
    ));

    Ok(())
}

fn write_gitignore(cwd: &PathBuf) -> Result<()> {
    let p = cwd.join(".gitignore");
    let existing = std::fs::read_to_string(&p).unwrap_or_default();
    if !existing.contains(".walgit/") {
        let new = if existing.is_empty() {
            ".walgit/\n".to_string()
        } else {
            format!("{}\n.walgit/\n", existing.trim_end())
        };
        std::fs::write(&p, new)?;
    }
    Ok(())
}
