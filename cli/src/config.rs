// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

//! Global and per-repository configuration.
//!
//! Layout on disk:
//! - `~/.walgit/config.toml`        — global config (active network, network presets)
//! - `<repo>/.walgit/config.toml`   — per-repository cache (object IDs, push history)

use crate::error::{Result, WalGitError};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ─── Global config ────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    /// Active network key ("testnet", "mainnet", "devnet", "localnet", or custom).
    pub network: String,
    /// Path to a custom Sui keystore (default: ~/.sui/sui_config/sui.keystore).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wallet_path: Option<String>,
    /// Map of network presets keyed by network name.
    #[serde(default)]
    pub networks: HashMap<String, NetworkConfig>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct NetworkConfig {
    /// Deployed WalGit Sui package ID — required for any on-chain operation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_id: Option<String>,
    /// Shared `Registry` object id created by the package's `init`. Required
    /// for `create_repository` / `fork_repository` / `delete_repository`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry_id: Option<String>,
    pub sui: SuiConfig,
    pub walrus: WalrusConfig,
    pub seal: SealConfig,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SuiConfig {
    /// GraphQL endpoint for reads.
    pub graphql_url: String,
    /// JSON-RPC endpoint for transaction execution.
    pub rpc_url: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct WalrusConfig {
    pub publisher_url: String,
    pub aggregator_url: String,
    /// Default number of Walrus epochs to store blobs.
    #[serde(default = "default_epochs")]
    pub epochs: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SealConfig {
    /// Seal key server Sui object ID.
    pub key_server_id: String,
    /// Seal key server HTTP endpoint.
    pub key_server_url: String,
}

fn default_epochs() -> u32 {
    1
}

impl Default for Config {
    fn default() -> Self {
        let mut networks = HashMap::new();
        networks.insert("testnet".to_string(), NetworkConfig::testnet());
        networks.insert("mainnet".to_string(), NetworkConfig::mainnet());
        Self {
            network: "testnet".to_string(),
            wallet_path: None,
            networks,
        }
    }
}

impl NetworkConfig {
    pub fn testnet() -> Self {
        Self {
            package_id: None,
            registry_id: None,
            sui: SuiConfig {
                graphql_url: "https://graphql.testnet.sui.io/graphql".to_string(),
                rpc_url: "https://fullnode.testnet.sui.io:443".to_string(),
            },
            walrus: WalrusConfig {
                publisher_url: "https://publisher.walrus-testnet.walrus.space".to_string(),
                aggregator_url: "https://aggregator.walrus-testnet.walrus.space".to_string(),
                epochs: 1,
            },
            seal: SealConfig {
                key_server_id:
                    "0x73d05d62c18d9374e3ea529e8e0ed6161da1a141a94d3f76ae3fe4e99356db75"
                        .to_string(),
                key_server_url: "https://seal-key-server-testnet-1.mystenlabs.com".to_string(),
            },
        }
    }

    pub fn mainnet() -> Self {
        Self {
            package_id: None,
            registry_id: None,
            sui: SuiConfig {
                graphql_url: "https://graphql.sui.io/graphql".to_string(),
                rpc_url: "https://fullnode.mainnet.sui.io:443".to_string(),
            },
            walrus: WalrusConfig {
                publisher_url: "https://publisher.walrus.space".to_string(),
                aggregator_url: "https://aggregator.walrus.space".to_string(),
                epochs: 1,
            },
            seal: SealConfig {
                key_server_id: String::new(),
                key_server_url: String::new(),
            },
        }
    }
}

impl Config {
    /// Return the network config for the currently active network.
    pub fn active_network(&self) -> Result<&NetworkConfig> {
        self.networks.get(&self.network).ok_or_else(|| {
            WalGitError::config(format!(
                "active network '{}' has no entry under [networks.{}]",
                self.network, self.network
            ))
        })
    }

    pub fn active_network_mut(&mut self) -> Result<&mut NetworkConfig> {
        let key = self.network.clone();
        self.networks.get_mut(&key).ok_or_else(|| {
            WalGitError::config(format!(
                "active network '{}' has no entry under [networks.{}]",
                key, key
            ))
        })
    }

    pub fn package_id(&self) -> Result<&str> {
        self.active_network()?
            .package_id
            .as_deref()
            .ok_or_else(|| {
                WalGitError::config(format!(
                    "package_id not configured for network '{}'.\n\
                     Run: walgit config --package-id <PACKAGE_ID>",
                    self.network
                ))
            })
    }

    pub fn registry_id(&self) -> Result<&str> {
        self.active_network()?
            .registry_id
            .as_deref()
            .ok_or_else(|| {
                WalGitError::config(format!(
                    "registry_id not configured for network '{}'.\n\
                     Run: walgit config --registry-id <REGISTRY_ID>",
                    self.network
                ))
            })
    }
}

// ─── Paths ────────────────────────────────────────────────────────────────────

pub fn config_dir() -> Result<PathBuf> {
    let home = dirs::home_dir()
        .ok_or_else(|| WalGitError::config("cannot find home directory"))?;
    Ok(home.join(".walgit"))
}

pub fn config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

pub fn load() -> Result<Config> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(Config::default());
    }
    let content = std::fs::read_to_string(&path)?;
    let mut cfg: Config = toml::from_str(&content)?;
    // Backfill missing default network presets so users with old configs
    // don't have to re-init when we add new networks.
    cfg.networks
        .entry("testnet".to_string())
        .or_insert_with(NetworkConfig::testnet);
    cfg.networks
        .entry("mainnet".to_string())
        .or_insert_with(NetworkConfig::mainnet);
    Ok(cfg)
}

pub fn save(config: &Config) -> Result<()> {
    let dir = config_dir()?;
    std::fs::create_dir_all(&dir)?;
    let path = config_path()?;
    let content = toml::to_string_pretty(config)?;
    std::fs::write(&path, content)?;
    Ok(())
}

// ─── Per-repository config (.walgit/config.toml) ──────────────────────────────

/// Metadata cached inside a local repository's `.walgit/config.toml`.
///
/// Note: this is a CACHE — the authoritative state lives on-chain. The CLI
/// can rebuild this file from `walgit::sui::queries::get_repo_by_id` if lost.
#[derive(Serialize, Deserialize, Default, Debug, Clone)]
pub struct LocalRepoConfig {
    /// Human-readable repository name (must be URL-safe).
    pub name: String,
    /// Sui object ID for the Repository object, or `"pending"` if not yet registered.
    pub id: String,
    /// Sui object ID of the companion shared AccessControl object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acl_id: Option<String>,
    /// Network the repo lives on (matches a key under `[networks]` in global config).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    /// Whether this repository uses Seal IBE encryption.
    pub private: bool,
    /// Walrus storage epochs chosen at `walgit init` time.
    #[serde(default = "default_epochs")]
    pub epochs: u32,
    /// History of all local pushes (newest last).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pushes: Vec<PushRecord>,
    /// Sui object ID of the original repository this repo was forked from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forked_from: Option<String>,
    /// ACL object ID of the original repository (cached to avoid chain lookups).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forked_from_acl_id: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PushRecord {
    pub git_head: String,
    pub blob_id: String,
    pub branch: String,
    /// Sui Commit object ID created by this push.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub commit_id: String,
    /// Walrus epochs this blob was stored for.
    #[serde(default = "default_epochs")]
    pub epochs: u32,
    /// Unix timestamp (seconds) at push time — used to estimate storage expiry.
    #[serde(default)]
    pub pushed_at_secs: u64,
}

pub fn load_repo_config(walgit_dir: &Path) -> Result<LocalRepoConfig> {
    let p = walgit_dir.join("config.toml");
    let content = std::fs::read_to_string(&p)?;
    Ok(toml::from_str(&content)?)
}

pub fn save_repo_config(walgit_dir: &Path, cfg: &LocalRepoConfig) -> Result<()> {
    std::fs::create_dir_all(walgit_dir)?;
    let content = toml::to_string_pretty(cfg)?;
    std::fs::write(walgit_dir.join("config.toml"), content)?;
    Ok(())
}
