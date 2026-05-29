// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

//! Snapshot garbage collection.
//!
//! Each push uploads a self-contained shallow snapshot that supersedes the
//! previous one, so older snapshots are redundant. We keep the newest `keep`
//! and delete the rest — but only blobs *we own* (stored deletable and
//! `send_object_to` our address), located by the `Blob` object id recorded in
//! `PushRecord`. Deletion is delegated to the installed `walrus` CLI.
//!
//! GC is best-effort: a failed delete never fails the push, and only blobs we
//! actually deleted are marked gone (so reruns are idempotent).

use crate::commands::find_repo;
use crate::config::{LocalRepoConfig, save_repo_config};
use crate::error::Result;
use crate::sui::SuiClient;
use crate::ui;
use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

pub struct GcOutcome {
    pub deleted: usize,
    pub failed: usize,
    pub kept: usize,
}

/// Delete our own snapshot blobs that are neither a current branch head nor in
/// the newest-`keep` rollback window, via `walrus delete --object-ids`.
///
/// `protected` holds the content `blob_id`s of every current on-chain branch
/// head — those must never be deleted or a fresh clone of that branch breaks.
/// On top of that we retain the newest `keep` snapshots as a rollback buffer.
/// Deleted entries are dropped from `cfg.live_snapshots` (keeping it bounded),
/// and the config is persisted. Never errors on a delete failure.
pub fn gc_snapshots(
    walgit_dir: &Path,
    cfg: &mut LocalRepoConfig,
    keep: usize,
    network: &str,
    protected: &HashSet<String>,
) -> GcOutcome {
    let keep = keep.max(1);
    let total = cfg.live_snapshots.len();
    let kept = keep.min(total);

    // Indices to keep: the newest `keep` (rollback buffer) plus any whose blob
    // is a live branch head. Everything else is deletable.
    let keep_from = total.saturating_sub(keep);
    let mut deleted = 0;
    let mut failed = 0;
    let mut survivors = Vec::with_capacity(total);

    for (i, snap) in cfg.live_snapshots.drain(..).enumerate() {
        let in_buffer = i >= keep_from;
        let is_head = protected.contains(&snap.blob_id);
        if in_buffer || is_head {
            survivors.push(snap);
            continue;
        }
        if walrus_delete(&snap.blob_object_id, network) {
            deleted += 1; // dropped (not pushed to survivors)
        } else {
            failed += 1;
            survivors.push(snap); // keep so we retry next time
        }
    }
    cfg.live_snapshots = survivors;

    if deleted > 0 {
        let _ = save_repo_config(walgit_dir, cfg);
    }
    GcOutcome {
        deleted,
        failed,
        kept,
    }
}

/// Content `blob_id`s of every current branch head of `repo_id` — the set gc
/// must never delete. Best-effort: unreadable commits are skipped.
pub async fn protected_head_blobs(sui: &SuiClient, repo_id: &str, owner: &str) -> HashSet<String> {
    let mut set = HashSet::new();
    let Ok(repo) = sui.get_repo_by_id(repo_id, owner).await else {
        return set;
    };
    for (_branch, commit_id) in &repo.branches {
        if let Ok(obj) = sui.get_object(commit_id).await {
            if let Some(b) = obj["blob_id"].as_str() {
                if !b.is_empty() {
                    set.insert(b.to_string());
                }
            }
        }
    }
    set
}

fn walrus_delete(object_id: &str, network: &str) -> bool {
    Command::new("walrus")
        .args([
            "delete",
            "--object-ids",
            object_id,
            "--yes",
            "--context",
            network,
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// `walgit gc` — manual trigger for the current repo.
pub async fn run(keep_override: Option<usize>) -> Result<()> {
    let (_repo_dir, walgit_dir, mut cfg) = find_repo()?;
    let global = crate::config::load()?;
    let keep = keep_override.unwrap_or(global.storage.keep);
    let network = cfg.network.clone().unwrap_or_else(|| global.network.clone());

    // Reads only (no package_id needed): resolve current branch heads so gc
    // never deletes a blob still serving a branch.
    let protected = match global.networks.get(&network) {
        Some(net) => {
            let sui = SuiClient::new(net.sui.graphql_url.clone())?;
            let owner = crate::sui::keystore::read_active_address(global.wallet_path.as_deref())
                .unwrap_or_default();
            protected_head_blobs(&sui, &cfg.id, &owner).await
        }
        None => HashSet::new(),
    };

    ui::header("gc");
    let out = gc_snapshots(&walgit_dir, &mut cfg, keep, &network, &protected);
    if out.deleted > 0 {
        ui::success(format!("deleted {} superseded snapshot blob(s)", out.deleted));
    }
    if out.failed > 0 {
        ui::warn(format!(
            "{} blob(s) could not be deleted (walrus unavailable or not owned)",
            out.failed
        ));
    }
    if out.deleted == 0 && out.failed == 0 {
        ui::info(format!("nothing to gc — keeping newest {}", out.kept));
    }
    Ok(())
}
