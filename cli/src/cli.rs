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
        description: Option<String>,
        #[arg(long)]
        private: bool,
        #[arg(long)]
        epochs: Option<u32>,
    },

    /// Show commit history for the current branch.
    Log {
        #[arg(long, default_value_t = 20)]
        limit: usize,
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
        #[arg(long)]
        description: Option<String>,
    },

    /// Pull request operations.
    Pr {
        #[command(subcommand)]
        action: PrAction,
    },

    /// Read or modify walgit configuration.
    Config {
        #[arg(long)]
        network: Option<String>,
        #[arg(long, value_name = "ID")]
        package_id: Option<String>,
        #[arg(long, value_name = "URL")]
        graphql_url: Option<String>,
        #[arg(long, value_name = "URL")]
        publisher_url: Option<String>,
        #[arg(long, value_name = "URL")]
        aggregator_url: Option<String>,
        #[arg(long)]
        epochs: Option<u32>,
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
pub enum PrAction {
    Create {
        source_branch: String,
        #[arg(long, default_value = "main")]
        target_branch: String,
    },
    List,
    Approve { pr_id: String },
    Merge { pr_id: String },
    Close { pr_id: String },
    Status { pr_id: String },
}
