// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

pub mod access;
pub mod agent;
pub mod cache;
pub mod config_cmd;
pub mod fork;
pub mod init;
pub mod log;
pub mod pr;
pub mod show;
pub mod status;
pub mod trace;

use crate::config::{Config, LocalRepoConfig, load_repo_config};
use crate::error::{Result, WalGitError};
use crate::sui::SuiClient;
use crate::sui::keystore;
use crate::walrus::WalrusClient;
use std::path::{Path, PathBuf};

/// Helpers used across multiple commands. Centralised to keep individual
/// command files focused on user-facing logic.

pub struct CommandContext {
    pub config: Config,
    pub package_id: String,
    pub registry_id: String,
    pub sui: SuiClient,
    pub walrus: WalrusClient,
    pub active_address: String,
}

impl CommandContext {
    pub async fn load() -> Result<Self> {
        let config = crate::config::load()?;
        let net = config.active_network()?.clone();
        let package_id = config.package_id()?.to_string();
        let registry_id = config.registry_id()?.to_string();
        let sui = SuiClient::new(net.sui.graphql_url.clone())?;
        let walrus = WalrusClient::new(net.walrus.publisher_url, net.walrus.aggregator_url);
        let active_address = keystore::read_active_address(config.wallet_path.as_deref())?;
        Ok(Self {
            config,
            package_id,
            registry_id,
            sui,
            walrus,
            active_address,
        })
    }

    pub fn keypair(&self) -> Result<keystore::KeyPair> {
        keystore::load_keypair(&self.active_address, self.config.wallet_path.as_deref())
    }

    pub fn seal_client(&self) -> Result<crate::seal::SealClient> {
        let net = self.config.active_network()?;
        Ok(crate::seal::SealClient::new(
            net.sui.graphql_url.clone(),
            net.seal.key_server_id.clone(),
            net.seal.key_server_url.clone(),
        ))
    }
}

/// Validate global configuration without touching the filesystem or network.
/// Run this before any command that may have side effects (init creates
/// directories, push uploads to Walrus, etc.) so misconfiguration fails fast
/// and visibly.
///
/// Checks, in order:
///   1. `~/.walgit/config.toml` parses (or default if missing)
///   2. active network exists under `[networks.<name>]`
///   3. `package_id` is set for the active network
///   4. Sui keystore + `client.yaml` are readable and an active address resolves
pub fn preflight() -> Result<()> {
    // Verify git is recent enough before anything else — older gits silently
    // misbehave on `--end-of-options` and a few other flags we rely on.
    let _ = crate::git::check_version()?;

    let config = crate::config::load()?;
    let _ = config.active_network()?;
    let _ = config.package_id()?;
    let _ = config.registry_id()?;
    let _ = keystore::read_active_address(config.wallet_path.as_deref())?;
    Ok(())
}

/// Resolve the working directory's `.walgit/` and the loaded repo config.
///
/// Walks upward from CWD looking for a per-repo `.walgit/` directory. The
/// **global** config dir at `~/.walgit/` is explicitly skipped — without this,
/// a `walgit` command run from `$HOME` (or anywhere outside a repo) would
/// mistake the global config for a per-repo one and fail with a confusing
/// "missing field `name`" TOML error.
pub fn find_repo() -> Result<(PathBuf, PathBuf, LocalRepoConfig)> {
    let cwd = std::env::current_dir()?;
    let global_dir = crate::config::config_dir().ok();
    let mut p: &Path = &cwd;
    loop {
        let walgit = p.join(".walgit");
        let is_global = global_dir
            .as_ref()
            .map(|g| paths_match(&walgit, g))
            .unwrap_or(false);
        if walgit.exists() && !is_global {
            let cfg = load_repo_config(&walgit)?;
            return Ok((p.to_path_buf(), walgit, cfg));
        }
        match p.parent() {
            Some(parent) => p = parent,
            None => return Err(WalGitError::NotARepo),
        }
    }
}

fn paths_match(a: &Path, b: &Path) -> bool {
    // Compare canonical forms when possible so symlinked paths don't fool us.
    let ca = std::fs::canonicalize(a).ok();
    let cb = std::fs::canonicalize(b).ok();
    match (ca, cb) {
        (Some(a), Some(b)) => a == b,
        _ => a == b,
    }
}

pub fn require_registered(cfg: &LocalRepoConfig) -> Result<()> {
    if cfg.id.is_empty() || cfg.id == "pending" {
        return Err(WalGitError::RepoNotRegistered);
    }
    Ok(())
}
