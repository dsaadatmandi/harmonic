use chrono::prelude::Utc;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use std::{fs};
use tracing::{debug, info, instrument, warn};
use walkdir::WalkDir;

use crate::Config;
use crate::proto::{
    BlockSignature, BlockSignatures, FileAction, FileStatus, FileType, TransferDirection,
};
use crate::sync::transfer::get_absolute_path;
use crate::utils::{BuzHash, HarmonicError, Result};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SyncState {
    pub last_sync_timestamp_micros: i64,
    pub tree: BTreeMap<PathBuf, FileMetadata>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FileMetadata {
    pub hash: [u8; 32],
    #[serde(with = "timestamp_proto")]
    pub modified_ts: prost_types::Timestamp,
}

mod timestamp_proto {
    use prost_types::Timestamp;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(ts: &Timestamp, s: S) -> Result<S::Ok, S::Error>
    where S: Serializer {
        let micros = ts.seconds * 1_000_000 + (ts.nanos / 1_000) as i64;
        s.serialize_i64(micros)
    }

    pub fn deserialize<'de, D>(d: D) -> Result<Timestamp, D::Error>
    where D: Deserializer<'de> {
        let micros = i64::deserialize(d)?;
        Ok(Timestamp {
            seconds: micros / 1_000_000,
            nanos: ((micros % 1_000_000) * 1_000) as i32,
        })
    }
}

#[derive(Debug)]
pub struct Diff {
    path: PathBuf,
    pub change: ChangeType,
    hash: [u8; 32],
    modified_ts: prost_types::Timestamp,
}

#[derive(Debug)]

pub enum ChangeType {
    Added,
    Removed,
    Modified,
}

pub struct BlockCache {
    pub blocks: Vec<Box<[u8]>>,
}

impl TryFrom<Diff> for FileStatus {
    type Error = HarmonicError;

    fn try_from(diff: Diff) -> Result<Self> {
        Ok(FileStatus {
            path: diff
                .path
                .to_str()
                .ok_or(HarmonicError::StringInvalid)?
                .to_string(),
            timestamp: Some(diff.modified_ts),
            file_type: FileType::Other.into(),
            hash: diff.hash.to_vec(),
        })
    }
}

impl FileMetadata {
    fn new<P: AsRef<Path>>(path: P) -> Result<FileMetadata> {
        let path = path.as_ref();
        let file = fs::read(&path)?;
        let hash = blake3::hash(&file);
        let hash: [u8; 32] = *hash.as_bytes();
        let modified_systime = fs::metadata(&path)?.modified()?;

        let d = modified_systime.duration_since(UNIX_EPOCH)?;
        let timestamp = prost_types::Timestamp {
            seconds: d.as_secs() as i64,
            nanos: d.subsec_nanos() as i32,
        };

        Ok(Self { hash, modified_ts: timestamp })
    }
}

fn proto_timestamp_gt(a: prost_types::Timestamp, b: prost_types::Timestamp) -> bool {
    return (a.seconds, a.nanos) > (b.seconds, b.nanos)
}

fn state_file_path() -> Result<PathBuf> {
    let mut path = PathBuf::from(".");
    path.push(".harmonic");
    path.push("state.json");

    Ok(path)
}

fn get_relative_path(absolute_path: &Path, sync_path: &Path) -> Result<PathBuf> {
    absolute_path
        .strip_prefix(sync_path)
        .map(|p| p.to_path_buf())
        .map_err(|_| HarmonicError::PathIntegrityError {
            path: absolute_path.to_path_buf(),
            sync_path: sync_path.to_path_buf(),
        })
}

pub fn save_state(state: SyncState) -> Result<()> {
    let state_json = serde_json::to_string(&state)?;

    fs::write(state_file_path()?, state_json)?;

    Ok(())
}

pub fn load_state() -> Result<SyncState> {
    match fs::read_to_string(state_file_path()?) {
        Ok(state_json) => Ok(serde_json::from_str(&state_json)?),
        Err(e) => match e.kind() {
            ErrorKind::NotFound => {
                info!("Initialising empty state");
                return Ok(SyncState {
                    last_sync_timestamp_micros: 0,
                    tree: BTreeMap::new(),
                });
            }
            _ => Err(HarmonicError::ConfigError),
        },
    }
}

