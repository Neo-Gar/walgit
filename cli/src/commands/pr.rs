// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

use crate::commands::{CommandContext, find_repo, require_registered};
use crate::error::{Result, WalGitError};
use crate::sui::types::PullRequestRecord;
use crate::{git, ui};
use chrono::{DateTime, Utc};
use console::style;

// ─── create ───────────────────────────────────────────────────────────────────

pub async fn create(
    source_branch: Option<String>,
    target_branch: Option<String>,
    yes: bool,
) -> Result<()> {
    ui::banner();

    let (repo_dir, _walgit_dir, local) = find_repo()?;
    require_registered(&local)?;
    let source_acl = local
        .acl_id
        .as_deref()
        .ok_or_else(|| WalGitError::config("ACL id missing".to_string()))?;

    let ctx = CommandContext::load().await?;
    let kp = ctx.keypair()?;

    // ─── Resolve source and target repo ───────────────────────────────────────
    // If this is a fork, default to PR'ing into the upstream (forked_from).
    // Otherwise we PR into the same repo (different branch).
    let (target_repo_id, target_acl_id, target_label, target_is_upstream, target_branches): (
        String,
        String,
        String,
        bool,
        std::collections::HashMap<String, String>,
    ) = if let Some(upstream_id) = local.forked_from.as_deref() {
        let upstream = ctx
            .sui
            .get_repo_by_id(upstream_id, &ctx.active_address)
            .await?;
        let acl = local
            .forked_from_acl_id
            .clone()
            .unwrap_or_else(|| upstream.acl_id.clone());
        let label = format!("{}/{}", upstream.owner, upstream.name);
        let branches = upstream.branches.clone();
        (upstream.id.clone(), acl, label, true, branches)
    } else {
        // Same-repo PR (branch → branch within own repo).
        let repo = ctx
            .sui
            .get_repo_by_id(&local.id, &ctx.active_address)
            .await?;
        let label = format!("{} (self)", repo.name);
        (
            repo.id.clone(),
            source_acl.to_string(),
            label,
            false,
            repo.branches.clone(),
        )
    };

    // ─── Resolve source branch (default = current HEAD branch) ────────────────
    let source_branch = match source_branch {
        Some(s) => s,
        None => current_branch(&repo_dir)?,
    };
    let source_tip = git::rev_parse(&repo_dir, &source_branch)?;

    // ─── Resolve target branch (default = main / sole branch on target) ───────
    let target_branch = match target_branch {
        Some(t) => t,
        None => default_target_branch(&target_branches),
    };

    // ─── Compute exclude tips: every commit known to the TARGET ───────────────
    // We pack only objects not reachable from any of these — so the PR blob
    // contains just the source-author's contribution, not the entire history.
    let mut exclude_tips: Vec<String> = Vec::new();
    for commit_id in target_branches.values() {
        if let Ok(obj) = ctx.sui.get_object(commit_id).await {
            if let Some(head) = obj["git_head"].as_str() {
                if !head.is_empty() && git::object_exists(&repo_dir, head) {
                    exclude_tips.push(head.to_string());
                }
            }
        }
    }

    // ─── Preview ──────────────────────────────────────────────────────────────
    ui::header("pull request preview");
    println!(
        "  {} {}  → {}",
        ui::label("source"),
        ui::highlight(&format!(
            "walgit://{}/{}#{}",
            ctx.active_address, local.name, source_branch
        )),
        ui::highlight(&format!("{}#{}", target_label, target_branch)),
    );
    println!(
        "  {} {}",
        ui::label("source tip"),
        ui::short_hash(&source_tip)
    );
    println!(
        "  {} {} commit(s) already on upstream — will not re-upload",
        ui::label("already there"),
        exclude_tips.len()
    );

    // ─── Pack incrementally ───────────────────────────────────────────────────
    ui::header("packing");
    let (pack, new_commit_count) =
        git::pack_objects_incremental(&repo_dir, &source_tip, &exclude_tips)?;

    if new_commit_count == 0 || pack.is_empty() {
        return Err(WalGitError::other(format!(
            "no new commits to send — {} already contains {}",
            target_label,
            ui::short_hash(&source_tip)
        )));
    }

    ui::success(format!(
        "packed {} new commit(s) — {}",
        new_commit_count,
        ui::fmt_bytes(pack.len())
    ));
    if target_is_upstream && !exclude_tips.is_empty() {
        ui::info(format!(
            "you pay storage only for these {} bytes; the {} parent commit(s) stay on the upstream's existing blobs",
            ui::fmt_bytes(pack.len()),
            exclude_tips.len()
        ));
    }

    if !yes {
        let proceed = ui::prompt_yes_no("open this pull request?", true)
            .map_err(|e| WalGitError::other(format!("prompt failed: {}", e)))?;
        if !proceed {
            return Err(WalGitError::other("aborted by user".to_string()));
        }
    }

    // ─── Encrypt + upload ─────────────────────────────────────────────────────
    // Encryption follows the TARGET's privacy: a PR to a private repo must be
    // encrypted under the target's key so the maintainer can decrypt.
    let target_repo = ctx
        .sui
        .get_repo_by_id(&target_repo_id, &ctx.active_address)
        .await?;
    let bytes = if target_repo.is_private {
        let seal = ctx.seal_client()?;
        seal.encrypt(&ctx.package_id, &target_repo.id, &pack)
            .await?
    } else {
        pack
    };

    ui::header("walrus");
    let upload = ctx.walrus.upload(bytes, local.epochs).await?;

    // ─── Create PR on chain ───────────────────────────────────────────────────
    ui::header("sui");
    let pb = ui::spinner("recording pull request…");
    let (pr_id, gas) = ctx
        .sui
        .create_pull_request(
            &kp,
            &ctx.package_id,
            &target_repo_id,
            &target_acl_id,
            &source_branch,
            &target_branch,
            &upload.blob_id,
            &source_tip,
        )
        .await?;
    pb.finish_and_clear();
    ui::success(format!("PR opened — {}", &pr_id));

    ui::header("summary");
    println!("  {} {}", ui::label("pr id   "), &pr_id);
    println!(
        "  {} {} → {}",
        ui::label("flow    "),
        source_branch,
        format!("{}#{}", target_label, target_branch)
    );
    println!("  {} {}", ui::label("commits "), new_commit_count);
    println!("  {} {}", ui::label("gas     "), gas.display());
    println!();
    ui::info(format!(
        "track it: {} {}",
        ui::dim("$"),
        ui::highlight(&format!("walgit pr show {}", pr_id))
    ));
    println!();
    Ok(())
}

