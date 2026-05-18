// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

//! git-remote-walgit — Git remote helper for `walgit://` URIs.
//!
//! Invoked by git as: git-remote-walgit <remote-name> walgit://<owner>/<repo>
//! Speaks the newline-delimited git remote helper protocol on stdin/stdout;
//! progress and errors go to stderr.

use anyhow::{Context, Result, anyhow, bail};
use std::io::{BufRead, Write};
use std::path::PathBuf;
use walgit::config::{LocalRepoConfig, load_repo_config, save_repo_config};
use walgit::sui::keystore;
use walgit::ui;

fn print_error(e: &anyhow::Error) {
    let msg = format!("{:#}", e);
    eprintln!();
    if msg.starts_with("access denied:") || msg.starts_with("Access denied:") {
        let (first, rest) = msg.split_once('\n').unwrap_or((&msg, ""));
        eprintln!(
            "  {} {}",
            console::style("✗").red().bold(),
            console::style(first).red().bold()
        );
        for line in rest.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if line.starts_with("walgit ") {
                eprintln!("    {}", console::style(line).cyan());
            } else {
                eprintln!("  {}", console::style(line).dim());
            }
        }
    } else {
        eprintln!(
            "  {} {}",
            console::style("✗").red().bold(),
            console::style(msg).red()
        );
    }
    eprintln!();
}

