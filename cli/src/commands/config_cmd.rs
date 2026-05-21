// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

use crate::config::{load, save};
use crate::error::{Result, WalGitError};
use crate::ui;

/// Validate a URL supplied via `walgit config --*-url`. Warns when plain HTTP
/// is used outside localhost (SSRF risk; delegate key exposed in transit).
fn validate_url(url: &str, flag: &str) -> Result<()> {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(WalGitError::config(format!(
            "{} must start with http:// or https://",
            flag
        )));
    }
    if url.starts_with("http://") {
        let is_local = url.starts_with("http://localhost")
            || url.starts_with("http://127.0.0.1")
            || url.starts_with("http://[::1]");
        if !is_local {
            eprintln!(
                "walgit warning: {} '{}' uses plain HTTP. \
                 Credentials and data will be sent unencrypted. \
                 Use HTTPS for production deployments.",
                flag, url
            );
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    network: Option<String>,
    package_id: Option<String>,
    registry_id: Option<String>,
    graphql_url: Option<String>,
    publisher_url: Option<String>,
    aggregator_url: Option<String>,
    epochs: Option<u32>,
    short_ids: Option<bool>,
    betterleaks_skip: Option<bool>,
    show: bool,
) -> Result<()> {
    let mut cfg = load()?;

    if let Some(v) = short_ids {
        cfg.display.short_ids = v;
    }

    if let Some(v) = betterleaks_skip {
        cfg.betterleaks.skip = v;
    }

    if let Some(net) = network {
        if !cfg.networks.contains_key(&net) {
            return Err(WalGitError::config(format!(
                "network '{}' is not defined under [networks]",
                net
            )));
        }
        cfg.network = net;
    }

    if package_id.is_some()
        || registry_id.is_some()
        || graphql_url.is_some()
        || publisher_url.is_some()
        || aggregator_url.is_some()
        || epochs.is_some()
    {
        let active = cfg.network.clone();
        let net = cfg
            .networks
            .get_mut(&active)
            .ok_or_else(|| WalGitError::config(format!("active network '{}' missing", active)))?;
        if let Some(p) = package_id {
            net.package_id = Some(p);
        }
        if let Some(r) = registry_id {
            net.registry_id = Some(r);
        }
        if let Some(g) = graphql_url {
            validate_url(&g, "--graphql-url")?;
            net.sui.graphql_url = g;
        }
        if let Some(u) = publisher_url {
            validate_url(&u, "--publisher-url")?;
            net.walrus.publisher_url = u;
        }
        if let Some(u) = aggregator_url {
            validate_url(&u, "--aggregator-url")?;
            net.walrus.aggregator_url = u;
        }
        if let Some(e) = epochs {
            net.walrus.epochs = e;
        }
    }

    save(&cfg)?;

    if show {
        let s = toml::to_string_pretty(&cfg)?;
        println!("{}", s);
    } else {
        ui::success("config updated");
        let net = cfg.active_network()?;
        ui::header("active network");
        println!(
            "  {} {}",
            ui::label("network    "),
            ui::highlight(&cfg.network)
        );
        println!(
            "  {} {}",
            ui::label("package_id "),
            net.package_id
                .as_deref()
                .map(ui::highlight)
                .unwrap_or_else(|| ui::dim("(unset)"))
        );
        println!(
            "  {} {}",
            ui::label("registry_id"),
            net.registry_id
                .as_deref()
                .map(ui::highlight)
                .unwrap_or_else(|| ui::dim("(unset)"))
        );
        println!("  {} {}", ui::label("sui graphql"), net.sui.graphql_url);
        println!(
            "  {} {}",
            ui::label("walrus pub "),
            net.walrus.publisher_url
        );
        println!(
            "  {} {}",
            ui::label("walrus agg "),
            net.walrus.aggregator_url
        );
        println!("  {} {}", ui::label("epochs     "), net.walrus.epochs);
        ui::header("display");
        println!(
            "  {} {}",
            ui::label("short_ids  "),
            if cfg.display.short_ids {
                ui::highlight("on")
            } else {
                ui::dim("off (full IDs)")
            }
        );
        ui::header("betterleaks");
        println!(
            "  {} {}",
            ui::label("scanning   "),
            if cfg.betterleaks.skip {
                ui::dim("disabled (`walgit config --betterleaks enable` to restore) ")
            } else {
                ui::highlight("enabled")
            }
        );
    }
    Ok(())
}
