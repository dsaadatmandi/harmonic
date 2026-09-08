// State file persistence tests
//
// The state file lives in .harmonic relative to the process working
// directory, so it is exercised in a single test function inside this
// process-isolated integration binary with the working directory switched
// into a tempdir

use harmonic::sync::state::{FileMetadata, SyncState, load_state, save_state};
use std::collections::BTreeMap;
use std::path::PathBuf;
use prost_types::Timestamp;
use tempfile::tempdir;

#[test]
fn test_save_and_load_state_round_trip() {
    let dir = tempdir().unwrap();
    std::env::set_current_dir(dir.path()).unwrap();

    // the state directory is created on demand
    let mut tree = BTreeMap::new();
    tree.insert(
        PathBuf::from("file.txt"),
        FileMetadata {
            hash: [2; 32],
            modified_ts: Timestamp { seconds: 2000, nanos: 0 },
        },
    );
    let state = SyncState {
        last_sync_timestamp_micros: 2000,
        tree,
    };

    save_state(state).unwrap();
    assert!(dir.path().join(".harmonic/state.json").exists());

    let persisted = load_state().unwrap();
    assert_eq!(persisted.last_sync_timestamp_micros, 2000);
    assert!(persisted.tree.contains_key(&PathBuf::from("file.txt")));
    assert_eq!(
        persisted.tree.get(&PathBuf::from("file.txt")).unwrap().hash,
        [2; 32]
    );
}
