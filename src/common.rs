use chrono::prelude::Utc;
use log::{ info, warn };
use serde;
use serde::{ Deserialize, Serialize };
use tokio::io::{ AsyncSeekExt, AsyncWriteExt };
use uuid::Uuid;
use std::{
    collections::{ BTreeMap, BTreeSet },
    fs::{ self },
    path::{ Path, PathBuf },
    time::UNIX_EPOCH,
    str::FromStr,
};
use tokio::fs::{ File, OpenOptions };
use walkdir::WalkDir;
use tokio_stream::{ Stream, StreamExt };
use tokio::io::{ AsyncReadExt };

use crate::harmonic::{ FileStatus, FileSync, FileType, TransferDirection, FileAction };

#[derive(Serialize, Deserialize)]
pub struct Config {
    uuid: uuid::Uuid,
    pub sync_path: PathBuf,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct SyncState {
    pub last_sync_timestamp_micros: i64,
    tree: BTreeMap<PathBuf, FileMetadata>,
}

#[derive(Serialize, Deserialize, Clone)]
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
            path: diff.path.to_str().expect("Issue converting strange chars.").to_string(),
            timestamp_micro: diff.modified_ts,
            file_type: FileType::Other.into(),
            hash: diff.hash.to_vec(),
        }
    }
}

impl FileMetadata {
    fn new<P: AsRef<Path>>(path: P) -> FileMetadata {
        let path = path.as_ref();
        let file = fs::read(&path).expect("Failed to open file.");
        let hash: [u8; 16] = md5::compute(&file).into();
        let modified_systime = fs
            ::metadata(&path)
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
    let config_toml = toml
        ::to_string(&config)
        .expect("Unable to serialize config struct to toml format.");

    fs::write(config_file_path(), config_toml).expect(
        "Unable to write serialized config struct to file."
    );
}

pub fn load_config() -> Config {
    let config_toml = fs::read_to_string(config_file_path()).expect("Unable to read file");

    toml::from_str(&config_toml).expect("Unable to parse string to toml")
}

pub fn save_state(state: SyncState, config: &Config) {
    let state_json = serde_json
        ::to_string(&state)
        .expect("Unable to serialise state to json format.");

    fs::write(&config.sync_path, state_json).expect(
        "Unable to write serialized Sync State struct to file."
    );
}

pub fn load_state(config: &Config) -> SyncState {
    let state_json = fs::read_to_string(&config.sync_path).expect("Unable to read file");

    serde_json::from_str(&state_json).expect("Unable to parse string to toml")
}

pub fn generate_state(root_path: &PathBuf) -> SyncState {
    let mut file_tree: BTreeMap<PathBuf, FileMetadata> = BTreeMap::new();

    // TODO: log
    for file in WalkDir::new(root_path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.metadata().unwrap().is_file()) {
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

    let all_paths: BTreeSet<&PathBuf> = before_state.tree
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
                });
            }
            (Some(meta), None) =>
                diffs.push(Diff {
                    path: path.to_owned(),
                    change: ChangeType::Added,
                    hash: meta.hash,
                    modified_ts: meta.modified_ts,
                }),
            (None, Some(meta)) =>
                diffs.push(Diff {
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

pub async fn write_data_to_offset(data: FileSync, file: &mut File) {
    file.seek(std::io::SeekFrom::Start(data.offset)).await.expect("Seek failed");

    file.write_all(&data.chunk).await.expect("Chunk write failed");
}

pub async fn get_file(data: &FileSync) -> File {
    let pb = PathBuf::from(&data.path);

    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .open(pb).await
        .expect("Could not create new file.");

    file.set_len(data.file_size).await;

    file
}

pub fn file_status_vec_to_tree(file_status_vec: Vec<FileStatus>) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut tree = BTreeMap::new();

    for f in file_status_vec {
        tree.insert(PathBuf::from(f.path), f.hash);
    }

    tree
}

pub fn generate_sync_plan(
    state_now: &SyncState,
    remote_files: &Vec<FileStatus>
) -> Vec<FileAction> {
    let mut sync_plan: Vec<FileAction> = Vec::new();

    let remote_tree: BTreeMap<PathBuf, &FileStatus> = remote_files
        .iter()
        .map(|r| (PathBuf::from(&r.path), r))
        .collect();

    let all_paths: BTreeSet<&PathBuf> = state_now.tree.keys().chain(remote_tree.keys()).collect();

    for path in all_paths {
        let local = state_now.tree.get(path);
        let remote = remote_tree.get(path);

        let direction: TransferDirection = match (local, remote) {
            (Some(local_file), Some(remote_file)) if remote_file.hash != local_file.hash => {
                if local_file.modified_ts > remote_file.timestamp_micro {
                    TransferDirection::ServerSend
                } else if remote_file.timestamp_micro > local_file.modified_ts {
                    TransferDirection::ClientSend
                } else {
                    warn!(
                        "File hash for {:?} is different but modified timestamp is identical! Needs investigation",
                        path
                    );
                    TransferDirection::Skip
                }
            }
            (Some(_), None) => {
                // TODO implement deleted file logic
                TransferDirection::ServerSend
            }
            (None, Some(_)) => {
                // TODO implement deleted file logic
                TransferDirection::ClientSend
            }
            (None, None) => unreachable!(),
            _ => TransferDirection::Skip,
        };

        sync_plan.push(FileAction {
            path: path.to_str().expect("path_to_str").to_string(),
            direction: direction.into(),
        });
    }

    sync_plan
}

pub fn string_to_uuid(uuid_str: &String) -> Uuid {
    match Uuid::from_str(uuid_str) {
        Ok(uuid) => uuid,
        Err(e) =>
            panic!("Failed to convert uuid string {} into Uuid struct due to: {:?}", uuid_str, e),
    }
}

pub fn file_to_chunked_file_sync(path: &PathBuf) -> impl Stream<Item = FileSync> {
    async_stream::stream! {
        let mut file = tokio::fs::File::open(&path).await.unwrap();
        let mut buffer = vec![0u8; 8192];
        let mut offset = 0;
        let file_size = file.metadata().await.unwrap().len();

        while let Ok(n) = file.read(&mut buffer).await {
            if n == 0 {
                break;
            }

            yield FileSync {
                sync_uuid: "TBD".to_string(),
                path: path.to_str().expect("Could not convert PathBuf to string slice").to_string(),
                chunk: buffer[..n].to_vec(),
                offset: offset,
                is_final: false,
                file_size: file_size,
            };
            offset += n as u64;
        }
    }
}
