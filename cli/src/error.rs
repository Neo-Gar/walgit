// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

//! Typed error hierarchy for the WalGit library layer.
//!
//! Library functions return `Result<T, WalGitError>`; binaries convert
//! at the top level using `anyhow::Result` for user-facing reporting.

use thiserror::Error;

pub type Result<T> = std::result::Result<T, WalGitError>;

#[derive(Error, Debug)]
pub enum WalGitError {
    // ─── Configuration / setup ──────────────────────────────────────────────
    #[error("config error: {0}")]
    Config(String),

    #[error("not a WalGit repository (no .walgit/ found). Run: walgit init <name>")]
    NotARepo,

    #[error("repository on disk is not yet registered on Sui. Run: walgit init")]
    RepoNotRegistered,

    // ─── Sui ────────────────────────────────────────────────────────────────
    #[error("Sui network error: {0}")]
    SuiNetwork(String),

    #[error("Sui GraphQL error: {0}")]
    SuiGraphQL(String),

    #[error("Sui transaction failed: {0}")]
    SuiTransaction(String),

    #[error("repository '{0}' not found on Sui")]
    RepoNotFound(String),

    #[error("object {0} not found on Sui")]
    ObjectNotFound(String),

    #[error("insufficient gas to execute transaction")]
    InsufficientGas,

    #[error("Move abort code {code} in {function}: {message}")]
    MoveAbort {
        function: String,
        code: u64,
        message: String,
    },

    // ─── Walrus ─────────────────────────────────────────────────────────────
    #[error("Walrus upload failed: {0}")]
    WalrusUpload(String),

    #[error("Walrus download failed for blob {blob_id}: {reason}")]
    WalrusDownload { blob_id: String, reason: String },

    #[error("insufficient WAL balance — get testnet WAL: `walrus get-wal --context testnet`")]
    InsufficientWal,

    // ─── Seal IBE ───────────────────────────────────────────────────────────
    #[error("Seal encryption failed: {0}")]
    SealEncrypt(String),

    #[error("Seal decryption failed: {0}")]
    SealDecrypt(String),

    #[error("Seal key server error: {0}")]
    SealKeyServer(String),

    // ─── Access control ─────────────────────────────────────────────────────
    #[error("access denied: {0}")]
    AccessDenied(String),

    // ─── Git ────────────────────────────────────────────────────────────────
    #[error("git subprocess failed: {0}")]
    Git(String),

    #[error("git not installed or not in PATH")]
    GitNotInstalled,

    // ─── Wallet / signing ───────────────────────────────────────────────────
    #[error("Sui wallet not found at {0}. Install Sui CLI and run `sui client`")]
    WalletNotFound(String),

    #[error("Sui CLI not installed or not in PATH")]
    SuiCliNotInstalled,

    #[error("no key for address {0} in Sui keystore")]
    KeyNotFound(String),

    // ─── IO / serialization passthroughs ────────────────────────────────────
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("TOML parse error: {0}")]
    TomlParse(#[from] toml::de::Error),

    #[error("TOML serialize error: {0}")]
    TomlSerialize(#[from] toml::ser::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("BCS error: {0}")]
    Bcs(#[from] bcs::Error),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    // ─── Catch-all for foreign errors that should still surface upward ──────
    #[error("{0}")]
    Other(String),
}

impl WalGitError {
    pub fn other(msg: impl Into<String>) -> Self {
        WalGitError::Other(msg.into())
    }

    pub fn config(msg: impl Into<String>) -> Self {
        WalGitError::Config(msg.into())
    }

    pub fn sui_network(msg: impl Into<String>) -> Self {
        WalGitError::SuiNetwork(msg.into())
    }

    pub fn sui_graphql(msg: impl Into<String>) -> Self {
        WalGitError::SuiGraphQL(msg.into())
    }

    pub fn sui_transaction(msg: impl Into<String>) -> Self {
        WalGitError::SuiTransaction(msg.into())
    }

    pub fn git(msg: impl Into<String>) -> Self {
        WalGitError::Git(msg.into())
    }

    /// True for errors worth retrying with backoff (transient network issues).
    /// False for permission errors, parse errors, etc. that won't change on retry.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            WalGitError::Http(_)
                | WalGitError::SuiNetwork(_)
                | WalGitError::SuiGraphQL(_)
                | WalGitError::WalrusUpload(_)
                | WalGitError::WalrusDownload { .. }
                | WalGitError::SealKeyServer(_)
        )
    }
}