fn current_branch(repo_dir: &std::path::Path) -> Result<String> {
    let out = std::process::Command::new("git")
        .args(["symbolic-ref", "--short", "HEAD"])
        .current_dir(repo_dir)
        .output()
        .map_err(|e| WalGitError::git(format!("git symbolic-ref failed: {}", e)))?;
    if !out.status.success() {
        return Err(WalGitError::git(
            "could not determine current branch (detached HEAD?)".to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn default_target_branch(branches: &std::collections::HashMap<String, String>) -> String {
    if branches.contains_key("main") {
        "main".to_string()
    } else if branches.contains_key("master") {
        "master".to_string()
    } else if branches.len() == 1 {
        branches.keys().next().cloned().unwrap_or_default()
    } else {
        "main".to_string()
    }
}

// ─── list ─────────────────────────────────────────────────────────────────────

pub async fn list(mine: bool) -> Result<()> {
    let ctx = CommandContext::load().await?;

    let (heading, prs) = if mine {
        let prs = ctx
            .sui
            .list_pull_requests_by_author(&ctx.package_id, &ctx.active_address)
            .await?;
        (format!("my pull requests ({})", prs.len()), prs)
    } else {
        let (_, _, local) = find_repo()?;
        require_registered(&local)?;
        let prs = ctx
            .sui
            .list_pull_requests(&ctx.package_id, &local.id)
            .await?;
        (
            format!("pull requests · {} ({})", local.name, prs.len()),
            prs,
        )
    };

    ui::header(&heading);
    if prs.is_empty() {
        ui::info("nothing here yet");
        return Ok(());
    }
    for pr in prs {
        print_pr_row(&pr, mine);
    }
    Ok(())
}

fn print_pr_row(pr: &PullRequestRecord, include_repo: bool) {
    let status = match pr.status {
        1 => style(format!("{:<8}", pr.status_label()))
            .green()
            .bold()
            .to_string(),
        2 => style(format!("{:<8}", pr.status_label())).red().to_string(),
        _ => style(format!("{:<8}", pr.status_label()))
            .yellow()
            .to_string(),
    };
    let prefix = if include_repo {
        format!(" {} ", style(ui::short_id(&pr.repo_id)).dim())
    } else {
        String::new()
    };
    println!(
        "  {} {}{}{} → {}  {} {}",
        style(format!("#{}", pr.number)).cyan().bold(),
        status,
        prefix,
        ui::highlight(&pr.source_branch),
        ui::highlight(&pr.target_branch),
        ui::dim("by"),
        ui::short_id(&pr.author),
    );
    println!("      {} {}", ui::dim("pr_id"), ui::short_id(&pr.id));
}

// ─── show ─────────────────────────────────────────────────────────────────────

pub async fn show(pr_id: String) -> Result<()> {
    let ctx = CommandContext::load().await?;
    let pr = ctx.sui.get_pull_request(&pr_id).await?;

    let status_color = match pr.status {
        1 => style(pr.status_label()).green().bold(),
        2 => style(pr.status_label()).red(),
        _ => style(pr.status_label()).yellow(),
    };
    ui::header(&format!("PR #{}", pr.number));
    println!("  {} {}", ui::label("status   "), status_color);
    println!("  {} {}", ui::label("repo     "), &pr.repo_id);
    println!(
        "  {} {} → {}",
        ui::label("flow     "),
        ui::highlight(&pr.source_branch),
        ui::highlight(&pr.target_branch)
    );
    println!("  {} {}", ui::label("author   "), &pr.author);
    let dt = DateTime::<Utc>::from_timestamp_millis(pr.created_at as i64)
        .map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_default();
    println!("  {} {}", ui::label("created  "), ui::dim(&dt));
    println!(
        "  {} {}",
        ui::label("approved "),
        if pr.approved {
            style("yes").green().bold().to_string()
        } else {
            style("no").yellow().to_string()
        }
    );
    if let Some(a) = &pr.approved_by {
        println!("  {} {}", ui::label("by       "), ui::short_id(a));
    }
    if !pr.source_blob_id.is_empty() {
        println!("  {} {}", ui::label("blob     "), pr.source_blob_id);
    }
    if !pr.source_git_head.is_empty() {
        println!("  {} {}", ui::label("source tip"), pr.source_git_head);
    }
    if let Some(b) = &pr.merge_commit_blob_id {
        println!("  {} {}", ui::label("merge    "), b);
    }
    ui::header("actions");
    println!(
        "    {} {} {}",
        ui::dim("$"),
        ui::highlight(&format!("walgit pr approve {}", pr.id)),
        ui::dim("# repo owners + writers only"),
    );
    println!(
        "    {} {} {}",
        ui::dim("$"),
        ui::highlight(&format!("walgit pr merge {}", pr.id)),
        ui::dim("# requires prior approve"),
    );
    println!(
        "    {} {} {}",
        ui::dim("$"),
        ui::highlight(&format!("walgit pr close {}", pr.id)),
        ui::dim("# owner / author"),
    );
    let _ = ctx; // keep ctx alive for any future expansion
    Ok(())
}

// ─── approve / merge / close ──────────────────────────────────────────────────

pub async fn approve(pr_id: String) -> Result<()> {
    let ctx = CommandContext::load().await?;
    // The PR knows which repo it lives on — don't trust the local dir, which
    // could be a fork pointing to a PR on the upstream.
    let target = resolve_pr_target(&ctx, &pr_id).await?;
    let kp = ctx.keypair()?;
    let gas = ctx
        .sui
        .approve_pull_request(
            &kp,
            &ctx.package_id,
            &pr_id,
            &target.repo_id,
            &target.acl_id,
        )
        .await?;
    ui::success(format!("approved PR {}", ui::short_id(&pr_id)));
    ui::info(format!("gas: {}", gas.display()));
    Ok(())
}

pub async fn merge(pr_id: String) -> Result<()> {
    // Merge has to touch a working tree (git unpack → merge → pack). Two
    // possibilities:
    //   1. CWD is already inside the target repo's local clone → reuse it.
    //   2. CWD is elsewhere (fork, unrelated dir) → auto-clone the target
    //      into `~/.walgit/work/<repo_id>/` so the maintainer doesn't have to
    //      remember where they originally `walgit init`-ed.
    let ctx = CommandContext::load().await?;
    let target = resolve_pr_target(&ctx, &pr_id).await?;
    let (repo_dir, epochs) = resolve_target_workdir(&ctx, &target).await?;
    let acl_id = target.acl_id.as_str();
    let kp = ctx.keypair()?;

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
    let pack = if target.is_private {
        let seal = ctx.seal_client()?;
        let v = ctx.sui.get_initial_shared_version(acl_id).await?;
        seal.decrypt(
            &ctx.package_id,
            &target.repo_id,
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
    // Prefer the PR's recorded source tip — after `unpack_objects` the new
    // commits are dangling (no ref pointing at them), so `git log --all` would
    // miss them. The chain knows the SHA explicitly.
    let source_tip = if !pr.source_git_head.is_empty() {
        pr.source_git_head.clone()
    } else {
        git::find_foreign_tip(&repo_dir, &pr.target_branch)?
    };
    git::merge_fast_forward(&repo_dir, &pr.target_branch, &source_tip)?;

    let merge_pack = git::pack_objects(&repo_dir)?;
    let upload = ctx.walrus.upload(merge_pack, epochs).await?;
    let merge_head = git::get_head_commit(&repo_dir)?;

    let parent = ctx
        .sui
        .get_repo_branch_head(&target.repo_id, &pr.target_branch)
        .await?;
    let (_commit_id, _gas) = ctx
        .sui
        .push_commit(
            &kp,
            &ctx.package_id,
            &target.repo_id,
            acl_id,
            &upload.blob_id,
            &merge_head,
            parent.as_deref(),
            &format!("merge PR #{}", pr.number),
            &pr.target_branch,
        )
        .await?;

    let was_auto_cloned = repo_dir
        .strip_prefix(
            dirs::home_dir()
                .unwrap_or_default()
                .join(".walgit")
                .join("work"),
        )
        .is_ok();

    let gas = ctx
        .sui
        .merge_pull_request(
            &kp,
            &ctx.package_id,
            &pr_id,
            &target.repo_id,
            acl_id,
            &upload.blob_id,
        )
        .await?;
    ui::success(format!("merged PR #{}", pr.number));
    ui::info(format!("gas: {}", gas.display()));
    if was_auto_cloned {
        let slug = target.repo_id.trim_start_matches("0x");
        ui::info(format!(
            "auto-clone left at ~/.walgit/work/{} — `walgit cache clean {}` to free it",
            slug, target.repo_id
        ));
    }
    Ok(())
}

pub async fn close(pr_id: String) -> Result<()> {
    // A PR's author can close it from anywhere — they may not even have the
    // target repo cloned. So resolve target purely from the PR object,
    // without consulting any local `.walgit/`.
    let ctx = CommandContext::load().await?;
    let target = resolve_pr_target(&ctx, &pr_id).await?;
    let kp = ctx.keypair()?;
    let gas = ctx
        .sui
        .close_pull_request(
            &kp,
            &ctx.package_id,
            &pr_id,
            &target.repo_id,
            &target.acl_id,
        )
        .await?;
    ui::success(format!("closed PR {}", ui::short_id(&pr_id)));
    ui::info(format!("gas: {}", gas.display()));
    Ok(())
}

/// Look up the target repository of a PR purely from on-chain state. Returns
/// `repo_id`, `acl_id`, `owner`, `name`, `is_private` so callers don't have
/// to make a second round-trip.
struct PrTarget {
    repo_id: String,
    acl_id: String,
    owner: String,
    name: String,
    is_private: bool,
}

/// Find or create a local working directory for `target`. Reuses the current
/// `.walgit/` if it already points at this repo, otherwise clones into
/// `~/.walgit/work/<repo_id>/` via `git clone walgit://owner/name`. Returns
/// the path plus the `epochs` setting to use for the next push.
async fn resolve_target_workdir(
    _ctx: &CommandContext,
    target: &PrTarget,
) -> Result<(std::path::PathBuf, u32)> {
    if let Ok((dir, _, local)) = find_repo() {
        if local.id == target.repo_id {
            ui::info(format!(
                "using current clone at {}",
                ui::highlight(&dir.display().to_string())
            ));
            return Ok((dir, local.epochs));
        }
    }

    let home = dirs::home_dir()
        .ok_or_else(|| WalGitError::config("cannot find home directory".to_string()))?;
    let work_root = home.join(".walgit").join("work");
    std::fs::create_dir_all(&work_root)?;
    let id_slug = target.repo_id.trim_start_matches("0x");
    let work_dir = work_root.join(id_slug);

    if work_dir.join(".git").exists() {
        ui::info(format!(
            "reusing cached clone at {}",
            ui::highlight(&work_dir.display().to_string())
        ));
        // Pull latest so we merge on top of current upstream HEAD.
        let _ = std::process::Command::new("git")
            .args(["fetch", "origin"])
            .current_dir(&work_dir)
            .output();
    } else {
        let url = format!("walgit://{}/{}", target.owner, target.name);
        ui::info(format!(
            "cloning {} into {}…",
            ui::highlight(&url),
            ui::dim(&work_dir.display().to_string())
        ));
        let out = std::process::Command::new("git")
            .args(["clone", "--quiet", &url, work_dir.to_str().unwrap_or("")])
            .output()
            .map_err(|e| WalGitError::git(format!("git clone failed: {}", e)))?;
        if !out.status.success() {
            return Err(WalGitError::git(format!(
                "git clone walgit://{}/{} failed: {}",
                target.owner,
                target.name,
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
    }

    // Load epochs from the freshly populated `.walgit/config.toml` — the
    // remote helper writes it during clone.
    let epochs = crate::config::load_repo_config(&work_dir.join(".walgit"))
        .map(|c| c.epochs)
        .unwrap_or(1);

    Ok((work_dir, epochs))
}

async fn resolve_pr_target(ctx: &CommandContext, pr_id: &str) -> Result<PrTarget> {
    let pr = ctx.sui.get_pull_request(pr_id).await?;
    let repo = ctx
        .sui
        .get_repo_by_id(&pr.repo_id, &ctx.active_address)
        .await?;
    Ok(PrTarget {
        repo_id: repo.id,
        acl_id: repo.acl_id,
        owner: repo.owner,
        name: repo.name,
        is_private: repo.is_private,
    })
}
