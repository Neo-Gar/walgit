// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

use crate::commands::CommandContext;
use crate::config::{LocalRepoConfig, save_repo_config};
use crate::error::{Result, WalGitError};
use crate::{git, ui};
use std::path::Path;

pub async fn run(
    name: String,
    here: bool,
    private: bool,
    epochs: Option<u32>,
) -> Result<()> {
    // Validate before any side effects — repo name flows into filesystem
    // paths (`cwd.join(&name)`) and URLs, so `../etc` or `--help` must die here.
    crate::validate::repo_name(&name)?;

    ui::banner();

    let cwd = std::env::current_dir()?;

    // Load context first so we can check the chain BEFORE any filesystem
    // changes — otherwise a duplicate-name abort would leave a dangling
    // empty directory or modified git state behind.
    let ctx = CommandContext::load().await?;

    // CLI-side early check for nicer UX — the contract enforces this too via
    // the shared Registry, but failing here saves the user from creating a
    // directory and a git repo only to abort on `create_repository`.
    if let Some(existing) = ctx
        .sui
        .get_repo_by_owner_name(&ctx.package_id, &ctx.active_address, &name)
        .await?
    {
        return Err(WalGitError::other(format!(
            "you already own a repository named '{}' on this network (id: {}).\n\
             Pick a different name, or use the existing repo (push url: walgit://{}/{}).",
            name,
            existing.id,
            ctx.active_address,
            name,
        )));
    }

    let repo_dir = if here {
        cwd.clone()
    } else {
        let target = cwd.join(&name);
        if target.exists() && std::fs::read_dir(&target)?.next().is_some() {
            return Err(WalGitError::other(format!(
                "{} already exists and is not empty. Use `walgit init {} --here` to initialise in place.",
                target.display(),
                name
            )));
        }
        std::fs::create_dir_all(&target)?;
        target
    };
    let walgit_dir = repo_dir.join(".walgit");

    if walgit_dir.exists() {
        ui::warn(format!(
            ".walgit already exists at {} — its Sui binding will be overwritten",
            walgit_dir.display()
        ));
    }

    ui::header("location");
    ui::info(format!(
        "working directory: {}",
        ui::highlight(&repo_dir.display().to_string())
    ));

    // ─── Step 1: ensure git is initialised in the target directory ────────────
    // .gitignore is committed BEFORE any on-chain registration so that the
    // soon-to-be-created .walgit/ directory cannot accidentally leak into
    // a future commit.
    ui::header("git");
    if !repo_dir.join(".git").exists() {
        ui::info(format!("no git repository in {}", repo_dir.display()));
        let proceed = ui::prompt_yes_no(
            "initialize a fresh git repository here?",
            true,
        )
        .map_err(|e| WalGitError::other(format!("prompt failed: {}", e)))?;
        if !proceed {
            return Err(WalGitError::other(
                "aborted: a git repository is required so that .walgit/ stays out of commits"
                    .to_string(),
            ));
        }
        git::init(&repo_dir)?;
        ui::success("git init");
    } else {
        ui::info("existing git repository detected");
    }

    // ─── Step 2: stage and commit .gitignore so .walgit/ is locked out ────────
    let added_walgit_rule = ensure_gitignore(&repo_dir)?;
    if added_walgit_rule {
        git::add(&repo_dir, &[".gitignore"])?;
        if git::has_staged_changes(&repo_dir) {
            match git::commit(&repo_dir, "chore: ignore .walgit/") {
                Ok(()) => ui::success("committed .gitignore (.walgit/ ignored)"),
                Err(e) => {
                    // Most common cause: missing user.name/user.email. Don't abort
                    // — fall through with a loud warning so the user can fix and rerun.
                    ui::warn(format!(
                        "could not commit .gitignore: {}. Configure git user.name/user.email and commit manually before pushing.",
                        e
                    ));
                }
            }
        }
    } else {
        ui::info(".gitignore already ignores .walgit/");
    }

    // ─── Step 3: register on Sui ──────────────────────────────────────────────
    ui::header("sui");
    let net = ctx.config.active_network()?;
    let epochs = epochs.unwrap_or(net.walrus.epochs);
    let kp = ctx.keypair()?;

    let pb = ui::spinner(format!(
        "creating repository '{}' on Sui ({})…",
        ui::highlight(&name),
        ctx.config.network
    ));

    let (repo_id, acl_id, gas) = ctx
        .sui
        .create_repository(&kp, &ctx.package_id, &ctx.registry_id, &name, private)
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

    // Wire up the `origin` git remote so the user can `git push` immediately
    // without having to remember the walgit:// URL. set_remote is idempotent:
    // it updates the URL if origin already exists (e.g. on re-init).
    let remote_url = format!("walgit://{}/{}", ctx.active_address, name);
    let remote_action = match git::get_remote_url(&repo_dir, "origin")? {
        Some(_) => "updated remote 'origin'",
        None => "added remote 'origin'",
    };
    git::set_remote(&repo_dir, "origin", &remote_url)?;

    ui::success(format!(
        "created {} ({})",
        ui::highlight(&name),
        if private { "private" } else { "public" }
    ));
    ui::success(format!("{} → {}", remote_action, ui::highlight(&remote_url)));

    ui::header("summary");
    println!(
        "  {} {}",
        ui::label("repository id "),
        &repo_id
    );
    println!(
        "  {} {}",
        ui::label("access control"),
        &acl_id
    );
    println!("  {} {}", ui::label("network       "), ctx.config.network);
    println!("  {} {}", ui::label("epochs        "), epochs);
    println!("  {} {}", ui::label("gas           "), gas.display());
    println!(
        "  {} {}",
        ui::label("push url      "),
        ui::highlight(&format!("walgit://{}/{}", ctx.active_address, name))
    );
    println!();
    ui::info("next steps:");
    if !here {
        println!(
            "    {} {}",
            ui::dim("$"),
            ui::highlight(&format!("cd {}", name))
        );
    }
    println!(
        "    {} {}    {}",
        ui::dim("$"),
        ui::highlight("git push -u origin main"),
        ui::dim("# remote 'origin' is already configured"),
    );
    println!();

    Ok(())
}

/// Ensure `.gitignore` contains a `.walgit/` rule. Returns true if a new rule
/// was appended (caller should stage + commit), false if the rule was already
/// present.
fn ensure_gitignore(repo_dir: &Path) -> Result<bool> {
    let p = repo_dir.join(".gitignore");
    let existing = std::fs::read_to_string(&p).unwrap_or_default();
    if existing
        .lines()
        .any(|l| l.trim() == ".walgit/" || l.trim() == ".walgit")
    {
        return Ok(false);
    }
    let new = if existing.is_empty() {
        "# WalGit — local metadata cache, never commit this\n.walgit/\n".to_string()
    } else {
        format!(
            "{}\n\n# WalGit — local metadata cache, never commit this\n.walgit/\n",
            existing.trim_end()
        )
    };
    std::fs::write(&p, new)?;
    Ok(true)
}
