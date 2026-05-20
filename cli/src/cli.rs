// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

//! Clap command surface for the `walgit` CLI binary.

use clap::{Parser, Subcommand};
use std::path::PathBuf;

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
    Diff { sha_a: String, sha_b: String },

    /// Open a pending trace in `.git/walgit/pending-trace.json`. Subsequent
    /// `record`/`set` calls accumulate into it; the next `git commit` flushes
    /// it into the commit message footer.
    Start {
        /// Free-form agent identifier (e.g. `writer-v1`). Required unless
        /// `--from-claude-hook` is set.
        #[arg(long)]
        agent: Option<String>,
        /// Override the auto-generated run_id (UUID/ULID-shaped string).
        #[arg(long)]
        run_id: Option<String>,
        /// One-sentence task description (≤ 200 chars). Can also be set later
        /// via `walgit trace set --task`.
        #[arg(long)]
        task: Option<String>,
        /// `run_id` of a prior agent action this one is responding to.
        #[arg(long)]
        parent_run: Option<String>,
        /// Adapter source label, surfaced in `walgit trace status`.
        #[arg(long)]
        source: Option<String>,
        /// Decode a Claude Code hook payload from stdin to derive agent_id/run_id.
        #[arg(long)]
        from_claude_hook: bool,
        /// Replace any existing pending trace from a different run.
        #[arg(long)]
        force: bool,
        /// Sentinel string carried only so installed hooks can be identified.
        /// Ignored by command logic.
        #[arg(long, hide = true)]
        tag: Option<String>,
        /// Exit 0 silently if the current repo is not marked enabled
        /// (`<git-dir>/walgit/enabled`). Used by the user-global Claude Code
        /// hooks so they no-op outside opted-in repos.
        #[arg(long, hide = true)]
        only_if_enabled: bool,
    },

    /// Append a tool call (or a Claude Code hook event) to the pending trace.
    Record {
        /// Tool name for the manual form.
        #[arg(long, requires = "input")]
        name: Option<String>,
        /// One-line input summary (≤ 200 chars).
        #[arg(long)]
        input: Option<String>,
        /// One-line output summary (≤ 200 chars).
        #[arg(long)]
        output: Option<String>,
        /// Decode stdin as a Claude Code hook payload of the given event.
        #[arg(long)]
        from_claude_hook: bool,
        /// Required with `--from-claude-hook`. One of:
        /// `user-prompt`, `post-tool-use`, `stop`.
        #[arg(long)]
        event: Option<String>,
        #[arg(long, hide = true)]
        tag: Option<String>,
        /// See `start --only-if-enabled`.
        #[arg(long, hide = true)]
        only_if_enabled: bool,
    },

    /// Set fields on the pending trace. Repeatable for alternatives.
    Set {
        #[arg(long)]
        task: Option<String>,
        #[arg(long)]
        decision: Option<String>,
        /// Append one rejected alternative. May be passed multiple times.
        #[arg(long)]
        alternative: Vec<String>,
        #[arg(long)]
        confidence: Option<f32>,
        #[arg(long)]
        parent_run: Option<String>,
    },

    /// Show what's currently in the pending trace, if anything.
    Status,

    /// Discard the pending trace (archives it to `last-trace.json`).
    Abort,

    /// Internal: called by the `prepare-commit-msg` git hook to inject the
    /// pending trace into the commit message. Safe to call manually.
    Flush {
        #[arg(long)]
        message_file: PathBuf,
    },

    /// Install hooks so traces are recorded automatically. Idempotent.
    ///
    /// By default writes adapter hooks to BOTH user-global (`~/.claude/`)
    /// and project-local (`<repo>/.claude/`) settings, installs the git
    /// hook in this repo, and marks this repo as opted-in via
    /// `<git-dir>/walgit/enabled`. The global hooks are gated by that
    /// marker, so other repos aren't affected.
    ///
    /// Why both: Cursor's Claude Code extension reads ONLY user-global
    /// settings, while `claude` CLI in a terminal also picks up
    /// project-local. Installing in both covers both usage modes without
    /// double-firing (markers gate the global path).
    ///
    /// With no `--agent`, opens an interactive picker. `--agent` accepts a
    /// single key (`claude-code`), a comma-separated list, or `all`.
    Install {
        #[arg(long, value_name = "AGENT[,AGENT…]|all")]
        agent: Option<String>,
        /// Skip writing to `~/.claude/settings.json` (project-local only).
        #[arg(long, conflicts_with = "global_only")]
        no_global: bool,
        /// Skip writing to `<repo>/.claude/settings.json` (global only).
        #[arg(long, conflicts_with = "no_global")]
        global_only: bool,
    },

    /// Remove hooks installed by `install`. Preserves user-authored hooks.
    /// Without `--agent`, sweeps every known adapter.
    ///
    /// By default removes the opt-in marker and project-local hooks but
    /// leaves user-global hooks intact (they're harmless without the
    /// marker). Pass `--purge-global` to also strip them from
    /// `~/.claude/settings.json`.
    Uninstall {
        #[arg(long, value_name = "AGENT[,AGENT…]|all")]
        agent: Option<String>,
        #[arg(long)]
        purge_global: bool,
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