fn main() {
    let rt = tokio::runtime::Runtime::new().expect("failed to start tokio runtime");
    if let Err(e) = rt.block_on(run()) {
        print_error(&e);
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        bail!("Usage: git-remote-walgit <remote-name> walgit://<owner>/<repo>");
    }
    let url = &args[2];
    let (owner, repo_name) = parse_uri(url)?;

    // During `git clone`, GIT_DIR points at .../<cloned>/.git so .walgit/
    // should live next to it, not in the parent directory.
    let repo_dir: PathBuf = if let Ok(git_dir) = std::env::var("GIT_DIR") {
        let gp = PathBuf::from(&git_dir);
        if gp.is_absolute() {
            gp.parent().map(|p| p.to_path_buf()).unwrap_or(gp)
        } else {
            std::env::current_dir()?
        }
    } else {
        std::env::current_dir()?
    };

    let config = walgit::config::load()?;
    let package_id = config
        .package_id()
        .map_err(|e| anyhow!("{}", e))?
        .to_string();
    let net = config
        .active_network()
        .map_err(|e| anyhow!("{}", e))?
        .clone();
    let sui = walgit::SuiClient::new(net.sui.graphql_url.clone())?;
    let walrus = walgit::WalrusClient::new(
        net.walrus.publisher_url.clone(),
        net.walrus.aggregator_url.clone(),
    );

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut stdin = stdin.lock();
    let mut stdout = stdout.lock();

    let mut sui_repo: Option<walgit::sui::types::RepoRecord> = None;

    let mut buf = String::new();
    loop {
        buf.clear();
        if stdin.read_line(&mut buf)? == 0 {
            break;
        }
        let cmd = buf
            .trim_end_matches('\n')
            .trim_end_matches('\r')
            .to_string();
        if cmd.is_empty() {
            break;
        }

        match cmd.as_str() {
            "capabilities" => {
                writeln!(stdout, "fetch")?;
                writeln!(stdout, "push")?;
                writeln!(stdout)?;
                stdout.flush()?;
            }

            "list" | "list for-push" => {
                if sui_repo.is_none() {
                    ui::eheader("resolving");
                    ui::einfo(format!(
                        "looking up {}/{} on Sui",
                        ui::highlight(&owner),
                        ui::highlight(&repo_name)
                    ));

                    let walgit_dir = repo_dir.join(".walgit");
                    let local_id = if walgit_dir.exists() {
                        load_repo_config(&walgit_dir).ok().and_then(|c| {
                            if c.id.is_empty() || c.id == "pending" {
                                None
                            } else {
                                Some(c.id)
                            }
                        })
                    } else {
                        None
                    };

                    let repo = if let Some(id) = local_id {
                        sui.get_repo_by_id(&id, &owner).await?
                    } else {
                        sui.get_repo_by_owner_name(&package_id, &owner, &repo_name)
                            .await?
                            .with_context(|| {
                                format!("repository '{}/{}' not found on Sui", owner, repo_name)
                            })?
                    };

                    if !walgit_dir.exists() {
                        let acl_id = if repo.acl_id.is_empty() {
                            None
                        } else {
                            Some(repo.acl_id.clone())
                        };
                        save_repo_config(
                            &walgit_dir,
                            &LocalRepoConfig {
                                name: repo.name.clone(),
                                id: repo.id.clone(),
                                acl_id,
                                network: Some(config.network.clone()),
                                private: repo.is_private,
                                epochs: net.walrus.epochs,
                                ..Default::default()
                            },
                        )?;
                    }

                    sui_repo = Some(repo);
                }
                let repo = sui_repo.as_ref().unwrap();

                if repo.is_private && !repo.acl_id.is_empty() {
                    let active = keystore::read_active_address(config.wallet_path.as_deref()).ok();
                    if let Some(active) = active {
                        if active != repo.owner {
                            match sui.get_access_control(&repo.acl_id).await {
                                Ok(acl)
                                    if !acl.allowed_readers.contains(&active)
                                        && !acl.allowed_writers.contains(&active) =>
                                {
                                    bail!(
                                        "access denied: {} is not authorised to access this private repository.\n\
                                         Ask the owner to run: walgit access grant read {}",
                                        &active[..10.min(active.len())],
                                        active
                                    );
                                }
                                _ => {}
                            }
                        }
                    }
                }

                let mut default_branch: Option<String> = None;
                for (branch, commit_id) in &repo.branches {
                    match sui.get_object(commit_id).await {
                        Ok(obj) => {
                            let git_head = obj["git_head"].as_str().unwrap_or("");
                            if !git_head.is_empty() {
                                writeln!(stdout, "{} refs/heads/{}", git_head, branch)?;
                                if default_branch.is_none()
                                    || branch == "main"
                                    || branch == "master"
                                {
                                    default_branch = Some(branch.clone());
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!(
                                "  {} could not fetch commit {}: {}",
                                console::style("!").yellow().bold(),
                                commit_id,
                                e
                            );
                        }
                    }
                }

                if let Some(b) = &default_branch {
                    writeln!(stdout, "@refs/heads/{} HEAD", b)?;
                }
                writeln!(stdout)?;
                stdout.flush()?;
            }

            _ if cmd.starts_with("fetch ") => {
                let mut hashes: Vec<String> = vec![];
                if let Some(h) = parse_fetch_hash(&cmd) {
                    hashes.push(h);
                }
                loop {
                    buf.clear();
                    if stdin.read_line(&mut buf)? == 0 {
                        break;
                    }
                    let next = buf.trim_end_matches('\n').trim_end_matches('\r');
                    if next.is_empty() {
                        break;
                    }
                    if next.starts_with("fetch ") {
                        if let Some(h) = parse_fetch_hash(next) {
                            hashes.push(h);
                        }
                    }
                }
                hashes.sort();
                hashes.dedup();

                if sui_repo.is_none() {
                    let repo = sui
                        .get_repo_by_owner_name(&package_id, &owner, &repo_name)
                        .await?
                        .with_context(|| {
                            format!("repository '{}/{}' not found", owner, repo_name)
                        })?;
                    sui_repo = Some(repo);
                }
                let repo = sui_repo.as_ref().unwrap();
                do_fetch(&config, &net, &sui, &walrus, repo, &repo_dir).await?;

                writeln!(stdout)?;
                stdout.flush()?;
            }

            _ if cmd.starts_with("push ") => {
                let mut refspecs: Vec<(String, String)> = vec![];
                if let Some(rs) = parse_refspec(cmd.trim_start_matches("push ")) {
                    refspecs.push(rs);
                }
                loop {
                    buf.clear();
                    if stdin.read_line(&mut buf)? == 0 {
                        break;
                    }
                    let next = buf.trim_end_matches('\n').trim_end_matches('\r');
                    if next.is_empty() {
                        break;
                    }
                    if next.starts_with("push ") {
                        if let Some(rs) = parse_refspec(next.trim_start_matches("push ")) {
                            refspecs.push(rs);
                        }
                    }
                }

                let walgit_dir = repo_dir.join(".walgit");
                let repo_cfg = load_repo_config(&walgit_dir)
                    .context("not a WalGit repository — run: walgit init <name>")?;
                if repo_cfg.id == "pending" || repo_cfg.id.is_empty() {
                    bail!("repository not registered on Sui — run: walgit init <name>");
                }

                for (src_ref, dst_ref) in &refspecs {
                    let branch = dst_ref.trim_start_matches("refs/heads/");
                    match do_push(
                        &config,
                        &net,
                        &sui,
                        &walrus,
                        &repo_cfg,
                        &package_id,
                        src_ref,
                        branch,
                        &repo_dir,
                        &walgit_dir,
                    )
                    .await
                    {
                        Ok(()) => writeln!(stdout, "ok {}", dst_ref)?,
                        Err(e) => {
                            print_error(&e);
                            let first_line = format!("{}", e)
                                .lines()
                                .next()
                                .unwrap_or("push failed")
                                .to_string();
                            writeln!(stdout, "error {} {}", dst_ref, first_line)?;
                        }
                    }
                }
                writeln!(stdout)?;
                stdout.flush()?;
            }

            other => {
                eprintln!("git-remote-walgit: unknown command: {:?}", other);
            }
        }
    }
    Ok(())
}

async fn do_fetch(
    config: &walgit::Config,
    net: &walgit::config::NetworkConfig,
    sui: &walgit::SuiClient,
    walrus: &walgit::WalrusClient,
    repo: &walgit::sui::types::RepoRecord,
    repo_dir: &PathBuf,
) -> Result<()> {
    // Single-blob model: every push runs `git pack-objects --all`, so each
    // blob already contains every reachable object — we only need one
    // download per branch HEAD.
    let mut blobs_to_download: Vec<(String, String)> = vec![];
    let mut seen = std::collections::HashSet::new();

    for (_branch, commit_id) in &repo.branches {
        let Ok(obj) = sui.get_object(commit_id).await else {
            continue;
        };
        let git_head = obj["git_head"].as_str().unwrap_or("").to_string();
        let blob_id = obj["blob_id"].as_str().unwrap_or("").to_string();
        if blob_id.is_empty() || git_head.is_empty() {
            continue;
        }
        if walgit::git::object_exists(repo_dir, &git_head) {
            continue;
        }
        if !seen.insert(blob_id.clone()) {
            continue;
        }
        blobs_to_download.push((blob_id, git_head));
    }

    if blobs_to_download.is_empty() {
        ui::einfo("already up to date");
        return Ok(());
    }

    ui::eheader("fetch");
    ui::estep(format!(
        "fetching {} blob(s) from Walrus",
        blobs_to_download.len()
    ));

    for (blob_id, _) in &blobs_to_download {
        let raw = walrus.download(blob_id).await.with_context(|| {
            format!(
                "download {} failed — storage may have expired (push again to renew)",
                blob_id
            )
        })?;

        let data = if repo.is_private && !repo.acl_id.is_empty() {
            let active = keystore::read_active_address(config.wallet_path.as_deref())?;
            let seal = walgit::SealClient::new(
                net.sui.graphql_url.clone(),
                net.seal.key_server_id.clone(),
                net.seal.key_server_url.clone(),
            );
            let acl_v = sui.get_initial_shared_version(&repo.acl_id).await?;
            let package_id = config
                .package_id()
                .map_err(|e| anyhow!("{}", e))?
                .to_string();
            seal.decrypt(
                &package_id,
                &repo.id,
                &repo.acl_id,
                acl_v,
                &active,
                config.wallet_path.as_deref(),
                &raw,
            )
            .await?
        } else {
            raw
        };

        walgit::git::unpack_objects(repo_dir, &data)?;
    }

    ui::esuccess("fetch complete");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn do_push(
    config: &walgit::Config,
    net: &walgit::config::NetworkConfig,
    sui: &walgit::SuiClient,
    walrus: &walgit::WalrusClient,
    repo_cfg: &LocalRepoConfig,
    package_id: &str,
    src_ref: &str,
    branch: &str,
    repo_dir: &PathBuf,
    walgit_dir: &PathBuf,
) -> Result<()> {
    let acl_id = repo_cfg
        .acl_id
        .as_deref()
        .context("ACL id missing in .walgit/config.toml — re-init")?;
    let active = keystore::read_active_address(config.wallet_path.as_deref())?;
    let kp = keystore::load_keypair(&active, config.wallet_path.as_deref())?;

    // Pre-flight access check.
    match sui.get_access_control(acl_id).await {
        Ok(acl) if active != acl.owner && !acl.allowed_writers.contains(&active) => {
            bail!(
                "access denied: {} does not have write access to this repository.\n\
                 Ask the owner to run: walgit access grant write {}",
                &active[..10.min(active.len())],
                active
            );
        }
        Ok(_) => {}
        Err(e) => bail!(
            "cannot verify write access (Sui network error): {}\n\
             Aborting to avoid wasting Walrus upload fees.",
            e
        ),
    }

    let git_head = walgit::git::rev_parse(repo_dir, src_ref)?;
    let message = walgit::git::get_commit_message(repo_dir, &git_head)?;

    // ─── Resolve the on-chain branch head (basis for incremental packing) ──
    // Cost-of-storage and dollar-per-push both fall by 10–100× when each push
    // uploads only the new commits
    let parent_commit_id = sui.get_repo_branch_head(&repo_cfg.id, branch).await?;
    let parent_git_head: Option<String> = match &parent_commit_id {
        Some(cid) => sui
            .get_object(cid)
            .await
            .ok()
            .and_then(|obj| obj["git_head"].as_str().map(String::from)),
        None => None,
    };

    // Fast-path: local tip already at on-chain tip → nothing to upload.
    if let Some(parent_sha) = &parent_git_head {
        if parent_sha == &git_head {
            ui::eheader(&format!("push  {} → already up-to-date", branch));
            return Ok(());
        }
    }

    // Decide packing mode.
    //   1. Incremental — we know the on-chain tip and have its commit locally.
    //   2. Full repack — first push to this branch, or we don't have the
    //      on-chain commit locally (different machine? rebase?). Slow path,
    //      but always correct.
    let (raw_pack, mode_label) = match &parent_git_head {
        Some(parent_sha) if walgit::git::object_exists(repo_dir, parent_sha) => {
            let (pack, new_commits) =
                walgit::git::pack_objects_incremental(repo_dir, &git_head, &[parent_sha.clone()])?;
            if new_commits == 0 || pack.is_empty() {
                ui::eheader(&format!("push  {} → already up-to-date", branch));
                return Ok(());
            }
            (
                pack,
                format!(
                    "incremental · {} new commit{}",
                    new_commits,
                    if new_commits == 1 { "" } else { "s" }
                ),
            )
        }
        _ => {
            let pack = walgit::git::pack_objects(repo_dir)?;
            (pack, "full pack (no incremental basis)".to_string())
        }
    };

    ui::eheader(&format!(
        "push  {} → {}  [{}]",
        branch,
        &git_head[..8],
        mode_label
    ));
    ui::einfo(format!(
        "pack size: {}",
        walgit::ui::fmt_bytes(raw_pack.len())
    ));

    let pack_data = if repo_cfg.private {
        ui::estep("encrypting with Seal IBE");
        let seal = walgit::SealClient::new(
            net.sui.graphql_url.clone(),
            net.seal.key_server_id.clone(),
            net.seal.key_server_url.clone(),
        );
        seal.encrypt(package_id, &repo_cfg.id, &raw_pack).await?
    } else {
        raw_pack
    };

    let upload = walrus.upload(pack_data, repo_cfg.epochs).await?;

    // Parent for the new on-chain Commit object = current on-chain branch
    // head we resolved above. Same value, no second round-trip.
    let parent = parent_commit_id.clone();

    ui::estep("recording commit on Sui");
    let (commit_id, _gas) = sui
        .push_commit(
            &kp,
            package_id,
            &repo_cfg.id,
            acl_id,
            &upload.blob_id,
            &git_head,
            parent.as_deref(),
            &message,
            branch,
        )
        .await?;

    // Append push history for diagnostics (cache only, not authoritative).
    let mut updated = repo_cfg.clone();
    updated.pushes.push(walgit::PushRecord {
        git_head: git_head.clone(),
        blob_id: upload.blob_id.clone(),
        branch: branch.to_string(),
        commit_id: commit_id.clone(),
        epochs: repo_cfg.epochs,
        pushed_at_secs: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    });
    save_repo_config(walgit_dir, &updated)?;

    ui::esuccess(format!(
        "pushed {} → blob {} · commit {}",
        ui::highlight(&git_head[..8].to_string()),
        console::style(&upload.blob_id[..12.min(upload.blob_id.len())]).cyan(),
        console::style(&commit_id[..12.min(commit_id.len())]).cyan(),
    ));

    Ok(())
}

fn parse_uri(uri: &str) -> Result<(String, String)> {
    let path = uri
        .strip_prefix("walgit://")
        .with_context(|| format!("invalid URI '{}' — expected walgit://<owner>/<repo>", uri))?;
    let parts: Vec<&str> = path.splitn(2, '/').collect();
    if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
        bail!("invalid URI '{}' — expected walgit://<owner>/<repo>", uri);
    }
    walgit::validate::sui_address(parts[0]).map_err(|e| anyhow!("{}", e))?;
    walgit::validate::repo_name(parts[1]).map_err(|e| anyhow!("{}", e))?;
    Ok((parts[0].to_string(), parts[1].to_string()))
}

fn parse_fetch_hash(cmd: &str) -> Option<String> {
    let parts: Vec<&str> = cmd.splitn(3, ' ').collect();
    if parts.len() >= 2 {
        Some(parts[1].to_string())
    } else {
        None
    }
}

fn parse_refspec(rs: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = rs.splitn(2, ':').collect();
    if parts.len() == 2 {
        Some((parts[0].to_string(), parts[1].to_string()))
    } else {
        None
    }
}
