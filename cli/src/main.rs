// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use clap::Parser;
use walgit::cli::{
    AccessAction, AgentAction, CacheAction, Cli, Command, MemwalAction, PrAction, TraceAction,
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
    // `trace` works against a plain git repo (no `.walgit/` needed) so it must
    // run even when walgit itself isn't configured for Sui/Walrus yet —
    // otherwise "turn on the hook in any git repo" would force users through
    // full walgit setup first.
    if !matches!(
        cli.command,
        Command::Config { .. }
            | Command::Cache { .. }
            | Command::Gc { .. }
            | Command::Trace { .. }
            | Command::Memwal { .. }
    ) {
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
        Command::Trace { action } => {
            use walgit::commands::trace as t;
            match action {
                TraceAction::Diff { sha_a, sha_b } => t::diff(sha_a, sha_b).await?,
                TraceAction::Start {
                    agent,
                    run_id,
                    task,
                    parent_run,
                    source,
                    from_claude_hook,
                    force,
                    tag: _,
                    only_if_enabled,
                } => {
                    t::start(t::StartOpts {
                        agent_id: agent,
                        run_id,
                        task,
                        parent_run_id: parent_run,
                        source,
                        from_claude_hook,
                        force,
                        only_if_enabled,
                    })
                    .await?
                }
                TraceAction::Record {
                    name,
                    input,
                    output,
                    from_claude_hook,
                    event,
                    tag: _,
                    only_if_enabled,
                } => {
                    let kind = if from_claude_hook {
                        let ev = event.as_deref().ok_or_else(|| {
                            anyhow::anyhow!("--event is required with --from-claude-hook")
                        })?;
                        let event = match ev {
                            "user-prompt" => t::ClaudeEvent::UserPrompt,
                            "post-tool-use" => t::ClaudeEvent::PostToolUse,
                            "stop" => t::ClaudeEvent::Stop,
                            other => {
                                return Err(anyhow::anyhow!(
                                    "unknown --event '{}' (expected user-prompt, post-tool-use, stop)",
                                    other
                                ));
                            }
                        };
                        t::RecordKind::ClaudeHook { event }
                    } else {
                        let name = name.ok_or_else(|| {
                            anyhow::anyhow!("--name is required (or use --from-claude-hook)")
                        })?;
                        t::RecordKind::Tool {
                            name,
                            input: input.unwrap_or_default(),
                            output: output.unwrap_or_default(),
                        }
                    };
                    t::record(kind, only_if_enabled).await?
                }
                TraceAction::Set {
                    task,
                    decision,
                    alternative,
                    confidence,
                    parent_run,
                } => {
                    t::set(t::SetOpts {
                        task,
                        decision,
                        alternative,
                        confidence,
                        parent_run_id: parent_run,
                    })
                    .await?
                }
                TraceAction::Status => t::status().await?,
                TraceAction::Abort => t::abort().await?,
                TraceAction::Snapshot { commit } => t::snapshot(commit).await?,
                TraceAction::Upload { commit, namespace } => t::upload(commit, namespace).await?,
                TraceAction::Recall {
                    query,
                    limit,
                    namespace,
                    threshold,
                } => t::recall(query, limit, namespace, threshold).await?,
                TraceAction::Install {
                    agent,
                    no_global,
                    global_only,
                } => {
                    t::install(t::InstallOpts {
                        agent_arg: agent,
                        no_global,
                        global_only,
                    })
                    .await?
                }
                TraceAction::Uninstall {
                    agent,
                    purge_global,
                } => {
                    t::uninstall(t::UninstallOpts {
                        agent_arg: agent,
                        purge_global,
                    })
                    .await?
                }
            }
        }
        Command::Memwal { action } => {
            use walgit::commands::memwal_cmd as m;
            match action {
                MemwalAction::Init {
                    force,
                    relayer_url,
                } => m::init(force, relayer_url).await?,
                MemwalAction::Status => m::status().await?,
                MemwalAction::List => m::list().await?,
                MemwalAction::AddDelegate {
                    pubkey_hex,
                    sui_address,
                    label,
                } => m::add_delegate(pubkey_hex, sui_address, label).await?,
                MemwalAction::RemoveDelegate { pubkey_hex } => {
                    m::remove_delegate(pubkey_hex).await?
                }
            }
        }
        Command::Status => walgit::commands::status::run().await?,
        Command::Access { action } => match action {
            AccessAction::List => walgit::commands::access::list().await?,
            AccessAction::Grant {
                role,
                address,
                memwal_pubkey,
                memwal_label,
            } => {
                walgit::commands::access::grant(role, address, memwal_pubkey, memwal_label).await?
            }
            AccessAction::Revoke {
                role,
                address,
                memwal_pubkey,
            } => walgit::commands::access::revoke(role, address, memwal_pubkey).await?,
        },
        Command::Fork { url, yes } => walgit::commands::fork::run(url, yes).await?,
        Command::Cache { action } => match action {
            CacheAction::List => walgit::commands::cache::list().await?,
            CacheAction::Clean { repo_id, all } => {
                walgit::commands::cache::clean(repo_id, all).await?
            }
        },
        Command::Gc { keep } => walgit::commands::gc::run(keep).await?,
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
            betterleaks,
            depth,
            keep,
            sponsored,
            show,
        } => {
            let short_pref = if short_ids {
                Some(true)
            } else if full_ids {
                Some(false)
            } else {
                None
            };
            let betterleaks_skip_pref = match betterleaks.as_deref() {
                Some("disable") => Some(true),
                Some("enable") => Some(false),
                Some(other) => {
                    return Err(anyhow::anyhow!(
                        "--betterleaks: expected 'enable' or 'disable', got '{}'",
                        other
                    ));
                }
                None => None,
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
                betterleaks_skip_pref,
                depth,
                keep,
                sponsored,
                show,
            )
            .await?
        }
    }
    Ok(())
}
