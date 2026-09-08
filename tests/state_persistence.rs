// State persistence tests
//
// The client must only persist its sync state when the transfer succeeded,
// otherwise a partially transferred file would be recorded as synced and its
// changes would be lost until the next local modification. This test runs in
// its own process (each integration test file is a separate binary) and in a
// single function because state paths are process relative and the working
// directory is switched into a tempdir

use harmonic::sync::state::{FileMetadata, SyncState, load_state, save_state, save_state_on_success};
use std::collections::BTreeMap;
use std::path::PathBuf;
use prost_types::Timestamp;
use tempfile::tempdir;

fn state_with_file(name: &str, hash_byte: u8, seconds: i64) -> SyncState {
    let mut tree = BTreeMap::new();
    tree.insert(
        PathBuf::from(name),
        FileMetadata {
            hash: [hash_byte; 32],
            modified_ts: Timestamp { seconds, nanos: 0 },
        },
    );

    SyncState {
        last_sync_timestamp_micros: seconds,
        tree,
    }
}

#[test]
fn test_state_is_only_persisted_on_successful_sync() {
    let dir = tempdir().unwrap();
    std::env::set_current_dir(dir.path()).unwrap();

    // Scenario: the very first sync fails before any state exists
    // Expected: no state file is created, the next run starts a fresh sync
    let state = state_with_file("file.txt", 1, 1000);
    let result = save_state_on_success(&Err(anyhow::anyhow!("connection refused")), state);

    assert!(result.is_err());
    assert!(
        !dir.path().join(".harmonic/state.json").exists(),
        "state file must not be created by a failed sync"
    );

    // Scenario: a previous sync succeeded, then the transfer fails
    // Expected: save_state_on_success errors and the stored state still
    // reflects the previous sync, so the failed transfer is retried later
    let old_state = state_with_file("file.txt", 1, 1000);
    save_state(old_state).unwrap();

    let unsynced_state = state_with_file("file.txt", 2, 2000);
    let result = save_state_on_success(&Err(anyhow::anyhow!("transfer failed")), unsynced_state);

    assert!(result.is_err(), "a failed sync must not report success");

    let persisted = load_state().unwrap();
    assert_eq!(persisted.last_sync_timestamp_micros, 1000, "previous state must be kept");
    assert_eq!(
        persisted.tree.get(&PathBuf::from("file.txt")).unwrap().hash,
        [1; 32],
        "old file hash must be kept so the transfer is retried"
    );

    // Scenario: a sync completes successfully
    // Expected: the new state is written and load_state returns it
    let new_state = state_with_file("file.txt", 2, 2000);
    save_state_on_success(&Ok(()), new_state).unwrap();

    let persisted = load_state().unwrap();
    assert_eq!(persisted.tree.len(), 1);
    assert!(persisted.tree.contains_key(&PathBuf::from("file.txt")));
    assert_eq!(persisted.last_sync_timestamp_micros, 2000);
}
