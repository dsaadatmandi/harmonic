use serde::{Deserialize, Serialize};
use std::io::ErrorKind;
use std::path::PathBuf;
use std::str::FromStr;
use std::{fs, io};
use tracing::{debug, error, info};
use tracing_core::Level;

use crate::utils::{HarmonicError, Result};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    pub sync_path: PathBuf,
    pub socket_addr: String,
    pub schedule_delay: u64,
    pub log_level: String,

    pub sync_threshold: u64,
    pub modify_weight: u64,
    pub remove_weight: u64,
    pub create_weight: u64,

    pub block_size: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            sync_path: PathBuf::from(
                dirs::home_dir().expect("Determination of home dir should never fail"),
            ),
            socket_addr: String::from("[::1]:42069"),
            schedule_delay: 3600,
            log_level: String::from("info"),
            sync_threshold: 20,
            modify_weight: 2,
            remove_weight: 5,
            create_weight: 10,
            block_size: 8192,
        }
    }
}

impl Config {
    pub fn server_uri(&self) -> String {
        format!("http://{}", self.socket_addr)
    }
}

pub fn config_dir_path() -> Result<PathBuf> {
    // let mut path = dirs::config_dir().ok_or(HarmonicError::ConfigError)?;
    let mut path = PathBuf::from(".");
    path.push(".harmonic");

    debug!(?path, "Config path");
    Ok(path)
}

fn config_file_path() -> Result<PathBuf> {
    let mut path = config_dir_path()?;
    path.push("config.toml");

    Ok(path)
}

fn save_config(config: Config) -> Result<()> {
    let config_toml = toml::to_string(&config)?;

    debug!("Writing config file to {:?}", config_file_path());
    fs::DirBuilder::new()
        .recursive(true)
        .create(config_dir_path()?)?;

    fs::write(config_file_path()?, config_toml)?;

    Ok(())
}

pub fn load_config() -> Result<Config> {
    info!("Loading config.");
    let config: Config = match fs::read_to_string(config_file_path()?) {
        Ok(config_toml) => Ok(toml::from_str(&config_toml)?),
        Err(error) => match error.kind() {
            ErrorKind::NotFound => {
                info!("Config file not found. Creating with default values.");
                let c = handle_no_config()?;
                info!("Config created. Continuing.");
                Ok(c)
            }
            _ => Err(HarmonicError::Io(error)),
        },
    }?;

    if let Err(_) = Level::from_str(&config.log_level) {
        error!("Config level was not a valid selection of: trace, debug, info, warn, error");
        return Err(HarmonicError::ConfigError);
    };

    Ok(config)
}

fn handle_no_config() -> Result<Config> {
    let mut path = String::new();
    let mut address = String::new();
    println!("No config found! Creating new file...");
    println!("Please enter the path you would like to Harmonize:");
    io::stdin()
        .read_line(&mut path)
        .expect("Failed to read input for path");

    // how about this just depends on which binary will be compiled
    println!(
        "Please enter the address your server should listen on / your client should connect to:"
    );
    println!("Valid formats include: [::1]:PORT, IP:PORT");
    io::stdin()
        .read_line(&mut address)
        .expect("Failed to read input for address");

    let mut c = Config::default();
    c.sync_path = PathBuf::from(path.trim());
    c.socket_addr = address.trim().to_string();

    println!("Saving config to: {:?}", config_dir_path());
    println!("Please review the config file to find additional configurable properties");
    save_config(c.clone())?;

    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_dir_path() {
        let path = config_dir_path().unwrap();
        assert!(path.ends_with(".harmonic"));
    }

    #[test]
    fn test_config_file_path() {
        let path = config_file_path().unwrap();
        assert!(path.ends_with(".harmonic/config.toml"));
    }

    #[test]
    fn test_server_uri_ipv4() {
        let config = Config {
            sync_path: PathBuf::from("/tmp/test"),
            socket_addr: String::from("192.168.1.100:42069"),
            log_level: String::from("debug"),
            schedule_delay: 3600,
            sync_threshold: 20,
            modify_weight: 2,
            remove_weight: 5,
            create_weight: 10,
            block_size: 8192,
        };

        let uri = config.server_uri();
        assert_eq!(uri, "http://192.168.1.100:42069");

        // Verify the original socket_addr can still be parsed as SocketAddr
        let socket_addr: std::net::SocketAddr = config.socket_addr.parse().unwrap();
        assert_eq!(socket_addr.port(), 42069);
    }

    #[test]
    fn test_server_uri_ipv6() {
        let config = Config {
            sync_path: PathBuf::from("/tmp/test"),
            socket_addr: String::from("[::1]:42069"),
            schedule_delay: 3600,
            log_level: String::from("debug"),
            sync_threshold: 20,
            modify_weight: 2,
            remove_weight: 5,
            create_weight: 10,
            block_size: 8192,
        };

        let uri = config.server_uri();
        assert_eq!(uri, "http://[::1]:42069");

        // Verify the original socket_addr can still be parsed as SocketAddr
        let socket_addr: std::net::SocketAddr = config.socket_addr.parse().unwrap();
        assert_eq!(socket_addr.port(), 42069);
    }

    #[test]
    fn test_server_uri_ipv6_all_interfaces() {
        let config = Config {
            sync_path: PathBuf::from("/tmp/test"),
            socket_addr: String::from("[::]:42069"),
            schedule_delay: 3600,
            log_level: String::from("debug"),
            sync_threshold: 20,
            modify_weight: 2,
            remove_weight: 5,
            create_weight: 10,
            block_size: 8192,
        };

        let uri = config.server_uri();
        assert_eq!(uri, "http://[::]:42069");

        // Verify the original socket_addr can still be parsed as SocketAddr
        let socket_addr: std::net::SocketAddr = config.socket_addr.parse().unwrap();
        assert_eq!(socket_addr.port(), 42069);
    }

    #[test]
    fn test_server_uri_ipv4_all_interfaces() {
        let config = Config {
            sync_path: PathBuf::from("/tmp/test"),
            socket_addr: String::from("0.0.0.0:42069"),
            log_level: String::from("debug"),
            schedule_delay: 3600,
            sync_threshold: 20,
            modify_weight: 2,
            remove_weight: 5,
            create_weight: 10,
            block_size: 8192,
        };

        let uri = config.server_uri();
        assert_eq!(uri, "http://0.0.0.0:42069");

        // Verify the original socket_addr can still be parsed as SocketAddr
        let socket_addr: std::net::SocketAddr = config.socket_addr.parse().unwrap();
        assert_eq!(socket_addr.port(), 42069);
    }
}
