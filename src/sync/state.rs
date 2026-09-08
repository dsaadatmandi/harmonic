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
    BlockSignature, BlockSignatures, ChangeType as ProtoChangeType, FileAction, FileStatus,
    FileType, TransferDirection,
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

/// Paths are normalized to forward slashes at the protocol boundary so a
/// Windows client and a Unix server agree on the same relative path encoding
pub fn to_protocol_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

pub fn from_protocol_path(path: &str) -> PathBuf {
    PathBuf::from(path.replace('\\', "/"))
}

impl TryFrom<Diff> for FileStatus {
    type Error = HarmonicError;

    fn try_from(diff: Diff) -> Result<Self> {
        let change_type = match diff.change {
            ChangeType::Added => ProtoChangeType::Added,
            ChangeType::Removed => ProtoChangeType::Removed,
            ChangeType::Modified => ProtoChangeType::Modified,
        };

        Ok(FileStatus {
            path: to_protocol_path(&diff.path),
            timestamp: Some(diff.modified_ts),
            file_type: FileType::Other.into(),
            hash: diff.hash.to_vec(),
            change_type: change_type as i32,
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

fn state_dir_path() -> Result<PathBuf> {
    let mut path = PathBuf::from(".");
    path.push(".harmonic");

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

    fs::DirBuilder::new()
        .recursive(true)
        .create(state_dir_path()?)?;

    fs::write(state_file_path()?, state_json)?;

    Ok(())
}

/// Persists the sync state only when the sync itself succeeded, so a failed
/// transfer never records files as synced that were not actually transferred
pub fn save_state_on_success(
    sync_result: &anyhow::Result<()>,
    state: SyncState,
) -> anyhow::Result<()> {
    match sync_result {
        Ok(()) => Ok(save_state(state)?),
        Err(e) => Err(anyhow::anyhow!("Sync failed, state not saved: {:#}", e)),
    }
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
        tree.insert(from_protocol_path(&f.path), f.hash);
    }

    tree
}

/// Builds the full client status list sent to the server. Unchanged files are
/// included with an UNCHANGED change type so the server can detect deletions
/// on either side: REMOVED entries carry their previous metadata, everything
/// else reflects the current tree
pub fn build_status_list(
    before_state: &SyncState,
    now_state: &SyncState,
) -> Result<Vec<FileStatus>> {
    let diffs = compare_states(before_state, now_state);
    let diff_by_path: BTreeMap<&Path, &Diff> =
        diffs.iter().map(|d| (d.path.as_path(), d)).collect();

    let mut status_list: Vec<FileStatus> = Vec::with_capacity(now_state.tree.len() + diffs.len());

    for (path, metadata) in &now_state.tree {
        let change_type = match diff_by_path.get(path.as_path()) {
            Some(diff) => match diff.change {
                ChangeType::Added => ProtoChangeType::Added,
                ChangeType::Modified => ProtoChangeType::Modified,
                ChangeType::Removed => ProtoChangeType::Unchanged,
            },
            None => ProtoChangeType::Unchanged,
        };

        status_list.push(FileStatus {
            path: to_protocol_path(path),
            timestamp: Some(metadata.modified_ts),
            file_type: FileType::Other.into(),
            hash: metadata.hash.to_vec(),
            change_type: change_type as i32,
        });
    }

    // removed files no longer exist in the current tree so they are appended from the diffs
    for diff in &diffs {
        if matches!(diff.change, ChangeType::Removed) {
            status_list.push(FileStatus {
                path: to_protocol_path(&diff.path),
                timestamp: Some(diff.modified_ts),
                file_type: FileType::Other.into(),
                hash: diff.hash.to_vec(),
                change_type: ProtoChangeType::Removed as i32,
            });
        }
    }

    Ok(status_list)
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
        .map(|r| (from_protocol_path(&r.path), r))
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

        let remote_change = remote
            .map(|r| ProtoChangeType::try_from(r.change_type).unwrap_or(ProtoChangeType::Added));

        let direction: TransferDirection = match (local, remote) {
            (Some(_), Some(_)) if remote_change == Some(ProtoChangeType::Removed) => {
                debug!(?path, "File deleted on client (remote), propagating deletion to server (local)");
                TransferDirection::Delete
            }
            (Some(local_file), Some(remote_file)) if remote_file.hash != local_file.hash => {
                let remote_file_timestamp = remote_file.timestamp.unwrap_or_default();
                if proto_timestamp_gt(local_file.modified_ts, remote_file_timestamp) {
                    // Server (local) file is newer → Download (server sends to client)
                    latest_timestamp = local_file.modified_ts;
                    TransferDirection::Download
                } else if proto_timestamp_gt(remote_file_timestamp, local_file.modified_ts) {
                    // Client (remote) file is newer → Upload (client sends to server)
                    latest_timestamp = remote_file_timestamp;
                    TransferDirection::Upload
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
                debug!(?path, "File present on server (local) but not on client (remote)");
                // file was never on the client, send it down
                latest_timestamp = local_file.modified_ts;
                TransferDirection::Download
            }
            (None, Some(remote_file)) if remote_change == Some(ProtoChangeType::Added)
                || remote_change == Some(ProtoChangeType::Modified) =>
            {
                debug!(?path, "File present on client (remote) but not on server (local)");
                latest_timestamp = remote_file.timestamp.unwrap_or_default();
                TransferDirection::Upload
            }
            (None, Some(_)) => {
                debug!(?path, "File unchanged on client (remote) but missing on server (local)");
                // client had the file at last sync and did not change it, it was deleted here
                TransferDirection::Delete
            }
            (None, None) => unreachable!(),
            _ => TransferDirection::Skip,
        };

        debug!(?direction, "Pushing file into sync plan");
        sync_plan.push(FileAction {
            path: to_protocol_path(path),
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
    fn test_to_protocol_path_converts_backslashes() {
        assert_eq!(to_protocol_path(Path::new("dir/file.txt")), "dir/file.txt");
        assert_eq!(to_protocol_path(Path::new("dir\\file.txt")), "dir/file.txt");
        assert_eq!(
            to_protocol_path(Path::new("dir\\sub\\file.txt")),
            "dir/sub/file.txt"
        );
    }

    #[test]
    fn test_from_protocol_path_handles_both_separators() {
        let forward = from_protocol_path("dir/file.txt");
        let backward = from_protocol_path("dir\\file.txt");

        assert_eq!(forward.components().count(), 2);
        assert_eq!(backward.components().count(), 2);
        assert_eq!(forward, backward);
    }

    #[test]
    fn test_protocol_path_round_trip() {
        let original = PathBuf::from("dir\\sub\\file.txt");
        let round_tripped = from_protocol_path(&to_protocol_path(&original));

        assert_eq!(round_tripped.components().count(), 3);
    }

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
                change_type: ProtoChangeType::Added as i32,
            },
            FileStatus {
                path: String::from("test2.txt"),
                timestamp: Some(Timestamp { seconds: 654, nanos: 321000 }),
                file_type: FileType::Other.into(),
                hash: vec![
                    33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52,
                    53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64,
                ],
                change_type: ProtoChangeType::Added as i32,
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
    fn test_file_status_vec_to_tree_normalizes_separators() {
        let file_statuses = vec![FileStatus {
            path: String::from("dir/file.txt"),
            timestamp: Some(Timestamp::default()),
            file_type: FileType::Other.into(),
            hash: vec![1; 32],
            change_type: ProtoChangeType::Added as i32,
        }];

        let tree = file_status_vec_to_tree(file_statuses);

        let key = tree.keys().next().unwrap();
        assert_eq!(key.components().count(), 2, "dir/file.txt should be a nested path");
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
            change_type: ProtoChangeType::Modified as i32,
        }];

        let plan = generate_sync_plan(&state_now, &remote_files).unwrap();

        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].path, "file.txt");
        // Server (local) timestamp 2000 > Client (remote) timestamp 1000
        // Server is newer → Download (server sends to client)
        assert_eq!(plan[0].direction, TransferDirection::Download as i32);
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
            change_type: ProtoChangeType::Modified as i32,
        }];

        let plan = generate_sync_plan(&state_now, &remote_files).unwrap();

        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].path, "file.txt");
        // Client (remote) timestamp 2000 > Server (local) timestamp 1000
        // Client is newer → Upload (client sends to server)
        assert_eq!(plan[0].direction, TransferDirection::Upload as i32);
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
            change_type: ProtoChangeType::Unchanged as i32,
        }];

        let plan = generate_sync_plan(&state_now, &remote_files).unwrap();

        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].direction, TransferDirection::Skip as i32);
    }

    #[test]
    fn test_generate_sync_plan_client_removed_deletes_on_server() {
        // Scenario: client deleted the file, server still has it
        // Expected: Delete so the server removes its copy instead of re-downloading
        let mut local_tree = BTreeMap::new();
        local_tree.insert(
            PathBuf::from("deleted.txt"),
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
            path: String::from("deleted.txt"),
            timestamp: Some(Timestamp { seconds: 1000, nanos: 0 }),
            file_type: FileType::Other.into(),
            hash: vec![1; 32],
            change_type: ProtoChangeType::Removed as i32,
        }];

        let plan = generate_sync_plan(&state_now, &remote_files).unwrap();

        assert_eq!(plan.len(), 1);
        assert_eq!(
            plan[0].direction,
            TransferDirection::Delete as i32,
            "Client deletion must propagate as Delete, not Download"
        );
    }

    #[test]
    fn test_generate_sync_plan_server_deleted_propagates_to_client() {
        // Scenario: server deleted the file, client still has it unchanged
        // Expected: Delete so the client removes its copy instead of re-uploading
        let state_now = SyncState {
            last_sync_timestamp_micros: 1000,
            tree: BTreeMap::new(),
        };

        let remote_files = vec![FileStatus {
            path: String::from("deleted.txt"),
            timestamp: Some(Timestamp { seconds: 1000, nanos: 0 }),
            file_type: FileType::Other.into(),
            hash: vec![1; 32],
            change_type: ProtoChangeType::Unchanged as i32,
        }];

        let plan = generate_sync_plan(&state_now, &remote_files).unwrap();

        assert_eq!(plan.len(), 1);
        assert_eq!(
            plan[0].direction,
            TransferDirection::Delete as i32,
            "Server deletion must propagate as Delete, not Upload"
        );
    }

    #[test]
    fn test_generate_sync_plan_both_deleted_is_noop_delete() {
        // Scenario: file was deleted on both sides
        // Expected: Delete which is a no-op on both sides
        let state_now = SyncState {
            last_sync_timestamp_micros: 1000,
            tree: BTreeMap::new(),
        };

        let remote_files = vec![FileStatus {
            path: String::from("deleted.txt"),
            timestamp: Some(Timestamp { seconds: 1000, nanos: 0 }),
            file_type: FileType::Other.into(),
            hash: vec![1; 32],
            change_type: ProtoChangeType::Removed as i32,
        }];

        let plan = generate_sync_plan(&state_now, &remote_files).unwrap();

        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].direction, TransferDirection::Delete as i32);
    }

    #[test]
    fn test_generate_sync_plan_remote_added_uploads_when_missing_locally() {
        // Scenario: client added a new file, server does not have it
        // Expected: Upload
        let state_now = SyncState {
            last_sync_timestamp_micros: 1000,
            tree: BTreeMap::new(),
        };

        let remote_files = vec![FileStatus {
            path: String::from("new.txt"),
            timestamp: Some(Timestamp { seconds: 2000, nanos: 0 }),
            file_type: FileType::Other.into(),
            hash: vec![1; 32],
            change_type: ProtoChangeType::Added as i32,
        }];

        let plan = generate_sync_plan(&state_now, &remote_files).unwrap();

        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].direction, TransferDirection::Upload as i32);
    }

    #[test]
    fn test_generate_sync_plan_remote_modified_uploads_when_missing_locally() {
        // Scenario: client modified a file that the server never had
        // Expected: Upload
        let state_now = SyncState {
            last_sync_timestamp_micros: 1000,
            tree: BTreeMap::new(),
        };

        let remote_files = vec![FileStatus {
            path: String::from("modified.txt"),
            timestamp: Some(Timestamp { seconds: 2000, nanos: 0 }),
            file_type: FileType::Other.into(),
            hash: vec![1; 32],
            change_type: ProtoChangeType::Modified as i32,
        }];

        let plan = generate_sync_plan(&state_now, &remote_files).unwrap();

        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].direction, TransferDirection::Upload as i32);
    }

    #[test]
    fn test_generate_sync_plan_absent_remote_entry_downloads() {
        // Scenario: server has a file that is not in the client status list at all
        // e.g. a brand new client. Expected: Download
        let mut local_tree = BTreeMap::new();
        local_tree.insert(
            PathBuf::from("server_file.txt"),
            FileMetadata {
                hash: [1; 32],
                modified_ts: Timestamp { seconds: 1000, nanos: 0 },
            },
        );

        let state_now = SyncState {
            last_sync_timestamp_micros: 1000,
            tree: local_tree,
        };

        let plan = generate_sync_plan(&state_now, &vec![]).unwrap();

        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].direction, TransferDirection::Download as i32);
    }

    #[test]
    fn test_generate_sync_plan_normalizes_plan_paths() {
        // Scenario: tree key contains a Windows style separator (simulating a
        // Windows client). Expected: plan path uses forward slashes
        let mut local_tree = BTreeMap::new();
        local_tree.insert(
            PathBuf::from("dir\\file.txt"),
            FileMetadata {
                hash: [1; 32],
                modified_ts: Timestamp { seconds: 1000, nanos: 0 },
            },
        );

        let state_now = SyncState {
            last_sync_timestamp_micros: 1000,
            tree: local_tree,
        };

        let plan = generate_sync_plan(&state_now, &vec![]).unwrap();

        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].path, "dir/file.txt");
        assert!(!plan[0].path.contains('\\'));
    }

    #[test]
    fn test_state_file_path() {
        let path = state_file_path().unwrap();
        assert!(path.ends_with(".harmonic/state.json"));
    }

    #[test]
    fn test_diff_to_file_status_conversion() {
        let diff = Diff {
            path: PathBuf::from("nested\\test_file.txt"),
            change: ChangeType::Removed,
            hash: [
                1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23,
                24, 25, 26, 27, 28, 29, 30, 31, 32,
            ],
            modified_ts: Timestamp { seconds: 123456789, nanos: 0 },
        };

        let file_status: FileStatus = FileStatus::try_from(diff).unwrap();

        assert_eq!(file_status.path, "nested/test_file.txt");
        assert_eq!(file_status.change_type, ProtoChangeType::Removed as i32);
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
    fn test_build_status_list_classifies_all_changes() {
        // Scenario: one file kept, one modified, one removed, one added since
        // last sync. Expected: full status list where every file carries the
        // correct change type, including the removed file
        let dir = tempdir().unwrap();
        let root = PathBuf::from(dir.path());

        let keep = root.join("keep.txt");
        let modify = root.join("modify.txt");
        let remove = root.join("remove.txt");

        fs::write(&keep, "unchanged").unwrap();
        fs::write(&modify, "original").unwrap();
        fs::write(&remove, "will be removed").unwrap();

        let before_state = generate_state(&root, false).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(&modify, "modified content").unwrap();
        fs::remove_file(&remove).unwrap();
        fs::write(root.join("new.txt"), "brand new").unwrap();

        let now_state = generate_state(&root, false).unwrap();

        let status_list = build_status_list(&before_state, &now_state).unwrap();

        assert_eq!(status_list.len(), 4, "3 current files plus 1 removed entry");

        let by_path: BTreeMap<&str, ProtoChangeType> = status_list
            .iter()
            .map(|s| (s.path.as_str(), ProtoChangeType::try_from(s.change_type).unwrap()))
            .collect();

        assert_eq!(by_path.get("keep.txt"), Some(&ProtoChangeType::Unchanged));
        assert_eq!(by_path.get("modify.txt"), Some(&ProtoChangeType::Modified));
        assert_eq!(by_path.get("new.txt"), Some(&ProtoChangeType::Added));
        assert_eq!(by_path.get("remove.txt"), Some(&ProtoChangeType::Removed));
    }

    #[test]
    fn test_build_status_list_paths_use_protocol_separators() {
        // Simulate a tree key with Windows style separators
        let mut tree = BTreeMap::new();
        tree.insert(
            PathBuf::from("dir\\file.txt"),
            FileMetadata {
                hash: [1; 32],
                modified_ts: Timestamp::default(),
            },
        );

        let state = SyncState {
            last_sync_timestamp_micros: 1000,
            tree,
        };

        let status_list = build_status_list(&state, &state).unwrap();

        assert_eq!(status_list.len(), 1);
        assert_eq!(status_list[0].path, "dir/file.txt");
        assert_eq!(
            status_list[0].change_type,
            ProtoChangeType::Unchanged as i32
        );
    }

    #[test]
    fn test_save_state_on_success_and_failure() {
        // Scenario: state must only be persisted when the sync succeeded.
        // Expected: failed sync leaves the previous state untouched, successful
        // sync writes the new state. This test switches the working directory
        // into a tempdir because state paths are process relative
        let dir = tempdir().unwrap();
        let original_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();

        let state_v1 = SyncState {
            last_sync_timestamp_micros: 1000,
            tree: BTreeMap::new(),
        };

        let mut tree_v2 = BTreeMap::new();
        tree_v2.insert(
            PathBuf::from("file.txt"),
            FileMetadata {
                hash: [2; 32],
                modified_ts: Timestamp { seconds: 2000, nanos: 0 },
            },
        );
        let state_v2 = SyncState {
            last_sync_timestamp_micros: 2000,
            tree: tree_v2,
        };

        // seed a previous successful sync
        save_state(state_v1).unwrap();

        // failed sync must not overwrite the stored state
        let failure = save_state_on_success(&Err(anyhow::anyhow!("transfer failed")), state_v2.clone());
        assert!(failure.is_err());

        let persisted = load_state().unwrap();
        assert_eq!(persisted.tree.len(), 0, "failed sync must not save state");

        // successful sync must overwrite the stored state
        save_state_on_success(&Ok(()), state_v2).unwrap();

        let persisted = load_state().unwrap();
        assert_eq!(persisted.tree.len(), 1, "successful sync must save state");
        assert!(persisted.tree.contains_key(&PathBuf::from("file.txt")));

        std::env::set_current_dir(original_dir).unwrap();
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
