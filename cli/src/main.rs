// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use clap::Parser;
use walgit::cli::{
    AccessAction, AgentAction, CacheAction, Cli, Command, PrAction, TraceAction,
};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Load display preferences before anything renders. We do this even for
    // `config --show` so the displayed table honours the user's setting.
    if let Ok(c) = walgit::config::load() {
        walgit::ui::set_short_ids(c.display.short_ids);
    }

    // Fail fast if global config is incomplete — before any command creates
    // directories, makes network calls, or uploads to Walrus. `config` is the
    // tool used to fix the problem, so it's exempt.
    if !matches!(cli.command, Command::Config { .. } | Command::Cache { .. }) {
        if let Err(e) = walgit::commands::preflight() {
            eprintln!();
            eprintln!(
                "  {} {}",
                console::style("✗").red().bold(),
                console::style("walgit is not configured yet").red().bold()
            );
            eprintln!("    {}", console::style(format!("{}", e)).dim());
            eprintln!();
            eprintln!("  Try:");
            eprintln!(
                "    {} {}",
                console::style("$").dim(),
                console::style("walgit config --show").cyan()
            );
            eprintln!(
                "    {} {}",
                console::style("$").dim(),
                console::style("walgit config --package-id <PACKAGE_ID>").cyan()
            );
            eprintln!();
            std::process::exit(1);
        }
    }

    match cli.command {
        Command::Init {
            name,
            here,
            private,
            epochs,
        } => walgit::commands::init::run(name, here, private, epochs).await?,
        Command::Log { limit, traces } => walgit::commands::log::run(limit, traces).await?,
        Command::Show { commit, trace } => walgit::commands::show::run(commit, trace).await?,
        Command::Agent { action } => match action {
            AgentAction::Commit {
                paths,
                message,
                trace,
            } => walgit::commands::agent::commit(paths, message, trace).await?,
        },
        Command::Trace { action } => match action {
            TraceAction::Diff { sha_a, sha_b } => {
                walgit::commands::trace::diff(sha_a, sha_b).await?
            }
        },
        Command::Status => walgit::commands::status::run().await?,
        Command::Access { action } => match action {
            AccessAction::List => walgit::commands::access::list().await?,
            AccessAction::Grant { role, address } => {
                walgit::commands::access::grant(role, address).await?
            }
            AccessAction::Revoke { role, address } => {
                walgit::commands::access::revoke(role, address).await?
            }
        },
        Command::Fork { url, yes } => walgit::commands::fork::run(url, yes).await?,
        Command::Cache { action } => match action {
            CacheAction::List => walgit::commands::cache::list().await?,
            CacheAction::Clean { repo_id, all } => {
                walgit::commands::cache::clean(repo_id, all).await?
            }
        },
        Command::Pr { action } => match action {
            PrAction::Create {
                source_branch,
                target_branch,
                yes,
            } => walgit::commands::pr::create(source_branch, target_branch, yes).await?,
            PrAction::List { mine } => walgit::commands::pr::list(mine).await?,
            PrAction::Show { pr_id } => walgit::commands::pr::show(pr_id).await?,
            PrAction::Diff { pr_id, stat } => walgit::commands::pr::diff(pr_id, stat).await?,
            PrAction::Approve { pr_id } => walgit::commands::pr::approve(pr_id).await?,
            PrAction::Merge { pr_id } => walgit::commands::pr::merge(pr_id).await?,
            PrAction::Close { pr_id } => walgit::commands::pr::close(pr_id).await?,
        },
        Command::Config {
            network,
            package_id,
            registry_id,
            graphql_url,
            publisher_url,
            aggregator_url,
            epochs,
            short_ids,
            full_ids,
            show,
        } => {
            let short_pref = if short_ids {
                Some(true)
            } else if full_ids {
                Some(false)
            } else {
                None
            };
            walgit::commands::config_cmd::run(
                network,
                package_id,
                registry_id,
                graphql_url,
                publisher_url,
                aggregator_url,
                epochs,
                short_pref,
                show,
            )
            .await?
        }
    }
    Ok(())
}
