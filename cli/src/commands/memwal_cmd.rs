// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

//! `walgit memwal` — manage the user's MemWal delegate keypair and the
//! on-chain `MemWalAccount.delegate_keys` table.
//!
//! Two roles operate here:
//!
//! - **Self** (every walgit user): generate a local Ed25519 delegate keypair,
//!   surface the public part for the repo owner to register. Local writes
//!   through `walgit trace upload` and `git push` then sign with this key.
//! - **Owner** of a `MemWalAccount`: call into the MemWal Move contract to
//!   register collaborators' public keys as delegates (or remove them).
//!   Without this step a collaborator's writes get rejected by the relayer.

use crate::commands::CommandContext;
use crate::config::{self, Config, MemWalConfig};
use crate::error::{Result, WalGitError};
use crate::memwal;
use crate::sui::keystore;
use crate::ui;
use console::style;
use std::path::PathBuf;

/// `walgit memwal init` — generate a fresh Ed25519 delegate keypair, save the
/// private half to disk with 0600 perms, write or refresh the `[memwal]`
/// section in the global config, and print the public part the repo owner
/// needs to register.
pub async fn init(force: bool, account_id: Option<String>, relayer_url: Option<String>) -> Result<()> {
    let mut cfg = config::load()?;

    let key_path = default_delegate_key_path()?;

    if key_path.exists() && !force {
        ui::warn(format!(
            "delegate key already exists at {} — pass --force to overwrite",
            key_path.display()
        ));
        // Still allow the user to update the rest of the [memwal] section even
        // if they don't want to regenerate the key. This is the common case:
        // user re-runs `walgit memwal init --account-id 0x...` to point at a
        // different MemWalAccount without rotating their own keypair.
        if account_id.is_some() || relayer_url.is_some() {
            apply_config_updates(&mut cfg, account_id, relayer_url, &key_path)?;
            config::save(&cfg)?;
            ui::success("config updated (key untouched)");
        }
        print_status_summary(&cfg).await?;
        return Ok(());
    }

    use rand::rngs::OsRng;
    let signing_key = ed25519_dalek::SigningKey::generate(&mut OsRng);
    let priv_hex = hex::encode(signing_key.to_bytes());
    let pub_hex = hex::encode(signing_key.verifying_key().to_bytes());

    // Write key file with restrictive perms BEFORE persisting config so a
    // crash mid-init never leaves a config pointing at a missing key.
    write_secret(&key_path, &priv_hex)?;
    apply_config_updates(&mut cfg, account_id, relayer_url, &key_path)?;
    config::save(&cfg)?;

    let sui_addr = keystore::read_active_address(cfg.wallet_path.as_deref())
        .unwrap_or_else(|_| "(no Sui wallet configured)".into());

    ui::header("memwal init — share these with the repo owner");
    println!("  {} {}", ui::label("public_key"), ui::highlight(&pub_hex));
    println!("  {} {}", ui::label("sui_addr  "), ui::highlight(&sui_addr));
    println!();
    ui::info(format!(
        "delegate private key saved to {} (0600)",
        key_path.display()
    ));
    if cfg.memwal.as_ref().and_then(|m| Some(m.account_id.is_empty())).unwrap_or(true)
        || cfg.memwal.as_ref().map(|m| m.account_id.is_empty()).unwrap_or(true)
    {
        ui::warn("memwal.account_id not set in config — pass --account-id <0x...> or set it manually");
    }
    println!();
    ui::info("the repo owner adds you on-chain with:");
    println!(
        "    {} walgit memwal add-delegate {} {} {}",
        style("$").dim(),
        pub_hex,
        sui_addr,
        style("--label \"My Laptop\"").dim()
    );
    Ok(())
}

/// `walgit memwal status` — show local delegate identity, configured account,
/// and (best-effort) whether we're currently registered on chain.
pub async fn status() -> Result<()> {
    let cfg = config::load()?;
    print_status_summary(&cfg).await
}

