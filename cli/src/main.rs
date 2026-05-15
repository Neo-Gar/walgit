// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use clap::Parser;
use walgit::cli::{AccessAction, Cli, Command, PrAction};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Fail fast if global config is incomplete — before any command creates
    // directories, makes network calls, or uploads to Walrus. `config` is the
    // tool used to fix the problem, so it's exempt.
    if !matches!(cli.command, Command::Config { .. }) {
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
            description,
            private,
            epochs,
        } => walgit::commands::init::run(name, here, description, private, epochs).await?,
        Command::Log { limit } => walgit::commands::log::run(limit).await?,
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
        Command::Fork { url, description } => walgit::commands::fork::run(url, description).await?,
        Command::Pr { action } => match action {
            PrAction::Create {
                source_branch,
                target_branch,
            } => walgit::commands::pr::create(source_branch, target_branch).await?,
            PrAction::List => walgit::commands::pr::list().await?,
            PrAction::Approve { pr_id } => walgit::commands::pr::approve(pr_id).await?,
            PrAction::Merge { pr_id } => walgit::commands::pr::merge(pr_id).await?,
            PrAction::Close { pr_id } => walgit::commands::pr::close(pr_id).await?,
            PrAction::Status { pr_id } => walgit::commands::pr::status(pr_id).await?,
        },
        Command::Config {
            network,
            package_id,
            graphql_url,
            publisher_url,
            aggregator_url,
            epochs,
            show,
        } => {
            walgit::commands::config_cmd::run(
                network,
                package_id,
                graphql_url,
                publisher_url,
                aggregator_url,
                epochs,
                show,
            )
            .await?
        }
    }
    Ok(())
}
