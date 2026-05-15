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

pub async fn grant(role: String, address: String) -> Result<()> {
    let (_, _, local) = find_repo()?;
    require_registered(&local)?;
    let acl_id = local
        .acl_id
        .as_deref()
        .ok_or_else(|| WalGitError::config("ACL id missing — re-initialize".to_string()))?;
    let ctx = CommandContext::load().await?;
    let kp = ctx.keypair()?;

    let pb = ui::spinner(format!("Granting {} access to {}…", role, address));
    let gas = ctx
        .sui
        .grant_access(&kp, &ctx.package_id, acl_id, &address, &role)
        .await?;
    pb.finish_and_clear();
    ui::success(format!("granted {} access to {}", role, address));
    ui::info(format!("gas: {}", gas.display()));
    Ok(())
}

pub async fn revoke(role: String, address: String) -> Result<()> {
    let (_, _, local) = find_repo()?;
    require_registered(&local)?;
    let acl_id = local
        .acl_id
        .as_deref()
        .ok_or_else(|| WalGitError::config("ACL id missing — re-initialize".to_string()))?;
    let ctx = CommandContext::load().await?;
    let kp = ctx.keypair()?;

    let pb = ui::spinner(format!("Revoking {} access from {}…", role, address));
    let gas = ctx
        .sui
        .revoke_access(&kp, &ctx.package_id, acl_id, &address, &role)
        .await?;
    pb.finish_and_clear();
    ui::success(format!("revoked {} access from {}", role, address));
    ui::info(format!("gas: {}", gas.display()));
    Ok(())
}
