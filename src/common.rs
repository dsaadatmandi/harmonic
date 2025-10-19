use chrono::prelude::Utc;
use log::info;
use serde;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};
use walkdir::WalkDir;

use crate::harmonic::{FileStatus, FileType, UpdateStrategy};

#[derive(Serialize, Deserialize)]
pub struct Config {
    uuid: uuid::Uuid,
    pub sync_path: PathBuf,
}

#[derive(Serialize, Deserialize)]
pub struct SyncState {
    pub last_sync_timestamp_micros: i64,
    tree: BTreeMap<PathBuf, FileMetadata>,
}

#[derive(Serialize, Deserialize)]
struct FileMetadata {
    hash: [u8; 16],
    modified_ts: i64,
}

pub struct Diff {
    path: PathBuf,
    pub change: ChangeType,
    hash: [u8; 16],
    modified_ts: i64,
}

pub enum ChangeType {
    Added,
    Removed,
    Modified,
}

impl From<Diff> for FileStatus {
    fn from(diff: Diff) -> Self {
        FileStatus {
            path: diff
                .path
                .to_str()
                .expect("Issue converting strange chars.")
                .to_string(),
            timestamp_micro: diff.modified_ts,
            file_type: FileType::Other.into(),
            hash: diff.hash.to_vec(),
        }
    }
}

// impl From<ChangeType> for UpdateStrategy {
//     fn from(change: ChangeType) -> Self {
//         match change {
//             ChangeType::Added => UpdateStrate::
//             ChangeType::Removed => UpdateStrategy::ClientPull
//             ChangeType::Modified => U
//         }
//     }
// }

impl FileMetadata {
    fn new<P: AsRef<Path>>(path: P) -> FileMetadata {
        let path = path.as_ref();
        let file = fs::read(&path).expect("Failed to open file.");
        let hash: [u8; 16] = md5::compute(&file).into();
        let modified_systime = fs::metadata(&path)
            .expect("Unable to read metadata for file")
            .modified()
            .expect("Unable to read modified time for file");

        let modified_ts = modified_systime
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_micros() as i64;

        Self { hash, modified_ts }
    }
}

fn config_dir_path() -> PathBuf {
    let mut path = dirs::config_dir().expect("No path could be created for config dir");
    path.push("harmonic");

    path
}

fn config_file_path() -> PathBuf {
    let mut path = config_dir_path();
    path.push("config.toml");

    path
}

fn save_config(config: Config) {
    let config_toml =
        toml::to_string(&config).expect("Unable to serialize config struct to toml format.");

    fs::write(config_file_path(), config_toml)
        .expect("Unable to write serialized config struct to file.");
}

pub fn load_config() -> Config {
    let config_toml = fs::read_to_string(config_file_path()).expect("Unable to read file");

    toml::from_str(&config_toml).expect("Unable to parse string to toml")
}

pub fn save_state(state: SyncState, config: &Config) {
    let state_json =
        serde_json::to_string(&state).expect("Unable to serialise state to json format.");

    fs::write(&config.sync_path, state_json)
        .expect("Unable to write serialized Sync State struct to file.");
}

pub fn load_state(config: &Config) -> SyncState {
    let state_json = fs::read_to_string(&config.sync_path).expect("Unable to read file");

    serde_json::from_str(&state_json).expect("Unable to parse string to toml")
}

pub fn generate_state(root_path: PathBuf) -> SyncState {
    let mut file_tree: BTreeMap<PathBuf, FileMetadata> = BTreeMap::new();

    // TODO: log
    for file in WalkDir::new(root_path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.metadata().unwrap().is_file())
    {
        let metadata = FileMetadata::new(file.path());
        file_tree.insert(file.into_path(), metadata);
    }

    SyncState {
        last_sync_timestamp_micros: Utc::now().timestamp_micros(),
        tree: file_tree,
    }
}

pub fn compare_states(before_state: &SyncState, now_state: &SyncState) -> Vec<Diff> {
    info!("Computing difference between current state with previous sync state");

    let mut diffs = Vec::new();

    let all_paths: BTreeSet<&PathBuf> = before_state
        .tree
        .keys()
        .chain(now_state.tree.keys())
        .collect();

    for path in all_paths {
        let before = before_state.tree.get(path);
        let now = now_state.tree.get(path);

        match (now, before) {
            (Some(now_meta), Some(before_meta)) if now_meta.hash != before_meta.hash => {
                let (hs, mts) = if now_meta.modified_ts > before_meta.modified_ts {
                    (now_meta.hash, now_meta.modified_ts)
                } else {
                    (before_meta.hash, before_meta.modified_ts)
                };
                diffs.push(Diff {
                    path: path.to_owned(),
                    change: ChangeType::Modified,
                    hash: hs,
                    modified_ts: mts,
                })
            }
            (Some(meta), None) => diffs.push(Diff {
                path: path.to_owned(),
                change: ChangeType::Added,
                hash: meta.hash,
                modified_ts: meta.modified_ts,
            }),
            (None, Some(meta)) => diffs.push(Diff {
                path: path.to_owned(),
                change: ChangeType::Removed,
                hash: meta.hash,
                modified_ts: meta.modified_ts,
            }),
            (Some(_), Some(_)) => {}
            (None, None) => unreachable!(),
        }
    }

    diffs
}
