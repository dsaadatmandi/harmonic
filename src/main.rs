use std::{fs::{self, File}, io::BufWriter, path::{Path, PathBuf}};
use serde::{Serialize, Deserialize};
use serde;


fn main() {
    println!("Hello, world!");
}

#[derive(Serialize, Deserialize)]
struct Config {
    uuid: uuid::Uuid,

}

fn config_dir_path() -> PathBuf {
    let mut path = dirs::config_dir()
    .expect("No path could be created for config dir");
    path.push("harmonic");

    path
}


fn config_file_path() -> PathBuf {
    let mut path = config_dir_path();
    path.push("config.toml");

    path
}

fn save_config(config: Config) {
    let serial_toml = toml::to_string(&config)
    .expect("Unable to serialize config struct to toml format.");

    fs::write(config_file_path(), serial_toml)
    .expect("Unable to write serialized config struct to file.");
}