pub fn generate_state(root_path: &PathBuf, ignore_hidden: bool) -> Result<SyncState> {
    debug!("Generating current sync state");
    let mut file_tree: BTreeMap<PathBuf, FileMetadata> = BTreeMap::new();

    for file in WalkDir::new(root_path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.metadata().map(|m| m.is_file()).unwrap_or(false))
        .filter(|p| if ignore_hidden {!has_hidden_components(p.path())} else {true})
    {
        let absolute_path = file.path();
        debug!(?absolute_path, "Getting metadata for path");
        let metadata = FileMetadata::new(absolute_path);

        let relative_path = get_relative_path(absolute_path, root_path)?;
        debug!(
            ?relative_path,
            ?absolute_path,
            "Inserted metadata for absolute path with relative path key"
        );
        file_tree.insert(relative_path, metadata?);
    }

    Ok(SyncState {
        last_sync_timestamp_micros: Utc::now().timestamp_micros(),
        tree: file_tree,
    })
}

fn has_hidden_components(path: &Path) -> bool {
    path.components()
        .any(|c| c.as_os_str().to_str().is_some_and(|s| s.starts_with(".")))
}

#[instrument(skip(before_state, now_state), fields(before_count = before_state.tree.len(), now_count = now_state.tree.len()))]
pub fn compare_states(before_state: &SyncState, now_state: &SyncState) -> Vec<Diff> {
    info!("Begin computing difference between current state with previous sync state");

    let mut diffs = Vec::new();

    let all_paths: BTreeSet<&PathBuf> = before_state
        .tree
        .keys()
        .chain(now_state.tree.keys())
        .filter(|p| !has_hidden_components(p))
        .collect();

    for path in all_paths {
        let before = before_state.tree.get(path);
        let now = now_state.tree.get(path);

        match (now, before) {
            (Some(now_meta), Some(before_meta)) if now_meta.hash != before_meta.hash => {
                let (hs, mts) = if proto_timestamp_gt(now_meta.modified_ts, before_meta.modified_ts) {
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
            (Some(_), Some(_)) => debug!("Identical hash, no action taken"),
            (None, None) => unreachable!(),
        }
    }
    info!("Completed comparing states");

    diffs
}

pub fn file_status_vec_to_tree(file_status_vec: Vec<FileStatus>) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut tree = BTreeMap::new();

    for f in file_status_vec {
        tree.insert(PathBuf::from(f.path), f.hash);
    }

    tree
}

#[instrument(skip(state_now, remote_files), fields(local_count = state_now.tree.len(), remote_count = remote_files.len()))]
pub fn generate_sync_plan(
    state_now: &SyncState,
    remote_files: &Vec<FileStatus>,
) -> Result<Vec<FileAction>> {
    debug!("Start generating sync plan");
    let mut sync_plan: Vec<FileAction> = Vec::new();

    let remote_tree: BTreeMap<PathBuf, &FileStatus> = remote_files
        .iter()
        .map(|r| (PathBuf::from(&r.path), r))
        .collect();

    let all_paths: BTreeSet<&PathBuf> = state_now.tree.keys().chain(remote_tree.keys()).collect();

    debug!(
        local_items = state_now.tree.len(),
        remote_items = remote_tree.len(),
        all_items = all_paths.len(),
        "Gathered local and remote states to compare"
    );

    for path in all_paths {
        let local = state_now.tree.get(path);
        let remote = remote_tree.get(path);
        let mut latest_timestamp: prost_types::Timestamp = Default::default();

        let direction: TransferDirection = match (local, remote) {
            (Some(local_file), Some(remote_file)) if remote_file.hash != local_file.hash => {
                let remote_file_timestamp = remote_file.timestamp.unwrap_or_default();
                if proto_timestamp_gt(local_file.modified_ts, remote_file_timestamp) {
                    latest_timestamp = local_file.modified_ts;
                    TransferDirection::Upload
                } else if proto_timestamp_gt(remote_file_timestamp, local_file.modified_ts) {
                    latest_timestamp = remote_file_timestamp;
                    TransferDirection::Download
                } else {
                    warn!(
                        ?path,
                        "File hash is different but modified timestamp is identical! Needs investigation"
                    );
                    latest_timestamp = local_file.modified_ts;
                    TransferDirection::Skip
                }
            }
            (Some(local_file), None) => {
                debug!(?path, "File present on client but not on server");
                // TODO implement deleted file logic
                latest_timestamp = local_file.modified_ts;
                TransferDirection::Download
            }
            (None, Some(remote_file)) => {
                debug!(?path, "File present on server but not on client");
                // TODO implement deleted file logic
                latest_timestamp = remote_file.timestamp.unwrap_or_default();
                TransferDirection::Upload
            }
            (None, None) => unreachable!(),
            _ => TransferDirection::Skip,
        };

        let path_str = path.to_str().ok_or(HarmonicError::StringInvalid)?;

        debug!(?path_str, ?direction, "Pushing file into sync plan");
        sync_plan.push(FileAction {
            path: path_str.to_string(),
            direction: direction.into(),
            timestamp_latest_modified: Some(latest_timestamp),
        });
    }
    info!("Completed generating sync plan");

    Ok(sync_plan)
}

pub async fn generate_blocks_signatures(
    file_path: &PathBuf,
    config: &Config,
) -> Result<(BlockSignatures, BlockCache)> {
    info!("Generating block signatures");
    let mut cache = BlockCache { blocks: vec![] };

    let abs_path = get_absolute_path(&file_path, &config.sync_path)?;
    let data = match tokio::fs::read(&abs_path).await {
        Ok(data) => data,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(HarmonicError::Io(e)),
    };
    let mut bs: Vec<BlockSignature> = Vec::new();
    let mut buz_hasher = BuzHash::new(config.block_size as usize);

    for c in data.chunks(config.block_size as usize) {
        cache.blocks.push(c.into());
        bs.push(BlockSignature {
            weak_checksum: buz_hasher.compute(c),
            strong_checksum: blake3::hash(c).as_bytes().to_vec(),
        });
    }

    Ok((
        BlockSignatures {
            block_size: config.block_size,
            blocks: bs,
        },
        cache,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use prost_types::Timestamp;
    use tempfile::tempdir;

    #[test]
    fn test_file_status_vec_to_tree() {
        let file_statuses = vec![
            FileStatus {
                path: String::from("test1.txt"),
                timestamp: Some(Timestamp { seconds: 123, nanos: 456000 }),
                file_type: FileType::Other.into(),
                hash: vec![
                    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22,
                    23, 24, 25, 26, 27, 28, 29, 30, 31, 32,
                ],
            },
            FileStatus {
                path: String::from("test2.txt"),
                timestamp: Some(Timestamp { seconds: 654, nanos: 321000 }),
                file_type: FileType::Other.into(),
                hash: vec![
                    33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52,
                    53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64,
                ],
            },
        ];

        let tree = file_status_vec_to_tree(file_statuses);

        assert_eq!(tree.len(), 2);
        assert_eq!(
            tree.get(&PathBuf::from("test1.txt")),
            Some(&vec![
                1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23,
                24, 25, 26, 27, 28, 29, 30, 31, 32
            ])
        );
        assert_eq!(
            tree.get(&PathBuf::from("test2.txt")),
            Some(&vec![
                33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53,
                54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64
            ])
        );
    }

    #[test]
    fn test_compare_states_added_file() {
        let before_state = SyncState {
            last_sync_timestamp_micros: 1000,
            tree: BTreeMap::new(),
        };

        let mut now_tree = BTreeMap::new();
        now_tree.insert(
            PathBuf::from("new_file.txt"),
            FileMetadata {
                hash: [1; 32],
                modified_ts: Timestamp { seconds: 2000, nanos: 0 },
            },
        );

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
        before_tree.insert(
            PathBuf::from("old_file.txt"),
            FileMetadata {
                hash: [1; 32],
                modified_ts: Timestamp { seconds: 1000, nanos: 0 },
            },
        );

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
        before_tree.insert(
            PathBuf::from("modified.txt"),
            FileMetadata {
                hash: [1; 32],
                modified_ts: Timestamp { seconds: 1000, nanos: 0 },
            },
        );

        let before_state = SyncState {
            last_sync_timestamp_micros: 1000,
            tree: before_tree,
        };

        let mut now_tree = BTreeMap::new();
        now_tree.insert(
            PathBuf::from("modified.txt"),
            FileMetadata {
                hash: [2; 32],
                modified_ts: Timestamp { seconds: 2000, nanos: 0 },
            },
        );

        let now_state = SyncState {
            last_sync_timestamp_micros: 2000,
            tree: now_tree,
        };

        let diffs = compare_states(&before_state, &now_state);

        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].path, PathBuf::from("modified.txt"));
        assert!(matches!(diffs[0].change, ChangeType::Modified));
        assert_eq!(diffs[0].modified_ts, Timestamp { seconds: 2000, nanos: 0 });
    }

    #[test]
    fn test_compare_states_no_changes() {
        let mut tree = BTreeMap::new();
        tree.insert(
            PathBuf::from("unchanged.txt"),
            FileMetadata {
                hash: [1; 32],
                modified_ts: Timestamp { seconds: 1000, nanos: 0 },
            },
        );

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
        local_tree.insert(
            PathBuf::from("file.txt"),
            FileMetadata {
                hash: [1; 32],
                modified_ts: Timestamp { seconds: 2000, nanos: 0 },
            },
        );

        let state_now = SyncState {
            last_sync_timestamp_micros: 2000,
            tree: local_tree,
        };

        let remote_files = vec![FileStatus {
            path: String::from("file.txt"),
            timestamp: Some(Timestamp { seconds: 1000, nanos: 0 }),
            file_type: FileType::Other.into(),
            hash: vec![2; 32],
        }];

        let plan = generate_sync_plan(&state_now, &remote_files).unwrap();

        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].path, "file.txt");
        assert_eq!(plan[0].direction, TransferDirection::Upload as i32);
    }

    #[test]
    fn test_generate_sync_plan_remote_newer() {
        let mut local_tree = BTreeMap::new();
        local_tree.insert(
            PathBuf::from("file.txt"),
            FileMetadata {
                hash: [1; 32],
                modified_ts: Timestamp { seconds: 1000, nanos: 0 },
            },
        );

        let state_now = SyncState {
            last_sync_timestamp_micros: 1000,
            tree: local_tree,
        };

        let remote_files = vec![FileStatus {
            path: String::from("file.txt"),
            timestamp: Some(Timestamp { seconds: 2000, nanos: 0 }),
            file_type: FileType::Other.into(),
            hash: vec![2; 32],
        }];

        let plan = generate_sync_plan(&state_now, &remote_files).unwrap();

        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].path, "file.txt");
        assert_eq!(plan[0].direction, TransferDirection::Download as i32);
    }

    #[test]
    fn test_generate_sync_plan_files_identical() {
        let mut local_tree = BTreeMap::new();
        local_tree.insert(
            PathBuf::from("file.txt"),
            FileMetadata {
                hash: [1; 32],
                modified_ts: Timestamp { seconds: 1000, nanos: 0 },
            },
        );

        let state_now = SyncState {
            last_sync_timestamp_micros: 1000,
            tree: local_tree,
        };

        let remote_files = vec![FileStatus {
            path: String::from("file.txt"),
            timestamp: Some(Timestamp { seconds: 1000, nanos: 0 }),
            file_type: FileType::Other.into(),
            hash: vec![1; 32],
        }];

        let plan = generate_sync_plan(&state_now, &remote_files).unwrap();

        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].direction, TransferDirection::Skip as i32);
    }

    #[test]
    fn test_state_file_path() {
        let path = state_file_path().unwrap();
        assert!(path.ends_with(".harmonic/state.json"));
    }

    #[test]
    fn test_diff_to_file_status_conversion() {
        let diff = Diff {
            path: PathBuf::from("test_file.txt"),
            change: ChangeType::Modified,
            hash: [
                1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23,
                24, 25, 26, 27, 28, 29, 30, 31, 32,
            ],
            modified_ts: Timestamp { seconds: 123456789, nanos: 0 },
        };

        let file_status: FileStatus = FileStatus::try_from(diff).unwrap();

        assert_eq!(file_status.path, "test_file.txt");
        assert_eq!(file_status.timestamp, Some(Timestamp { seconds: 123456789, nanos: 0 }));
        assert_eq!(
            file_status.hash,
            vec![
                1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23,
                24, 25, 26, 27, 28, 29, 30, 31, 32
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

        assert_eq!(metadata.unwrap().hash, hash);
    }
}
