use chrono::prelude::Utc;
use log::{ debug, info, warn };
use serde;
use serde::{ Deserialize, Serialize };
use std::io::{ Error, ErrorKind };
use std::process::exit;
use std::{
    collections::{ BTreeMap, BTreeSet },
    fs::{ self },
    path::{ Path, PathBuf },
    str::FromStr,
    time::UNIX_EPOCH,
};
use tokio::fs::{ File, OpenOptions };
use tokio::io::AsyncReadExt;
use tokio::io::{ AsyncSeekExt, AsyncWriteExt };
use tokio_stream::Stream;
use uuid::Uuid;
use walkdir::WalkDir;

use crate::harmonic::{ FileAction, FileStatus, FileSync, FileType, TransferDirection };

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    pub sync_path: PathBuf,
    pub socket_addr: String,
    pub schedule_delay: u64,

    pub sync_threshold: u64,
    pub modify_weight: u64,
    pub remove_weight: u64,
    pub create_weight: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SyncState {
    pub last_sync_timestamp_micros: i64,
    tree: BTreeMap<PathBuf, FileMetadata>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct FileMetadata {
    hash: [u8; 32],
    modified_ts: i64,
}

pub struct Diff {
    path: PathBuf,
    pub change: ChangeType,
    hash: [u8; 32],
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
        let hash = blake3::hash(&file);
        let hash: [u8; 32] = *hash.as_bytes();
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

impl Default for Config {
    fn default() -> Self {
        Self {
            sync_path: PathBuf::from("/Users/milad/harmonic"),
            socket_addr: String::from("[::1]:42069"),
            schedule_delay: 3600,
            sync_threshold: 20,
            modify_weight: 2,
            remove_weight: 5,
            create_weight: 10,
        }
    }
}

impl Config {
    pub fn server_uri(&self) -> String {
        format!("http://{}", self.socket_addr)
    }
}

fn config_dir_path() -> PathBuf {
    let mut path = dirs::config_dir().expect("No path could be created for config dir");
    path.push("harmonic");

    debug!("Config path: {:?}", path);
    path
}

fn config_file_path() -> PathBuf {
    let mut path = config_dir_path();
    path.push("config.toml");

    path
}

fn state_file_path() -> PathBuf {
    let mut path = config_dir_path();
    path.push("state.json");

    path
}

fn get_absolute_path(relative_path: &Path, sync_path: &Path) -> PathBuf {
    sync_path.join(relative_path)
}

fn get_relative_path(absolute_path: &Path, sync_path: &Path) -> Result<PathBuf, Error> {
    absolute_path
        .strip_prefix(sync_path)
        .map(|p| p.to_path_buf())
        .map_err(|_| {
            Error::new(
                ErrorKind::InvalidFilename,
                format!(
                    "{:?} is not in {:?}. Unable to generate relative path.",
                    absolute_path,
                    sync_path
                )
            )
        })
}

fn save_config(config: Config) {
    let config_toml = toml
        ::to_string(&config)
        .expect("Unable to serialize config struct to toml format.");

    debug!("Writing config file to {:?}", config_file_path());
    fs::DirBuilder::new().recursive(true).create(config_dir_path()).unwrap();

    fs::write(config_file_path(), config_toml).expect(
        "Unable to write serialized config struct to file."
    );
}

pub fn load_config() -> Config {
    info!("Loading config.");
    match fs::read_to_string(config_file_path()) {
        Ok(config_toml) => toml::from_str(&config_toml).expect("Unable to parse string to toml"),
        Err(error) =>
            match error.kind() {
                ErrorKind::NotFound => {
                    info!("Config file not found. Creating with default values.");
                    handle_no_config();
                    info!(
                        "Program will exit now. Please edit default configuration and try again."
                    );
                    exit(0);
                }
                _ => panic!("Failed reading config with uncaught error"),
            }
    }
}

fn handle_no_config() {
    let c = Config::default();
    println!("Saving config to: {:?}", config_dir_path());
    println!("Please edit config with required values");
    save_config(c);
}

pub fn save_state(state: SyncState) {
    let state_json = serde_json
        ::to_string(&state)
        .expect("Unable to serialise state to json format.");

    fs::write(state_file_path(), state_json).expect(
        "Unable to write serialized Sync State struct to file."
    );
}

pub fn load_state() -> SyncState {
    match fs::read_to_string(state_file_path()) {
        Ok(state_json) => {
            serde_json::from_str(&state_json).expect("Unable to parse string to json")
        }
        Err(e) =>
            match e.kind() {
                ErrorKind::NotFound => {
                    info!("Initialising empty state");
                    return SyncState {
                        last_sync_timestamp_micros: 0,
                        tree: BTreeMap::new(),
                    };
                }
                _ => panic!("Failed reading config with uncaught error"),
            }
    }
}

pub fn generate_state(root_path: &PathBuf) -> SyncState {
    let mut file_tree: BTreeMap<PathBuf, FileMetadata> = BTreeMap::new();

    // TODO: log
    for file in WalkDir::new(root_path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.metadata().unwrap().is_file()) {
        let absolute_path = file.path();
        let metadata = FileMetadata::new(absolute_path);

        let relative_path = get_relative_path(absolute_path, root_path).expect(
            "File path must be within sync dir"
        );
        file_tree.insert(relative_path, metadata);
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

pub async fn get_file(data: &FileSync, sync_path: &Path) -> File {
    let relative_path = PathBuf::from(&data.path);
    let absolute_path = get_absolute_path(&relative_path, sync_path);

    if let Some(parent_path) = absolute_path.parent() {
        tokio::fs
            ::create_dir_all(parent_path).await
            .expect("Unable to create parent path for file}");
    }
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .open(absolute_path).await
        .expect("Could not create new file.");

    file.set_len(data.file_size).await.expect(
        "Failed to set file length. This error is not recoverable."
    );

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

pub fn file_to_chunked_file_sync(relative_path: &PathBuf, sync_path: &Path) -> impl Stream<Item = FileSync> {
    let absolute_path = get_absolute_path(relative_path, sync_path);
    let relative_path_c = relative_path.clone();

    debug!("Writing to file: {:?}", absolute_path);
    async_stream::stream! {
        let mut file = tokio::fs::File::open(&absolute_path).await.unwrap();
        let mut buffer = vec![0u8; 8192];
        let mut offset = 0;
        let file_size = file.metadata().await.unwrap().len();

        while let Ok(n) = file.read(&mut buffer).await {
            if n == 0 {
                break;
            }

            yield FileSync {
                path: relative_path_c.to_str().expect("Could not convert PathBuf to string slice").to_string(),
                chunk: buffer[..n].to_vec(),
                offset: offset,
                is_final: false,
                file_size: file_size,
            };
            offset += n as u64;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use tempfile::tempdir;

    #[test]
    fn test_file_status_vec_to_tree() {
        let file_statuses = vec![
            FileStatus {
                path: String::from("test1.txt"),
                timestamp_micro: 123456,
                file_type: FileType::Other.into(),
                hash: vec![
                    1,
                    2,
                    3,
                    4,
                    5,
                    6,
                    7,
                    8,
                    9,
                    10,
                    11,
                    12,
                    13,
                    14,
                    15,
                    16,
                    17,
                    18,
                    19,
                    20,
                    21,
                    22,
                    23,
                    24,
                    25,
                    26,
                    27,
                    28,
                    29,
                    30,
                    31,
                    32
                ],
            },
            FileStatus {
                path: String::from("test2.txt"),
                timestamp_micro: 654321,
                file_type: FileType::Other.into(),
                hash: vec![
                    33,
                    34,
                    35,
                    36,
                    37,
                    38,
                    39,
                    40,
                    41,
                    42,
                    43,
                    44,
                    45,
                    46,
                    47,
                    48,
                    49,
                    50,
                    51,
                    52,
                    53,
                    54,
                    55,
                    56,
                    57,
                    58,
                    59,
                    60,
                    61,
                    62,
                    63,
                    64
                ],
            }
        ];

        let tree = file_status_vec_to_tree(file_statuses);

        assert_eq!(tree.len(), 2);
        assert_eq!(
            tree.get(&PathBuf::from("test1.txt")),
            Some(
                &vec![
                    1,
                    2,
                    3,
                    4,
                    5,
                    6,
                    7,
                    8,
                    9,
                    10,
                    11,
                    12,
                    13,
                    14,
                    15,
                    16,
                    17,
                    18,
                    19,
                    20,
                    21,
                    22,
                    23,
                    24,
                    25,
                    26,
                    27,
                    28,
                    29,
                    30,
                    31,
                    32
                ]
            )
        );
        assert_eq!(
            tree.get(&PathBuf::from("test2.txt")),
            Some(
                &vec![
                    33,
                    34,
                    35,
                    36,
                    37,
                    38,
                    39,
                    40,
                    41,
                    42,
                    43,
                    44,
                    45,
                    46,
                    47,
                    48,
                    49,
                    50,
                    51,
                    52,
                    53,
                    54,
                    55,
                    56,
                    57,
                    58,
                    59,
                    60,
                    61,
                    62,
                    63,
                    64
                ]
            )
        );
    }

    #[test]
    fn test_string_to_uuid_valid() {
        let uuid_str = String::from("550e8400-e29b-41d4-a716-446655440000");
        let uuid = string_to_uuid(&uuid_str);
        assert_eq!(uuid.to_string(), uuid_str);
    }

    #[test]
    #[should_panic(expected = "Failed to convert uuid string")]
    fn test_string_to_uuid_invalid() {
        let invalid_uuid = String::from("bad-uuid-string");
        string_to_uuid(&invalid_uuid);
    }

    #[test]
    fn test_compare_states_added_file() {
        let before_state = SyncState {
            last_sync_timestamp_micros: 1000,
            tree: BTreeMap::new(),
        };

        let mut now_tree = BTreeMap::new();
        now_tree.insert(PathBuf::from("new_file.txt"), FileMetadata {
            hash: [1; 32],
            modified_ts: 2000,
        });

        let now_state = SyncState {
            last_sync_timestamp_micros: 2000,
            tree: now_tree,
        };

        let diffs = compare_states(&before_state, &now_state);

        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].path, PathBuf::from("new_file.txt"));
        assert!(matches!(diffs[0].change, ChangeType::Added));
    }

    #[test]
    fn test_compare_states_removed_file() {
        let mut before_tree = BTreeMap::new();
        before_tree.insert(PathBuf::from("old_file.txt"), FileMetadata {
            hash: [1; 32],
            modified_ts: 1000,
        });

        let before_state = SyncState {
            last_sync_timestamp_micros: 1000,
            tree: before_tree,
        };

        let now_state = SyncState {
            last_sync_timestamp_micros: 2000,
            tree: BTreeMap::new(),
        };

        let diffs = compare_states(&before_state, &now_state);

        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].path, PathBuf::from("old_file.txt"));
        assert!(matches!(diffs[0].change, ChangeType::Removed));
    }

    #[test]
    fn test_compare_states_modified_file() {
        let mut before_tree = BTreeMap::new();
        before_tree.insert(PathBuf::from("modified.txt"), FileMetadata {
            hash: [1; 32],
            modified_ts: 1000,
        });

        let before_state = SyncState {
            last_sync_timestamp_micros: 1000,
            tree: before_tree,
        };

        let mut now_tree = BTreeMap::new();
        now_tree.insert(PathBuf::from("modified.txt"), FileMetadata {
            hash: [2; 32],
            modified_ts: 2000,
        });

        let now_state = SyncState {
            last_sync_timestamp_micros: 2000,
            tree: now_tree,
        };

        let diffs = compare_states(&before_state, &now_state);

        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].path, PathBuf::from("modified.txt"));
        assert!(matches!(diffs[0].change, ChangeType::Modified));
        assert_eq!(diffs[0].modified_ts, 2000);
    }

    #[test]
    fn test_compare_states_no_changes() {
        let mut tree = BTreeMap::new();
        tree.insert(PathBuf::from("unchanged.txt"), FileMetadata {
            hash: [1; 32],
            modified_ts: 1000,
        });

        let before_state = SyncState {
            last_sync_timestamp_micros: 1000,
            tree: tree.clone(),
        };

        let now_state = SyncState {
            last_sync_timestamp_micros: 2000,
            tree,
        };

        let diffs = compare_states(&before_state, &now_state);

        assert_eq!(diffs.len(), 0);
    }

    #[test]
    fn test_generate_sync_plan_local_newer() {
        let mut local_tree = BTreeMap::new();
        local_tree.insert(PathBuf::from("file.txt"), FileMetadata {
            hash: [1; 32],
            modified_ts: 2000,
        });

        let state_now = SyncState {
            last_sync_timestamp_micros: 2000,
            tree: local_tree,
        };

        let remote_files = vec![FileStatus {
            path: String::from("file.txt"),
            timestamp_micro: 1000,
            file_type: FileType::Other.into(),
            hash: vec![2; 32],
        }];

        let plan = generate_sync_plan(&state_now, &remote_files);

        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].path, "file.txt");
        assert_eq!(plan[0].direction, TransferDirection::ServerSend as i32);
    }

    #[test]
    fn test_generate_sync_plan_remote_newer() {
        let mut local_tree = BTreeMap::new();
        local_tree.insert(PathBuf::from("file.txt"), FileMetadata {
            hash: [1; 32],
            modified_ts: 1000,
        });

        let state_now = SyncState {
            last_sync_timestamp_micros: 1000,
            tree: local_tree,
        };

        let remote_files = vec![FileStatus {
            path: String::from("file.txt"),
            timestamp_micro: 2000,
            file_type: FileType::Other.into(),
            hash: vec![2; 32],
        }];

        let plan = generate_sync_plan(&state_now, &remote_files);

        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].path, "file.txt");
        assert_eq!(plan[0].direction, TransferDirection::ClientSend as i32);
    }

    #[test]
    fn test_generate_sync_plan_files_identical() {
        let mut local_tree = BTreeMap::new();
        local_tree.insert(PathBuf::from("file.txt"), FileMetadata {
            hash: [1; 32],
            modified_ts: 1000,
        });

        let state_now = SyncState {
            last_sync_timestamp_micros: 1000,
            tree: local_tree,
        };

        let remote_files = vec![FileStatus {
            path: String::from("file.txt"),
            timestamp_micro: 1000,
            file_type: FileType::Other.into(),
            hash: vec![1; 32],
        }];

        let plan = generate_sync_plan(&state_now, &remote_files);

        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].direction, TransferDirection::Skip as i32);
    }

    #[test]
    fn test_config_dir_path() {
        let path = config_dir_path();
        assert!(path.ends_with("harmonic"));
    }

    #[test]
    fn test_config_file_path() {
        let path = config_file_path();
        assert!(path.ends_with("harmonic/config.toml"));
    }

    #[test]
    fn test_state_file_path() {
        let path = state_file_path();
        assert!(path.ends_with("harmonic/state.json"));
    }

    #[test]
    fn test_diff_to_file_status_conversion() {
        let diff = Diff {
            path: PathBuf::from("test_file.txt"),
            change: ChangeType::Modified,
            hash: [
                1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
                25, 26, 27, 28, 29, 30, 31, 32,
            ],
            modified_ts: 123456789,
        };

        let file_status: FileStatus = diff.into();

        assert_eq!(file_status.path, "test_file.txt");
        assert_eq!(file_status.timestamp_micro, 123456789);
        assert_eq!(
            file_status.hash,
            vec![
                1,
                2,
                3,
                4,
                5,
                6,
                7,
                8,
                9,
                10,
                11,
                12,
                13,
                14,
                15,
                16,
                17,
                18,
                19,
                20,
                21,
                22,
                23,
                24,
                25,
                26,
                27,
                28,
                29,
                30,
                31,
                32
            ]
        );
    }

    #[test]
    fn test_filemedata_constructor() {
        let dir = tempdir().unwrap();
        let mut path = PathBuf::from(&dir.path());
        path.push("file.txt");
        let data = "randomdata123";

        std::fs::write(&path, &data).unwrap();

        let metadata = FileMetadata::new(&path);
        let _h = blake3::hash(data.as_bytes());
        let hash: [u8; 32] = *_h.as_bytes();

        assert_eq!(metadata.hash, hash);
    }

    #[test]
    fn test_server_uri_ipv4() {
        let config = Config {
            sync_path: PathBuf::from("/tmp/test"),
            socket_addr: String::from("192.168.1.100:42069"),
            schedule_delay: 3600,
            sync_threshold: 20,
            modify_weight: 2,
            remove_weight: 5,
            create_weight: 10,
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
            sync_threshold: 20,
            modify_weight: 2,
            remove_weight: 5,
            create_weight: 10,
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
            sync_threshold: 20,
            modify_weight: 2,
            remove_weight: 5,
            create_weight: 10,
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
            schedule_delay: 3600,
            sync_threshold: 20,
            modify_weight: 2,
            remove_weight: 5,
            create_weight: 10,
        };

        let uri = config.server_uri();
        assert_eq!(uri, "http://0.0.0.0:42069");

        // Verify the original socket_addr can still be parsed as SocketAddr
        let socket_addr: std::net::SocketAddr = config.socket_addr.parse().unwrap();
        assert_eq!(socket_addr.port(), 42069);
    }
}