/// `walgit memwal list` — read `delegate_keys` from the configured
/// `MemWalAccount` on chain.
pub async fn list() -> Result<()> {
    let cfg = config::load()?;
    let mw = cfg
        .memwal
        .as_ref()
        .ok_or_else(|| WalGitError::config("memwal not configured (run `walgit memwal init`)"))?;

    let ctx = CommandContext::load().await?;
    let delegates = ctx.sui.memwal_get_delegates(&mw.account_id).await?;
    ui::header(&format!(
        "delegates on {} ({})",
        ui::short_id(&mw.account_id),
        delegates.len()
    ));
    if delegates.is_empty() {
        ui::info("no delegates registered");
        return Ok(());
    }
    let my_pubkey = mw.load_delegate_key().ok().map(|k| {
        hex::encode(ed25519_dalek::SigningKey::from_bytes(&k).verifying_key().to_bytes())
    });
    for d in &delegates {
        let mark = if Some(&d.public_key_hex) == my_pubkey.as_ref() {
            style("●").green().bold().to_string()
        } else {
            style("·").dim().to_string()
        };
        println!(
            "  {} {} {}",
            mark,
            ui::highlight(&d.label),
            ui::dim(&format!("({})", &d.public_key_hex[..16]))
        );
        println!("      {}", ui::dim(&d.sui_address));
    }
    Ok(())
}

/// `walgit memwal add-delegate <pubkey-hex> <sui-addr> --label <s>` — owner-only.
/// Calls the MemWal Move contract to register the delegate.
pub async fn add_delegate(
    pubkey_hex: String,
    sui_address: String,
    label: Option<String>,
) -> Result<()> {
    let cfg = config::load()?;
    let mw = cfg
        .memwal
        .as_ref()
        .ok_or_else(|| WalGitError::config("memwal not configured (run `walgit memwal init`)"))?;
    let pkg = memwal::package_id_for_network(&cfg.network).ok_or_else(|| {
        WalGitError::config(format!(
            "no MemWal package_id known for network '{}' (only testnet/mainnet supported)",
            cfg.network
        ))
    })?;
    let pubkey_bytes = parse_pubkey_hex(&pubkey_hex)?;
    let label = label.unwrap_or_else(|| "walgit".to_string());

    let ctx = CommandContext::load().await?;
    let kp = ctx.keypair()?;
    let pb = ui::spinner(format!("Registering delegate {}…", &pubkey_hex[..16]));
    let gas = ctx
        .sui
        .memwal_add_delegate(&kp, pkg, &mw.account_id, &pubkey_bytes, &sui_address, &label)
        .await?;
    pb.finish_and_clear();
    ui::success(format!("delegate '{}' added on chain", label));
    ui::info(format!("gas: {}", gas.display()));
    Ok(())
}

/// `walgit memwal remove-delegate <pubkey-hex>` — owner-only.
pub async fn remove_delegate(pubkey_hex: String) -> Result<()> {
    let cfg = config::load()?;
    let mw = cfg
        .memwal
        .as_ref()
        .ok_or_else(|| WalGitError::config("memwal not configured"))?;
    let pkg = memwal::package_id_for_network(&cfg.network).ok_or_else(|| {
        WalGitError::config(format!("no MemWal package_id known for '{}'", cfg.network))
    })?;
    let pubkey_bytes = parse_pubkey_hex(&pubkey_hex)?;

    let ctx = CommandContext::load().await?;
    let kp = ctx.keypair()?;
    let pb = ui::spinner(format!("Removing delegate {}…", &pubkey_hex[..16]));
    let gas = ctx
        .sui
        .memwal_remove_delegate(&kp, pkg, &mw.account_id, &pubkey_bytes)
        .await?;
    pb.finish_and_clear();
    ui::success("delegate removed");
    ui::info(format!("gas: {}", gas.display()));
    Ok(())
}

