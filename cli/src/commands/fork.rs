// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

use crate::commands::CommandContext;
use crate::config::{LocalRepoConfig, save_repo_config};
use crate::error::{Result, WalGitError};
use crate::{git, ui};

pub async fn run(url: String, description: Option<String>) -> Result<()> {
    let (owner, name) = parse_walgit_uri(&url)?;

    let ctx = CommandContext::load().await?;
    let original = ctx
        .sui
        .get_repo_by_owner_name(&ctx.package_id, &owner, &name)
        .await?
        .ok_or_else(|| WalGitError::RepoNotFound(format!("{}/{}", owner, name)))?;

    if original.is_private {
        return Err(WalGitError::AccessDenied(
            "cannot fork a private repository".to_string(),
        ));
    }
    if original.owner == ctx.active_address {
        return Err(WalGitError::AccessDenied(
            "cannot fork your own repository".to_string(),
        ));
    }

    // Pre-check: contract enforces one-fork-per-address — detect duplicate before paying gas.
    if let Some((fork_id, fork_name)) = ctx
        .sui
        .find_fork_of(&ctx.package_id, &original.id, &ctx.active_address)
        .await?
    {
        return Err(WalGitError::other(format!(
            "you have already forked this repository as '{}' ({})",
            fork_name,
            ui::short_id(&fork_id)
        )));
    }

    let kp = ctx.keypair()?;
    let pb = ui::spinner(format!("Forking {}/{} on Sui…", owner, name));
    let (fork_id, fork_acl_id, gas) = ctx
        .sui
        .fork_repository(
            &kp,
            &ctx.package_id,
            &original.id,
            &name,
            &description.unwrap_or_else(|| original.description.clone()),
        )
        .await?;
    pb.finish_and_clear();

    // Copy each branch HEAD from the original by issuing one push_commit per branch.
    // (Walrus blob is reused — no re-upload.)
    if !original.branches.is_empty() {
        ui::info(format!("copying {} branch(es)…", original.branches.len()));
    }
    for (branch, commit_id) in &original.branches {
        let commit = ctx.sui.get_object(commit_id).await?;
        let blob_id = commit["blob_id"].as_str().unwrap_or("");
        let git_head = commit["git_head"].as_str().unwrap_or("");
        let message = format!("forked from {}/{}", owner, name);
        if blob_id.is_empty() || git_head.is_empty() {
            continue;
        }
        ctx.sui
            .push_commit(
                &kp,
                &ctx.package_id,
                &fork_id,
                &fork_acl_id,
                blob_id,
                git_head,
                None,
                &message,
                branch,
            )
            .await?;
    }

    let cwd = std::env::current_dir()?.join(&name);
    std::fs::create_dir_all(&cwd)?;
    git::init(&cwd)?;
    let remote_url = format!("walgit://{}/{}", ctx.active_address, name);
    git::set_remote(&cwd, "origin", &remote_url)?;
    let walgit_dir = cwd.join(".walgit");
    let cfg = LocalRepoConfig {
        name: name.clone(),
        id: fork_id.clone(),
        acl_id: Some(fork_acl_id.clone()),
        network: Some(ctx.config.network.clone()),
        private: false,
        epochs: ctx.config.active_network()?.walrus.epochs,
        pushes: vec![],
        forked_from: Some(original.id.clone()),
        forked_from_acl_id: Some(original.acl_id.clone()),
    };
    save_repo_config(&walgit_dir, &cfg)?;

    ui::success(format!("forked {}/{} → {}", owner, name, ui::short_id(&fork_id)));
    ui::info(format!("gas: {}", gas.display()));
    Ok(())
}

fn parse_walgit_uri(uri: &str) -> Result<(String, String)> {
    let path = uri
        .strip_prefix("walgit://")
        .ok_or_else(|| WalGitError::other(format!("invalid URI '{}'", uri)))?;
    let parts: Vec<&str> = path.splitn(2, '/').collect();
    if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
        return Err(WalGitError::other(format!(
            "invalid URI '{}' — expected walgit://<owner>/<repo>",
            uri
        )));
    }
    Ok((parts[0].to_string(), parts[1].to_string()))
}
