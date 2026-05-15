// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

//! Clap command surface for the `walgit` CLI binary.

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "walgit", version, about = "Decentralized Git on Walrus + Sui")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Create a new WalGit repository.
    Init {
        /// Repository name. Without `--here`, a new directory `<name>/` is
        /// created in the current working directory.
        name: String,
        /// Initialise inside the current directory instead of creating `<name>/`.
        #[arg(long)]
        here: bool,
        #[arg(long)]
        private: bool,
        #[arg(long)]
        epochs: Option<u32>,
    },

    /// Show commit history for the current branch.
    Log {
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Show agent_id + task summary for each commit that carries a trace.
        #[arg(long)]
        traces: bool,
    },

    /// Show a single commit. Pass `--trace` to render the reasoning trace.
    Show {
        /// Commit SHA. Defaults to HEAD.
        #[arg(default_value = "HEAD")]
        commit: String,
        #[arg(long)]
        trace: bool,
    },

    /// Agent-facing helpers (commit, …).
    Agent {
        #[command(subcommand)]
        action: AgentAction,
    },

    /// Operate on reasoning traces.
    Trace {
        #[command(subcommand)]
        action: TraceAction,
    },

    /// Show repository status.
    Status,

    /// Manage repository access control.
    Access {
        #[command(subcommand)]
        action: AccessAction,
    },

    /// Fork another user's public repository.
    Fork {
        /// walgit://<owner>/<repo>
        url: String,
        /// Skip the interactive preview/confirmation step.
        #[arg(long, short = 'y')]
        yes: bool,
    },

    /// Pull request operations.
    Pr {
        #[command(subcommand)]
        action: PrAction,
    },

    /// Manage the auto-clone cache at ~/.walgit/work/.
    Cache {
        #[command(subcommand)]
        action: CacheAction,
    },

    /// Read or modify walgit configuration.
    Config {
        #[arg(long)]
        network: Option<String>,
        #[arg(long, value_name = "ID")]
        package_id: Option<String>,
        #[arg(long, value_name = "ID")]
        registry_id: Option<String>,
        #[arg(long, value_name = "URL")]
        graphql_url: Option<String>,
        #[arg(long, value_name = "URL")]
        publisher_url: Option<String>,
        #[arg(long, value_name = "URL")]
        aggregator_url: Option<String>,
        #[arg(long)]
        epochs: Option<u32>,
        /// Render Sui object IDs as `0xabcde…12345` everywhere.
        #[arg(long)]
        short_ids: bool,
        /// Render full Sui object IDs everywhere (default).
        #[arg(long, conflicts_with = "short_ids")]
        full_ids: bool,
        #[arg(long)]
        show: bool,
    },
}

#[derive(Subcommand)]
pub enum AccessAction {
    /// List allowed readers and writers.
    List,
    /// Grant access. role = "read" or "write".
    Grant { role: String, address: String },
    /// Revoke access. role = "read" or "write".
    Revoke { role: String, address: String },
}

#[derive(Subcommand)]
pub enum AgentAction {
    /// Stage paths, write a commit, and embed a reasoning trace footer.
    Commit {
        /// Files / directories to add (passed to `git add`). Use `.` for all.
        #[arg(required = true)]
        paths: Vec<String>,
        /// Short commit subject (the user-visible message).
        #[arg(long, short = 'm', required = true)]
        message: String,
        /// Path to a JSON trace file. Pass `-` to read trace from stdin.
        #[arg(long, required = true)]
        trace: String,
    },
}

#[derive(Subcommand)]
pub enum TraceAction {
    /// Side-by-side diff of two commits' reasoning traces.
    Diff {
        sha_a: String,
        sha_b: String,
    },
}

#[derive(Subcommand)]
pub enum CacheAction {
    /// List every cached clone with its size on disk.
    List,
    /// Delete one or all cached clones.
    Clean {
        /// Repository ID (0x…) of the clone to remove. Mutually exclusive with `--all`.
        repo_id: Option<String>,
        #[arg(long, conflicts_with = "repo_id")]
        all: bool,
    },
}

#[derive(Subcommand)]
pub enum PrAction {
    /// Open a new pull request. With no args, runs interactively:
    /// auto-detects source branch (HEAD) and target (fork parent if any).
    Create {
        #[arg(long)]
        source_branch: Option<String>,
        #[arg(long)]
        target_branch: Option<String>,
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// List PRs. Defaults to current repo; `--mine` lists PRs you authored
    /// across all repositories on the active network.
    List {
        #[arg(long)]
        mine: bool,
    },
    Show { pr_id: String },
    /// Show the diff between target branch and the PR's source tip.
    Diff {
        pr_id: String,
        /// Show only the per-file stat summary, no patch bodies.
        #[arg(long)]
        stat: bool,
    },
    Approve { pr_id: String },
    Merge { pr_id: String },
    Close { pr_id: String },
}
