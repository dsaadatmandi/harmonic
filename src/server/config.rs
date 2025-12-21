use std::net::SocketAddr;

use anyhow::{Context, Result};

use crate::{Config, sync};



pub fn create_server_config() -> Result<Config> {
    let config = sync::load_config().context("Failed to load config")?;

    let addr: SocketAddr = config.socket_addr.parse().context("Failed to parse socket address")?;

    let socket_addr = String::from("0.0.0.0:") + &addr.port().to_string();
    Ok(Config {
        socket_addr,
        ..config
    })

}