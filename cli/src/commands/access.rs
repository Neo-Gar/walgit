// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

use crate::commands::{CommandContext, find_repo, require_registered};
use crate::error::{Result, WalGitError};
use crate::ui;

pub async fn list() -> Result<()> {
    let (_, _, local) = find_repo()?;
    require_registered(&local)?;
    let acl_id = local
        .acl_id
        .as_deref()
        .ok_or_else(|| WalGitError::config("ACL id missing — re-initialize".to_string()))?;

    let ctx = CommandContext::load().await?;
    let acl = ctx.sui.get_access_control(acl_id).await?;

    ui::header("access");
    println!("  {} {}", ui::label("owner  "), ui::highlight(&acl.owner));

    println!();
    println!(
        "  {} ({})",
        ui::label("readers"),
        acl.allowed_readers.len()
    );
    if acl.allowed_readers.is_empty() {
        println!("    {}", ui::dim("(none)"));
    } else {
        for r in &acl.allowed_readers {
            println!("    {} {}", ui::dim("·"), r);
        }
    }
    println!();
    println!(
        "  {} ({})",
        ui::label("writers"),
        acl.allowed_writers.len()
    );
    if acl.allowed_writers.is_empty() {
        println!("    {}", ui::dim("(none)"));
    } else {
        for w in &acl.allowed_writers {
            println!("    {} {}", ui::dim("·"), w);
        }
    }
    Ok(())
}

pub async fn grant(
    role: String,
    address: String,
    memwal_pubkey: Option<String>,
    memwal_label: String,
) -> Result<()> {
    let (_, _, local) = find_repo()?;
    require_registered(&local)?;
    let acl_id = local
        .acl_id
        .as_deref()
        .ok_or_else(|| WalGitError::config("ACL id missing — re-initialize".to_string()))?;
    let ctx = CommandContext::load().await?;
    let kp = ctx.keypair()?;

    let pb = ui::spinner(format!("Granting {} access to {}…", role, address));
    let mut gas = ctx
        .sui
        .grant_access(&kp, &ctx.package_id, acl_id, &address, &role)
        .await?;
    pb.finish_and_clear();
    ui::success(format!("granted {} access to {}", role, address));

    // Optional: also register the collaborator's MemWal delegate key so
    // their trace pushes pass the relayer's signature check. We do this
    // best-effort AFTER the walgit grant — if it fails, the ACL change is
    // still done, and the owner can retry with `walgit memwal add-delegate`.
    if let Some(pubkey_hex) = memwal_pubkey {
        let cfg = crate::config::load()?;
        let mw = cfg.memwal.as_ref().ok_or_else(|| {
            WalGitError::config(
                "[memwal] not configured — set it before passing --memwal-pubkey",
            )
        })?;
        let pkg = crate::memwal::package_id_for_network(&cfg.network).ok_or_else(|| {
            WalGitError::config(format!("no MemWal package_id known for '{}'", cfg.network))
        })?;
        let pubkey_bytes = parse_pubkey_hex(&pubkey_hex)?;

        let pb = ui::spinner(format!("Registering MemWal delegate {}…", &pubkey_hex[..16]));
        let mw_gas = ctx
            .sui
            .memwal_add_delegate(&kp, pkg, &mw.account_id, &pubkey_bytes, &address, &memwal_label)
            .await?;
        pb.finish_and_clear();
        ui::success(format!(
            "MemWal delegate '{}' registered for {}",
            memwal_label, address
        ));
        gas = gas + mw_gas;
    }

    ui::info(format!("gas: {}", gas.display()));
    Ok(())
}

pub async fn revoke(
    role: String,
    address: String,
    memwal_pubkey: Option<String>,
) -> Result<()> {
    let (_, _, local) = find_repo()?;
    require_registered(&local)?;
    let acl_id = local
        .acl_id
        .as_deref()
        .ok_or_else(|| WalGitError::config("ACL id missing — re-initialize".to_string()))?;
    let ctx = CommandContext::load().await?;
    let kp = ctx.keypair()?;

    let pb = ui::spinner(format!("Revoking {} access from {}…", role, address));
    let mut gas = ctx
        .sui
        .revoke_access(&kp, &ctx.package_id, acl_id, &address, &role)
        .await?;
    pb.finish_and_clear();
    ui::success(format!("revoked {} access from {}", role, address));

    if let Some(pubkey_hex) = memwal_pubkey {
        let cfg = crate::config::load()?;
        let mw = cfg.memwal.as_ref().ok_or_else(|| {
            WalGitError::config(
                "[memwal] not configured — set it before passing --memwal-pubkey",
            )
        })?;
        let pkg = crate::memwal::package_id_for_network(&cfg.network).ok_or_else(|| {
            WalGitError::config(format!("no MemWal package_id known for '{}'", cfg.network))
        })?;
        let pubkey_bytes = parse_pubkey_hex(&pubkey_hex)?;

        let pb = ui::spinner(format!("Removing MemWal delegate {}…", &pubkey_hex[..16]));
        let mw_gas = ctx
            .sui
            .memwal_remove_delegate(&kp, pkg, &mw.account_id, &pubkey_bytes)
            .await?;
        pb.finish_and_clear();
        ui::success("MemWal delegate removed");
        gas = gas + mw_gas;
    }

    ui::info(format!("gas: {}", gas.display()));
    Ok(())
}

fn parse_pubkey_hex(s: &str) -> Result<[u8; 32]> {
    let trimmed = s.trim().trim_start_matches("0x");
    let bytes = hex::decode(trimmed)
        .map_err(|e| WalGitError::other(format!("bad public key hex: {}", e)))?;
    bytes.try_into().map_err(|v: Vec<u8>| {
        WalGitError::other(format!("public key must be 32 bytes, got {}", v.len()))
    })
}
