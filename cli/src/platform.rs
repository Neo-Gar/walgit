// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

//! Platform (sponsored) network resolution.
//!
//! When `[storage] sponsored = true`, the active network's contract + endpoint
//! parameters are taken from the WalGit platform backend instead of the local
//! config — so users on the platform never hand-set `package_id`/`registry_id`,
//! and the platform can rotate contracts centrally. Standalone users leave
//! `sponsored = false` and configure their own deployment.
//!
//! Resolved values overlay the in-memory `Config` only (the on-disk config is
//! left untouched); a short-TTL on-disk cache avoids a network round-trip on
//! every command.

use crate::config::{Config, config_dir};
use crate::error::{Result, WalGitError};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_API: &str = "https://api.walgit.com";
const CACHE_TTL_SECS: u64 = 6 * 3600;

/// Backend base URL. Hardcoded to the platform API; `WALGIT_BACKEND` overrides
/// it for local development against a self-hosted backend.
fn api_base() -> String {
    std::env::var("WALGIT_BACKEND").unwrap_or_else(|_| DEFAULT_API.to_string())
}

/// Network parameters served by `GET {api}/v1/networks/{network}`.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct NetworkParams {
    pub package_id: String,
    pub registry_id: String,
    #[serde(default)]
    pub publisher_url: Option<String>,
    #[serde(default)]
    pub aggregator_url: Option<String>,
    #[serde(default)]
    pub graphql_url: Option<String>,
    #[serde(default)]
    pub seal_key_server_id: Option<String>,
    #[serde(default)]
    pub seal_key_server_url: Option<String>,
    #[serde(default)]
    pub epochs: Option<u32>,
}

/// Overlay sponsored network params onto `config` when sponsored mode is on.
/// No-op otherwise. Errors only when sponsored but neither the backend nor a
/// cached entry can supply the params.
pub async fn resolve(config: &mut Config) -> Result<()> {
    if !config.storage.sponsored {
        return Ok(());
    }
    let network = config.network.clone();
    let params = fetch_or_cache(&network).await?;
    overlay(config, &params)
}

async fn fetch_or_cache(network: &str) -> Result<NetworkParams> {
    let now = now_secs();
    let mut cache = load_cache();
    if let Some(e) = cache.get(network) {
        if now.saturating_sub(e.fetched_at) < CACHE_TTL_SECS {
            return Ok(e.params.clone());
        }
    }
    match fetch(network).await {
        Ok(params) => {
            cache.insert(
                network.to_string(),
                CacheEntry {
                    params: params.clone(),
                    fetched_at: now,
                },
            );
            save_cache(&cache);
            Ok(params)
        }
        Err(e) => match cache.get(network) {
            Some(stale) => {
                eprintln!(
                    "walgit: sponsored backend unreachable ({}); using cached network params",
                    e
                );
                Ok(stale.params.clone())
            }
            None => Err(WalGitError::config(format!(
                "sponsored mode: could not fetch network '{}' from {} and no cache available: {}",
                network,
                api_base(),
                e
            ))),
        },
    }
}

async fn fetch(network: &str) -> Result<NetworkParams> {
    let url = format!("{}/v1/networks/{}", api_base(), network);
    let resp = reqwest::Client::new()
        .get(&url)
        .send()
        .await
        .map_err(|e| WalGitError::other(format!("GET {}: {}", url, e)))?;
    if !resp.status().is_success() {
        return Err(WalGitError::other(format!(
            "{} returned HTTP {}",
            url,
            resp.status()
        )));
    }
    resp.json::<NetworkParams>()
        .await
        .map_err(|e| WalGitError::other(format!("parsing network params: {}", e)))
}

fn overlay(config: &mut Config, p: &NetworkParams) -> Result<()> {
    let net = config.active_network_mut()?;
    net.package_id = Some(p.package_id.clone());
    net.registry_id = Some(p.registry_id.clone());
    if let Some(u) = &p.publisher_url {
        net.walrus.publisher_url = u.clone();
    }
    if let Some(u) = &p.aggregator_url {
        net.walrus.aggregator_url = u.clone();
    }
    if let Some(u) = &p.graphql_url {
        net.sui.graphql_url = u.clone();
    }
    if let Some(s) = &p.seal_key_server_id {
        net.seal.key_server_id = s.clone();
    }
    if let Some(s) = &p.seal_key_server_url {
        net.seal.key_server_url = s.clone();
    }
    if let Some(e) = p.epochs {
        net.walrus.epochs = e;
    }
    Ok(())
}

// ─── cache ──────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone)]
struct CacheEntry {
    params: NetworkParams,
    fetched_at: u64,
}

type Cache = HashMap<String, CacheEntry>;

fn cache_path() -> Option<std::path::PathBuf> {
    config_dir().ok().map(|d| d.join("sponsored-cache.json"))
}

fn load_cache() -> Cache {
    cache_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_cache(cache: &Cache) {
    if let Some(p) = cache_path() {
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(s) = serde_json::to_string_pretty(cache) {
            let _ = std::fs::write(p, s);
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
