use serde::{Deserialize, Serialize};
use std::io::ErrorKind;
use std::net::SocketAddr;
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
            socket_addr: String::from("[::1]:42069"), // server overwrites this with localhost, uses same port
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
        format!("https://{}", self.socket_addr)
    }

    pub fn socket_addr(&self) -> Result<SocketAddr> {
        self.socket_addr
            .parse()
            .map_err(|_| HarmonicError::ConfigError)
    }
}

pub fn config_dir_path() -> Result<PathBuf> {
    let mut path = PathBuf::from(".");
    path.push(".harmonic");

    debug!(?path, "Config path");
    Ok(path)
}

pub fn ensure_config_dir() -> Result<PathBuf> {
    let path = config_dir_path()?;

    fs::DirBuilder::new()
        .recursive(true)
        .create(&path)?;

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
    ensure_config_dir()?;

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
    println!("No config found! Creating new file...");
    let c = config_from_input(&mut io::stdin().lock())?;

    println!("Saving config to: {:?}", config_dir_path());
    println!("Please review the config file to find additional configurable properties");
    save_config(c.clone())?;

    Ok(c)
}

fn config_from_input<R: io::BufRead>(input: &mut R) -> Result<Config> {
    let path = read_input_line(input, "Please enter the path you would like to Harmonize:")?;

    // how about this just depends on which binary will be compiled
    let address = read_input_line(input, "Please enter the address your server should listen on / your client should connect to: Valid formats include: IP:PORT and [::1]:PORT. The generated certificate will be valid for this address and the server's local IP")?;

    let mut c = Config::default();
    c.sync_path = PathBuf::from(path);
    c.socket_addr = address;

    Ok(c)
}

fn read_input_line<R: io::BufRead>(input: &mut R, prompt: &str) -> Result<String> {
    println!("{prompt}");
    let mut buf = String::new();
    let read = input.read_line(&mut buf).map_err(HarmonicError::Io)?;

    if read == 0 {
        // stdin closed before an answer arrived, e.g. a headless launcher
        return Err(HarmonicError::Input(String::from(
            "input closed before configuration was completed, create a config file manually",
        )));
    }

    Ok(buf.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_config_dir_path() {
        let path = config_dir_path().unwrap();
        assert!(path.ends_with(".harmonic"));
    }

    #[test]
    fn test_config_from_input_reads_both_answers() {
        let mut input = Cursor::new("/storage/emulated/0/Books\n192.168.1.10:42069\n");

        let config = config_from_input(&mut input).unwrap();

        assert_eq!(config.sync_path, PathBuf::from("/storage/emulated/0/Books"));
        assert_eq!(config.socket_addr, "192.168.1.10:42069");
    }

    #[test]
    fn test_config_from_input_fails_on_closed_input() {
        let mut input = Cursor::new("");

        let result = config_from_input(&mut input);

        assert!(result.is_err(), "closed input must fail instead of an empty config");
    }

    #[test]
    fn test_config_from_input_fails_on_closed_input_after_first_answer() {
        let mut input = Cursor::new("/storage/emulated/0/Books\n");

        assert!(config_from_input(&mut input).is_err());
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
        assert_eq!(uri, "https://192.168.1.100:42069");

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
        assert_eq!(uri, "https://[::1]:42069");

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
        assert_eq!(uri, "https://[::]:42069");

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
        assert_eq!(uri, "https://0.0.0.0:42069");

        // Verify the original socket_addr can still be parsed as SocketAddr
        let socket_addr: std::net::SocketAddr = config.socket_addr.parse().unwrap();
        assert_eq!(socket_addr.port(), 42069);
    }
}
