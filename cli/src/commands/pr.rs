// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

use crate::commands::{CommandContext, find_repo, require_registered};
use crate::error::{Result, WalGitError};
use crate::{git, ui};

pub async fn create(source_branch: String, target_branch: String) -> Result<()> {
    let (repo_dir, _walgit_dir, local) = find_repo()?;
    require_registered(&local)?;
    let acl_id = local
        .acl_id
        .as_deref()
        .ok_or_else(|| WalGitError::config("ACL id missing".to_string()))?;
    let ctx = CommandContext::load().await?;

    let kp = ctx.keypair()?;

    ui::info(format!("packing source branch '{}'…", source_branch));
    let _ = git::rev_parse(&repo_dir, &source_branch)?;
    let pack = git::pack_objects(&repo_dir)?;
    let bytes = if local.private {
        let seal = ctx.seal_client()?;
        seal.encrypt(&ctx.package_id, &local.id, &pack).await?
    } else {
        pack
    };

    let upload = ctx.walrus.upload(bytes, local.epochs).await?;
    let (pr_id, gas) = ctx
        .sui
        .create_pull_request(
            &kp,
            &ctx.package_id,
            &local.id,
            acl_id,
            &source_branch,
            &target_branch,
            &upload.blob_id,
        )
        .await?;
    ui::success(format!("opened PR {}", ui::short_id(&pr_id)));
    ui::info(format!("gas: {}", gas.display()));
    Ok(())
}

pub async fn list() -> Result<()> {
    let (_, _, local) = find_repo()?;
    require_registered(&local)?;
    let ctx = CommandContext::load().await?;
    let prs = ctx.sui.list_pull_requests(&ctx.package_id, &local.id).await?;
    if prs.is_empty() {
        ui::info("no pull requests");
        return Ok(());
    }
    println!();
    for pr in prs {
        println!(
            "  #{:<3} {:<8} {} → {}  by {}",
            pr.number,
            pr.status_label(),
            pr.source_branch,
            pr.target_branch,
            ui::short_id(&pr.author),
        );
    }
    Ok(())
}

pub async fn approve(pr_id: String) -> Result<()> {
    let (_, _, local) = find_repo()?;
    require_registered(&local)?;
    let acl_id = local
        .acl_id
        .as_deref()
        .ok_or_else(|| WalGitError::config("ACL id missing".to_string()))?;
    let ctx = CommandContext::load().await?;
    let kp = ctx.keypair()?;
    let gas = ctx
        .sui
        .approve_pull_request(&kp, &ctx.package_id, &pr_id, &local.id, acl_id)
        .await?;
    ui::success(format!("approved PR {}", ui::short_id(&pr_id)));
    ui::info(format!("gas: {}", gas.display()));
    Ok(())
}

pub async fn merge(pr_id: String) -> Result<()> {
    let (repo_dir, _walgit_dir, local) = find_repo()?;
    require_registered(&local)?;
    let acl_id = local
        .acl_id
        .as_deref()
        .ok_or_else(|| WalGitError::config("ACL id missing".to_string()))?;
    let ctx = CommandContext::load().await?;
    let kp = ctx.keypair()?;

    // Fetch PR, download source blob, unpack into local repo, ff-merge, push,
    // then record merge on-chain.
    let pr = ctx.sui.get_pull_request(&pr_id).await?;
    if pr.status != 0 {
        return Err(WalGitError::other(format!(
            "PR is already {}",
            pr.status_label()
        )));
    }
    if !pr.approved {
        return Err(WalGitError::other("PR is not approved yet".to_string()));
    }

    let raw = ctx.walrus.download(&pr.source_blob_id).await?;
    let pack = if local.private {
        let seal = ctx.seal_client()?;
        let v = ctx.sui.get_initial_shared_version(acl_id).await?;
        seal.decrypt(
            &ctx.package_id,
            &local.id,
            acl_id,
            v,
            &ctx.active_address,
            ctx.config.wallet_path.as_deref(),
            &raw,
        )
        .await?
    } else {
        raw
    };

    git::unpack_objects(&repo_dir, &pack)?;
    let source_tip = git::find_foreign_tip(&repo_dir, &pr.target_branch)?;
    git::merge_fast_forward(&repo_dir, &pr.target_branch, &source_tip)?;

    let merge_pack = git::pack_objects(&repo_dir)?;
    let upload = ctx.walrus.upload(merge_pack, local.epochs).await?;
    let merge_head = git::get_head_commit(&repo_dir)?;

    let parent = ctx
        .sui
        .get_repo_branch_head(&local.id, &pr.target_branch)
        .await?;
    let (_commit_id, _gas) = ctx
        .sui
        .push_commit(
            &kp,
            &ctx.package_id,
            &local.id,
            acl_id,
            &upload.blob_id,
            &merge_head,
            parent.as_deref(),
            &format!("merge PR #{}", pr.number),
            &pr.target_branch,
        )
        .await?;

    let gas = ctx
        .sui
        .merge_pull_request(&kp, &ctx.package_id, &pr_id, &local.id, acl_id, &upload.blob_id)
        .await?;
    ui::success(format!("merged PR #{}", pr.number));
    ui::info(format!("gas: {}", gas.display()));
    Ok(())
}

pub async fn close(pr_id: String) -> Result<()> {
    let (_, _, local) = find_repo()?;
    require_registered(&local)?;
    let acl_id = local
        .acl_id
        .as_deref()
        .ok_or_else(|| WalGitError::config("ACL id missing".to_string()))?;
    let ctx = CommandContext::load().await?;
    let kp = ctx.keypair()?;
    let gas = ctx
        .sui
        .close_pull_request(&kp, &ctx.package_id, &pr_id, &local.id, acl_id)
        .await?;
    ui::success(format!("closed PR {}", ui::short_id(&pr_id)));
    ui::info(format!("gas: {}", gas.display()));
    Ok(())
}

pub async fn status(pr_id: String) -> Result<()> {
    let ctx = CommandContext::load().await?;
    let pr = ctx.sui.get_pull_request(&pr_id).await?;
    println!();
    println!("  PR #{}  ({})", pr.number, pr.status_label());
    ui::info(format!("source: {}", pr.source_branch));
    ui::info(format!("target: {}", pr.target_branch));
    ui::info(format!("author: {}", ui::short_id(&pr.author)));
    ui::info(format!("approved: {}", pr.approved));
    if let Some(a) = &pr.approved_by {
        ui::info(format!("approved by: {}", ui::short_id(a)));
    }
    if let Some(b) = &pr.merge_commit_blob_id {
        ui::info(format!("merge blob: {}", b));
    }
    Ok(())
}
