use chrono::prelude::Utc;
use std::{fs::{self}, path:: PathBuf, time::UNIX_EPOCH};
use serde::{Serialize, Deserialize};
use serde;
use walkdir::WalkDir;


// pub struct HealthStatus {
//     time: i64,
//     state: HealthStatus,
// }

// impl HealthStatus {
//     pub fn new(state: Status) -> Self {
//         HealthStatus {
//             time: Utc::now().timestamp_micros(),
//             state,
//         }
//     }
// }


#[derive(Serialize, Deserialize)]
pub struct Config {
    uuid: uuid::Uuid,
    pub sync_path: PathBuf,

}

#[derive(Serialize, Deserialize)]
pub struct SyncState {
    last_sync_timestamp_micros: i64,
    tree: Vec<PathHash>,
}

#[derive(Serialize, Deserialize)]
struct PathHash {
    path: PathBuf,
    hash: [u8; 16],
    modified_ts: i64,
}

impl PathHash {
    fn new(path: PathBuf) -> PathHash {
        let file = fs::read(&path)
        .expect("Failed to open file.");
        let hash: [u8; 16] = md5::compute(&file).into();
        let modified_systime = fs::metadata(&path)
        .expect("Unable to read metadata for file")
        .modified()
        .expect("Unable to read modified time for file");

        let modified_ts = modified_systime
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_micros() as i64;

        Self {path, hash, modified_ts}
    }
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

pub fn load_config() -> Config {
    let content = fs::read_to_string(config_file_path())
    .expect("Unable to read file");

    toml::from_str(&content)
    .expect("Unable to parse string to toml")

}

pub fn generate_sync_state_for_all_files(root_path: PathBuf) -> SyncState {

    let mut ph_vec: Vec<PathHash> = Vec::new();

    // TODO: log
    for file in WalkDir::new(root_path)
    .into_iter()
    .filter_map(|e| e.ok())
    .filter(|e| e.metadata().unwrap().is_file()) {
        let ph = PathHash::new(file.into_path());
        ph_vec.push(ph);
    }

    SyncState { last_sync_timestamp_micros: Utc::now().timestamp_micros(), tree: ph_vec }

}