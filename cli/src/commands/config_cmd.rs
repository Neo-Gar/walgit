// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

use crate::config::{load, save};
use crate::error::{Result, WalGitError};
use crate::ui;

#[allow(clippy::too_many_arguments)]
pub async fn run(
    network: Option<String>,
    package_id: Option<String>,
    graphql_url: Option<String>,
    publisher_url: Option<String>,
    aggregator_url: Option<String>,
    epochs: Option<u32>,
    show: bool,
) -> Result<()> {
    let mut cfg = load()?;

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
        if let Some(g) = graphql_url {
            net.sui.graphql_url = g;
        }
        if let Some(u) = publisher_url {
            net.walrus.publisher_url = u;
        }
        if let Some(u) = aggregator_url {
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
        ui::info(format!("active network: {}", cfg.network));
        ui::info(format!(
            "package_id:      {}",
            net.package_id.as_deref().unwrap_or("(unset)")
        ));
        ui::info(format!("sui graphql:     {}", net.sui.graphql_url));
        ui::info(format!("walrus pub:      {}", net.walrus.publisher_url));
        ui::info(format!("walrus agg:      {}", net.walrus.aggregator_url));
        ui::info(format!("epochs default:  {}", net.walrus.epochs));
    }
    Ok(())
}
