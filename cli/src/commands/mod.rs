// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

pub mod access;
pub mod config_cmd;
pub mod fork;
pub mod init;
pub mod log;
pub mod pr;
pub mod status;

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
    pub sui: SuiClient,
    pub walrus: WalrusClient,
    pub active_address: String,
}

impl CommandContext {
    pub async fn load() -> Result<Self> {
        let config = crate::config::load()?;
        let net = config.active_network()?.clone();
        let package_id = config.package_id()?.to_string();
        let sui = SuiClient::new(net.sui.graphql_url.clone())?;
        let walrus = WalrusClient::new(net.walrus.publisher_url, net.walrus.aggregator_url);
        let active_address = keystore::read_active_address(config.wallet_path.as_deref())?;
        Ok(Self {
            config,
            package_id,
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

/// Resolve the working directory's `.walgit/` and the loaded repo config.
pub fn find_repo() -> Result<(PathBuf, PathBuf, LocalRepoConfig)> {
    let cwd = std::env::current_dir()?;
    let mut p: &Path = &cwd;
    loop {
        let walgit = p.join(".walgit");
        if walgit.exists() {
            let cfg = load_repo_config(&walgit)?;
            return Ok((p.to_path_buf(), walgit, cfg));
        }
        match p.parent() {
            Some(parent) => p = parent,
            None => return Err(WalGitError::NotARepo),
        }
    }
}

pub fn require_registered(cfg: &LocalRepoConfig) -> Result<()> {
    if cfg.id.is_empty() || cfg.id == "pending" {
        return Err(WalGitError::RepoNotRegistered);
    }
    Ok(())
}
