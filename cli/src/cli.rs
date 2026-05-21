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

    /// MemWal — manage delegate keys and on-chain `MemWalAccount.delegate_keys`.
    Memwal {
        #[command(subcommand)]
        action: MemwalAction,
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
pub enum MemwalAction {
    /// Generate a local Ed25519 delegate keypair and refresh `[memwal]` config.
    /// Prints the public part for the repo owner to register.
    Init {
        /// Overwrite the existing delegate key if one is already on disk.
        #[arg(long)]
        force: bool,
        /// Set `memwal.account_id` in the global config.
        #[arg(long)]
        account_id: Option<String>,
        /// Override the relayer URL (defaults to staging on testnet,
        /// production on mainnet).
        #[arg(long)]
        relayer_url: Option<String>,
    },
    /// Show local delegate identity and whether it's registered on chain.
    Status,
    /// List all delegates on the configured `MemWalAccount`.
    List,
    /// Owner-only: register `<pubkey-hex>` paired with `<sui-address>` as a
    /// delegate on the configured `MemWalAccount`.
    AddDelegate {
        pubkey_hex: String,
        sui_address: String,
        #[arg(long)]
        label: Option<String>,
    },
    /// Owner-only: remove a delegate by its public key.
    RemoveDelegate { pubkey_hex: String },
}

#[derive(Subcommand)]
pub enum AccessAction {
    /// List allowed readers and writers.
    List,
    /// Grant access. role = "read" or "write". With `--memwal-pubkey` also
    /// registers the collaborator's delegate key on the repo's
    /// `MemWalAccount` so they can write reasoning traces.
    Grant {
        role: String,
        address: String,
        /// Hex Ed25519 public key shared by the collaborator (32 bytes /
        /// 64 hex chars). Skip to only update the walgit ACL.
        #[arg(long)]
        memwal_pubkey: Option<String>,
        /// Free-form label for the delegate entry.
        #[arg(long, default_value = "walgit")]
        memwal_label: String,
    },
    /// Revoke access. role = "read" or "write". With `--memwal-pubkey`
    /// also removes the delegate from `MemWalAccount`.
    Revoke {
        role: String,
        address: String,
        #[arg(long)]
        memwal_pubkey: Option<String>,
    },
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

    /// Internal: called by the `post-commit` git hook to snapshot the pending
    /// trace as `<git-dir>/walgit/traces/<HEAD-sha>.json`. Safe to call
    /// manually if you want to retroactively associate the current pending
    /// trace with the freshly-created commit.
    Snapshot {
        /// Override the commit SHA to write under. Defaults to HEAD.
        #[arg(long)]
        commit: Option<String>,
    },

    /// Upload local trace snapshots to MemWal. Without args uploads everything
    /// in `<git-dir>/walgit/traces/`; with `<sha>` uploads only that one.
    Upload {
        /// Specific commit SHA to upload. Without it, uploads all snapshots.
        commit: Option<String>,
        /// MemWal namespace override. Defaults to the current repo name.
        #[arg(long)]
        namespace: Option<String>,
    },

    /// Semantic search across the project's MemWal namespace. Surfaces past
    /// reasoning traces relevant to the query.
    Recall {
        /// Natural-language query.
        query: String,
        #[arg(long, default_value_t = 5)]
        limit: u32,
        #[arg(long)]
        namespace: Option<String>,
        /// Minimum similarity score (0.0–1.0). Lower values broaden the search.
        /// Default: relayer's built-in threshold (typically ~0.5).
        /// Try 0.3 if you're getting "no matches" for queries you expect to hit.
        #[arg(long)]
        threshold: Option<f32>,
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
