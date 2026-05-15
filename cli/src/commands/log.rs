// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

use crate::commands::{CommandContext, find_repo, require_registered};
use crate::error::Result;
use crate::trace::{self, Trace};
use crate::{git, ui};
use chrono::{DateTime, Utc};

pub async fn run(limit: usize, traces: bool) -> Result<()> {
    if traces {
        return run_local_with_traces(limit);
    }
    run_onchain(limit).await
}

/// Default mode: walk the on-chain Commit chain (network-truth view).
async fn run_onchain(limit: usize) -> Result<()> {
    let (_, _, local) = find_repo()?;
    require_registered(&local)?;

    let ctx = CommandContext::load().await?;
    let repo = ctx.sui.get_repo_by_id(&local.id, &ctx.active_address).await?;

    let main_branch = repo
        .branches
        .keys()
        .find(|k| k.as_str() == "main")
        .cloned()
        .or_else(|| repo.branches.keys().next().cloned());

    let Some(branch) = main_branch else {
        ui::warn("no branches yet — run `git push walgit://…`");
        return Ok(());
    };

    let head_id = repo
        .branches
        .get(&branch)
        .expect("branch key must exist")
        .clone();
    let commits = ctx.sui.get_commit_chain(&head_id, limit).await?;

    ui::header(&format!("log — {} ({} commits)", branch, commits.len()));
    for c in commits {
        let dt = DateTime::<Utc>::from_timestamp_millis(c.timestamp as i64)
            .map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string())
            .unwrap_or_default();
        println!(
            "  {}  {}",
            console::style(ui::short_hash(&c.git_head)).yellow(),
            ui::highlight(&c.message),
        );
        println!(
            "      {} {} · {} · blob {}",
            ui::dim("·"),
            ui::short_id(&c.author),
            ui::dim(&dt),
            console::style(&c.blob_id[..12.min(c.blob_id.len())]).cyan()
        );
    }
    Ok(())
}

/// `--traces` mode: read local git log and surface agent_id + task for every
/// commit that carries a reasoning trace footer. Commits without traces show
/// dimmed so the contrast is obvious.
fn run_local_with_traces(limit: usize) -> Result<()> {
    let (repo_dir, _, _local) = find_repo()?;
    let commits = git::recent_commits(&repo_dir, "HEAD", limit)?;

    ui::header(&format!("log — traces ({} commits)", commits.len()));
    for (sha, message) in commits {
        let subject = message.lines().next().unwrap_or("").to_string();
        match trace::extract_trace_json(&message).and_then(|j| Trace::parse(&j).ok()) {
            Some(t) => {
                println!(
                    "  {}  {}  {}",
                    console::style(ui::short_hash(&sha)).yellow(),
                    console::style(format!("[{}]", t.agent_id)).cyan().bold(),
                    ui::highlight(&t.task),
                );
                println!("      {} {}", ui::dim("·"), ui::dim(&subject));
            }
            None => {
                println!(
                    "  {}  {}",
                    console::style(ui::short_hash(&sha)).yellow().dim(),
                    ui::dim(&subject),
                );
            }
        }
    }
    Ok(())
}
