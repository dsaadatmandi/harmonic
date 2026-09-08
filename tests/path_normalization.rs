// Cross-platform path handling tests
//
// Paths are exchanged as forward-slash separated strings over the protocol so
// a Windows client and a Unix server agree on the same relative path encoding.
// These tests feed Windows style paths through the protocol boundary and
// verify they resolve to nested paths regardless of the host platform

use harmonic::proto::{ChangeType, FileStatus, FileType, TransferDirection};
use harmonic::sync::state::{FileMetadata, SyncState};
use harmonic::sync::transfer::get_absolute_path;
use harmonic::sync::*;
use std::collections::BTreeMap;
use std::path::PathBuf;
use tempfile::tempdir;
use prost_types::Timestamp;

#[test]
fn test_nested_state_paths_serialize_with_forward_slashes() {
    // Scenario: a real nested file is scanned into the state tree
    // Expected: the protocol path uses forward slashes on every platform
    let dir = tempdir().unwrap();
    let root = PathBuf::from(dir.path());

    let subdir = root.join("notes");
    std::fs::create_dir(&subdir).unwrap();
    std::fs::write(subdir.join("idea.txt"), "nested content").unwrap();

    let state = generate_state(&root, false).unwrap();

    let status_list: Vec<FileStatus> = state
        .tree
        .iter()
        .map(|(path, meta)| FileStatus {
            path: to_protocol_path(path),
            timestamp: Some(meta.modified_ts),
            file_type: FileType::Other.into(),
            hash: meta.hash.to_vec(),
            change_type: ChangeType::Unchanged as i32,
        })
        .collect();

    assert_eq!(status_list.len(), 1);
    assert_eq!(
        status_list[0].path, "notes/idea.txt",
        "protocol paths must use forward slashes"
    );
}

#[test]
fn test_windows_client_path_resolves_as_nested_on_any_platform() {
    // Scenario: a Windows client reports "notes\\idea.txt" and the server
    // plans a Download for it
    // Expected: the plan path is normalized and resolves to the nested file
    // inside the server sync root
    let dir = tempdir().unwrap();
    let server_root = PathBuf::from(dir.path());

    let mut tree = BTreeMap::new();
    tree.insert(
        from_protocol_path("notes\\idea.txt"),
        FileMetadata {
            hash: [1; 32],
            modified_ts: Timestamp { seconds: 1000, nanos: 0 },
        },
    );
    let server_state = SyncState {
        last_sync_timestamp_micros: 1000,
        tree,
    };

    let client_files = vec![FileStatus {
        path: String::from("notes\\idea.txt"),
        timestamp: Some(Timestamp { seconds: 1000, nanos: 0 }),
        file_type: FileType::Other.into(),
        hash: vec![1; 32],
        change_type: ChangeType::Unchanged as i32,
    }];

    let plan = generate_sync_plan(&server_state, &client_files).unwrap();

    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].path, "notes/idea.txt");
    assert_eq!(plan[0].direction, TransferDirection::Skip as i32);

    // the client resolves the action path against its sync root
    let resolved = get_absolute_path(&from_protocol_path(&plan[0].path), &server_root).unwrap();
    assert_eq!(resolved, server_root.join("notes").join("idea.txt"));
}

#[test]
fn test_file_status_vec_to_tree_from_windows_style_paths() {
    // Scenario: FileStatus messages arrive with Windows style separators
    // Expected: tree keys are nested paths, not flat names containing backslashes
    let file_statuses = vec![FileStatus {
        path: String::from("notes\\idea.txt"),
        timestamp: Some(Timestamp::default()),
        file_type: FileType::Other.into(),
        hash: vec![1; 32],
        change_type: ChangeType::Unchanged as i32,
    }];

    let tree = file_status_vec_to_tree(file_statuses);

    let key = tree.keys().next().unwrap();
    assert_eq!(
        key.components().count(),
        2,
        "windows style path must map to a nested path"
    );
}

#[test]
fn test_modified_nested_file_round_trips_through_protocol() {
    // Scenario: a nested file is modified between syncs and reported to the server
    // Expected: status list, sync plan and local resolution all agree on the
    // same forward-slash path
    // The client timestamp is synthetic and newer than the server one, real
    // mtimes are not reliable for ordering across two separate writes
    let client_dir = tempdir().unwrap();
    let server_dir = tempdir().unwrap();
    let client_root = PathBuf::from(client_dir.path());
    let server_root = PathBuf::from(server_dir.path());

    let subdir = client_root.join("notes");
    std::fs::create_dir(&subdir).unwrap();
    std::fs::write(subdir.join("idea.txt"), "v1").unwrap();

    std::fs::write(subdir.join("idea.txt"), "v2").unwrap();
    let now = generate_state(&client_root, false).unwrap();
    let client_meta = now.tree.get(&PathBuf::from("notes/idea.txt")).unwrap();

    // server has an older copy
    let server_subdir = server_root.join("notes");
    std::fs::create_dir(&server_subdir).unwrap();
    std::fs::write(server_subdir.join("idea.txt"), "v1").unwrap();
    let server_state = generate_state(&server_root, false).unwrap();
    let server_meta = server_state.tree.get(&PathBuf::from("notes").join("idea.txt")).unwrap();

    let newer_ts = prost_types::Timestamp {
        seconds: server_meta.modified_ts.seconds + 10,
        nanos: server_meta.modified_ts.nanos,
    };

    let status_list = vec![FileStatus {
        path: to_protocol_path(PathBuf::from("notes").join("idea.txt").as_path()),
        timestamp: Some(newer_ts),
        file_type: FileType::Other.into(),
        hash: client_meta.hash.to_vec(),
        change_type: ChangeType::Modified as i32,
    }];

    assert_eq!(status_list[0].path, "notes/idea.txt");

    let plan = generate_sync_plan(&server_state, &status_list).unwrap();

    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].path, "notes/idea.txt");
    assert_eq!(plan[0].direction, TransferDirection::Upload as i32);

    let resolved = get_absolute_path(&from_protocol_path(&plan[0].path), &server_root).unwrap();
    assert_eq!(resolved, server_root.join("notes").join("idea.txt"));
}