// ─── helpers ────────────────────────────────────────────────────────────────

fn default_delegate_key_path() -> Result<PathBuf> {
    Ok(config::config_dir()?.join("memwal-delegate.key"))
}

fn write_secret(path: &PathBuf, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, format!("{}\n", content))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(path, perms)?;
    }
    Ok(())
}

fn apply_config_updates(
    cfg: &mut Config,
    account_id: Option<String>,
    relayer_url: Option<String>,
    key_path: &PathBuf,
) -> Result<()> {
    let path_str = key_path.display().to_string();
    // Normalise to `~/...` form so the saved config doesn't bake in a
    // user-specific absolute path that wouldn't roundtrip across machines.
    let tilde_path = match dirs::home_dir() {
        Some(home) if key_path.starts_with(&home) => {
            let rest = key_path.strip_prefix(&home).unwrap();
            format!("~/{}", rest.display())
        }
        _ => path_str,
    };

    let mut next = cfg.memwal.clone().unwrap_or_else(|| MemWalConfig {
        account_id: String::new(),
        relayer_url: default_relayer_url(&cfg.network).to_string(),
        delegate_key_hex: None,
        delegate_key_path: Some(tilde_path.clone()),
    });
    if let Some(a) = account_id {
        next.account_id = a;
    }
    if let Some(u) = relayer_url {
        next.relayer_url = u;
    }
    next.delegate_key_path = Some(tilde_path);
    next.delegate_key_hex = None; // explicit path overrides inline hex
    cfg.memwal = Some(next);
    Ok(())
}

fn default_relayer_url(network: &str) -> &'static str {
    match network {
        "mainnet" => "https://relayer.memwal.ai",
        _ => "https://relayer.staging.memwal.ai",
    }
}

fn parse_pubkey_hex(s: &str) -> Result<[u8; 32]> {
    let trimmed = s.trim().trim_start_matches("0x");
    let bytes = hex::decode(trimmed)
        .map_err(|e| WalGitError::other(format!("bad public key hex: {}", e)))?;
    bytes.try_into().map_err(|v: Vec<u8>| {
        WalGitError::other(format!("public key must be 32 bytes, got {}", v.len()))
    })
}

async fn print_status_summary(cfg: &Config) -> Result<()> {
    ui::header("memwal status");
    let Some(mw) = &cfg.memwal else {
        ui::warn("memwal is not configured — run `walgit memwal init`");
        return Ok(());
    };
    println!(
        "  {} {}",
        ui::label("account  "),
        ui::highlight(&mw.account_id)
    );
    println!("  {} {}", ui::label("relayer  "), ui::dim(&mw.relayer_url));
    if let Some(p) = &mw.delegate_key_path {
        println!("  {} {}", ui::label("key path "), ui::dim(p));
    }
    match mw.load_delegate_key() {
        Ok(k) => {
            let pub_hex =
                hex::encode(ed25519_dalek::SigningKey::from_bytes(&k).verifying_key().to_bytes());
            println!("  {} {}", ui::label("pubkey   "), ui::highlight(&pub_hex));

            // Best-effort: check whether our pubkey is on chain. Failure is
            // non-fatal (offline status should still print local state).
            if let Ok(ctx) = CommandContext::load().await {
                match ctx.sui.memwal_get_delegates(&mw.account_id).await {
                    Ok(delegates) => {
                        let registered = delegates.iter().any(|d| d.public_key_hex == pub_hex);
                        if registered {
                            ui::success("this delegate IS registered on chain");
                        } else {
                            ui::warn(
                                "this delegate is NOT registered on chain — ask the account owner to run `walgit memwal add-delegate`",
                            );
                        }
                    }
                    Err(e) => ui::info(format!("chain check skipped ({})", e)),
                }
            }
        }
        Err(e) => {
            ui::warn(format!("delegate key unreadable: {}", e));
        }
    }
    Ok(())
}
