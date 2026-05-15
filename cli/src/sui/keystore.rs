// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

//! Read Sui keystore (`~/.sui/sui_config/sui.keystore`) and client config
//! (`~/.sui/sui_config/client.yaml`) without invoking the Sui CLI subprocess.
//!
//! Supports Ed25519 keys (flag byte 0x00). Other key schemes (Secp256k1,
//! Secp256r1) are intentionally rejected — WalGit uses Ed25519 for both
//! transaction signing and Seal session keys.

use crate::error::{Result, WalGitError};
use base64::Engine as _;
use blake2::{Blake2b, Digest, digest::consts::U32};
use std::path::PathBuf;

/// Default Sui keystore location.
pub fn default_keystore_path() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("SUI_KEYSTORE_PATH") {
        return Ok(PathBuf::from(p));
    }
    let home = dirs::home_dir()
        .ok_or_else(|| WalGitError::config("cannot determine home directory"))?;
    Ok(home.join(".sui").join("sui_config").join("sui.keystore"))
}

/// Default Sui client.yaml location.
pub fn default_client_yaml_path() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("SUI_CLIENT_CONFIG") {
        return Ok(PathBuf::from(p));
    }
    let home = dirs::home_dir()
        .ok_or_else(|| WalGitError::config("cannot determine home directory"))?;
    Ok(home.join(".sui").join("sui_config").join("client.yaml"))
}

/// Parse `client.yaml` and return the `active_address` field. This avoids
/// shelling out to `sui client active-address`.
pub fn read_active_address(custom_path: Option<&str>) -> Result<String> {
    let path = match custom_path {
        Some(p) => PathBuf::from(p),
        None => default_client_yaml_path()?,
    };
    if !path.exists() {
        return Err(WalGitError::WalletNotFound(path.display().to_string()));
    }
    let content = std::fs::read_to_string(&path)?;
    for line in content.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("active_address:") {
            let val = rest.trim().trim_matches('"').trim_matches('\'');
            if !val.is_empty() {
                return Ok(val.to_string());
            }
        }
    }
    Err(WalGitError::config(format!(
        "active_address field not found in {}",
        path.display()
    )))
}

pub struct KeyPair {
    pub private_key: [u8; 32],
    pub public_key: [u8; 32],
    pub address: String,
}

/// Load the keypair for `address` from the user's Sui keystore.
pub fn load_keypair(address: &str, custom_keystore: Option<&str>) -> Result<KeyPair> {
    let path = match custom_keystore {
        Some(p) => PathBuf::from(p),
        None => default_keystore_path()?,
    };
    if !path.exists() {
        return Err(WalGitError::WalletNotFound(path.display().to_string()));
    }
    let content = std::fs::read_to_string(&path)?;
    let entries: Vec<String> = serde_json::from_str(&content)?;

    let target_bytes = hex::decode(address.trim_start_matches("0x"))
        .map_err(|_| WalGitError::config(format!("invalid Sui address hex: {}", address)))?;

    for entry in &entries {
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(entry)
            .map_err(|e| WalGitError::config(format!("base64 decode failed: {}", e)))?;

        // Sui keystore: [flag_byte | key_bytes...]. Ed25519 flag = 0x00, 32-byte private key.
        if decoded.len() < 33 || decoded[0] != 0x00 {
            continue;
        }
        let private_bytes: [u8; 32] = decoded[1..33].try_into().unwrap();
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&private_bytes);
        let public_bytes: [u8; 32] = signing_key.verifying_key().to_bytes();

        let derived = derive_sui_address(&public_bytes);
        if derived.as_slice() == target_bytes.as_slice() {
            return Ok(KeyPair {
                private_key: private_bytes,
                public_key: public_bytes,
                address: address.to_string(),
            });
        }
    }
    Err(WalGitError::KeyNotFound(address.to_string()))
}

/// Sui address = Blake2b-256([flag_byte] ++ pubkey). For Ed25519, flag = 0x00.
fn derive_sui_address(pubkey: &[u8; 32]) -> Vec<u8> {
    let mut hasher = Blake2b::<U32>::new();
    Digest::update(&mut hasher, [0x00u8]);
    Digest::update(&mut hasher, pubkey);
    hasher.finalize().to_vec()
}
