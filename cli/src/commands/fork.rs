// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

use crate::commands::CommandContext;
use crate::config::{LocalRepoConfig, save_repo_config};
use crate::error::{Result, WalGitError};
use crate::{git, ui};

pub async fn run(url: String, yes: bool) -> Result<()> {
    ui::banner();

    let (owner, name) = parse_walgit_uri(&url)?;

    ui::header("resolving");
    ui::info(format!(
        "looking up {}/{}",
        ui::highlight(&owner),
        ui::highlight(&name)
    ));

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

    // Pre-check the contract's one-fork-per-address rule so the user sees a
    // clear pointer to their existing fork instead of an opaque Move abort.
    if let Some((fork_id, fork_name)) = ctx
        .sui
        .find_fork_of(&ctx.package_id, &original.id, &ctx.active_address)
        .await?
    {
        return Err(WalGitError::other(format!(
            "you have already forked this repository as '{}' ({}).\n\
             Open it: cd {} && walgit status",
            fork_name, &fork_id, fork_name,
        )));
    }

    // ─── Preview + confirm ────────────────────────────────────────────────────
    ui::header("fork preview");
    println!(
        "  {} {}",
        ui::label("from   "),
        ui::highlight(&format!("{}/{}", owner, name))
    );
    println!(
        "  {} walgit://{}/{}",
        ui::label("to     "),
        ctx.active_address,
        name
    );
    println!(
        "  {} {} (reusing existing Walrus blobs — no re-upload)",
        ui::label("branches"),
        original.branches.len()
    );
    for b in original.branches.keys() {
        println!("    {} {}", ui::dim("·"), b);
    }
    println!("  {} {}", ui::label("network"), ctx.config.network);

    if !yes {
        let proceed = ui::prompt_yes_no("create this fork?", true)
            .map_err(|e| WalGitError::other(format!("prompt failed: {}", e)))?;
        if !proceed {
            return Err(WalGitError::other("fork aborted by user".to_string()));
        }
    }

    // ─── Fork on-chain ────────────────────────────────────────────────────────
    ui::header("sui");
    let kp = ctx.keypair()?;
    let pb = ui::spinner(format!("creating fork on {}…", ctx.config.network));
    let (fork_id, fork_acl_id, gas) = ctx
        .sui
        .fork_repository(&kp, &ctx.package_id, &ctx.registry_id, &original.id, &name)
        .await?;
    pb.finish_and_clear();
    ui::success(format!("fork created → {}", &fork_id));

    // ─── Copy branch heads (single push_commit per branch, blobs reused) ──────
    if !original.branches.is_empty() {
        ui::header("branches");
        for (branch, commit_id) in &original.branches {
            let commit = ctx.sui.get_object(commit_id).await?;
            let blob_id = commit["blob_id"].as_str().unwrap_or("");
            let git_head = commit["git_head"].as_str().unwrap_or("");
            if blob_id.is_empty() || git_head.is_empty() {
                ui::warn(format!("skipping branch '{}' (missing data)", branch));
                continue;
            }
            let message = format!("forked from {}/{}", owner, name);
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
            ui::success(format!(
                "{} → {}",
                ui::highlight(branch),
                ui::short_hash(git_head)
            ));
        }
    }

    // ─── Local working copy ───────────────────────────────────────────────────
    ui::header("local workspace");
    let cwd = std::env::current_dir()?.join(&name);
    std::fs::create_dir_all(&cwd)?;
    git::init(&cwd)?;
    let remote_url = format!("walgit://{}/{}", ctx.active_address, name);
    git::set_remote(&cwd, "origin", &remote_url)?;
    // Also save the upstream (original) repo as `upstream` so PRs back to
    // it are a single `git push upstream …` away.
    let upstream_url = format!("walgit://{}/{}", original.owner, original.name);
    git::set_remote(&cwd, "upstream", &upstream_url)?;

    let walgit_dir = cwd.join(".walgit");
    let cfg = LocalRepoConfig {
        name: name.clone(),
        id: fork_id.clone(),
        acl_id: Some(fork_acl_id.clone()),
        network: Some(ctx.config.network.clone()),
        private: false,
        epochs: ctx.config.active_network()?.walrus.epochs,
        live_snapshots: vec![],
        forked_from: Some(original.id.clone()),
        forked_from_acl_id: Some(original.acl_id.clone()),
    };
    save_repo_config(&walgit_dir, &cfg)?;
    ui::success(format!(
        "workspace at {}",
        ui::highlight(&cwd.display().to_string())
    ));
    ui::info(format!("origin   → {}", remote_url));
    ui::info(format!("upstream → {}", upstream_url));

    ui::header("summary");
    println!("  {} {}", ui::label("fork id "), &fork_id);
    println!("  {} {}", ui::label("acl id  "), &fork_acl_id);
    println!("  {} {}", ui::label("gas     "), gas.display());
    println!();
    ui::info("next steps:");
    println!(
        "    {} {}",
        ui::dim("$"),
        ui::highlight(&format!("cd {}", name))
    );
    println!(
        "    {} {} {}",
        ui::dim("$"),
        ui::highlight("git pull origin main"),
        ui::dim("# fetch fork content"),
    );
    println!(
        "    {} {} {}",
        ui::dim("$"),
        ui::highlight("# … make changes, commit … "),
        ui::dim(""),
    );
    println!(
        "    {} {} {}",
        ui::dim("$"),
        ui::highlight("git push origin main"),
        ui::dim("# push to your fork"),
    );
    println!(
        "    {} {} {}",
        ui::dim("$"),
        ui::highlight("walgit pr create"),
        ui::dim("# open PR back to upstream"),
    );
    println!();

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
    // Strict validation: parts[1] later becomes a path segment in
    // `cwd.join(&name)` and a git ref. Reject traversal + flag-like inputs.
    crate::validate::sui_address(parts[0])?;
    crate::validate::repo_name(parts[1])?;
    Ok((parts[0].to_string(), parts[1].to_string()))
}
