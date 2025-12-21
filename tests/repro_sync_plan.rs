use harmonic::proto::{FileStatus, FileType, TransferDirection};
use harmonic::sync::state::{FileMetadata, SyncState, generate_sync_plan};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[test]
fn test_sync_plan_direction() {
    // Local (Client) is newer
    let mut local_tree = BTreeMap::new();
    local_tree.insert(
        PathBuf::from("file.txt"),
        FileMetadata {
            hash: [1; 32],
            modified_ts: prost_types::Timestamp {
                seconds: 2000,
                nanos: 0,
            },
        },
    );
    let state_now = SyncState {
        last_sync_timestamp_micros: 2000,
        tree: local_tree,
    };

    // Remote (Server) is older
    let remote_files = vec![FileStatus {
        path: String::from("file.txt"),
        timestamp: Some(prost_types::Timestamp {
            seconds: 1000,
            nanos: 0,
        }),
        file_type: FileType::Other.into(),
        hash: vec![2; 32],
    }];

    let plan = generate_sync_plan(&state_now, &remote_files).unwrap();

    // If Client is newer, Client should SEND file (Upload).

    println!(
        "Direction for Client(Newer) vs Server(Older): {:?}",
        plan[0].direction
    );

    if plan[0].direction == TransferDirection::Download as i32 {
        println!("Current logic: Download (Client Pulls / Server Pushes)");
    } else if plan[0].direction == TransferDirection::Upload as i32 {
        println!("Current logic: Upload (Client Pushes / Server Pulls)");
    }
}

#[test]
fn test_generate_sync_plan_local_newer() {
    // Server (local) timestamp: 2000
    // Client (remote) timestamp: 1000
    // Server is newer → Download (server sends to client)
    let mut local_tree = BTreeMap::new();
    local_tree.insert(
        PathBuf::from("file.txt"),
        FileMetadata {
            hash: [1; 32],
            modified_ts: prost_types::Timestamp {
                seconds: 2000,
                nanos: 0,
            },
        },
    );

    let state_now = SyncState {
        last_sync_timestamp_micros: 2000,
        tree: local_tree,
    };

    let remote_files = vec![FileStatus {
        path: String::from("file.txt"),
        timestamp: Some(prost_types::Timestamp{
            seconds: 1000,
            nanos: 0
        }),
        file_type: FileType::Other.into(),
        hash: vec![2; 32],
    }];

    let plan = generate_sync_plan(&state_now, &remote_files).unwrap();

    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].path, "file.txt");
    assert_eq!(plan[0].direction, TransferDirection::Download as i32);
}

#[test]
fn test_generate_sync_plan_remote_newer() {
    // Server (local) timestamp: 1000
    // Client (remote) timestamp: 2000
    // Client is newer → Upload (client sends to server)
    let mut local_tree = BTreeMap::new();
    local_tree.insert(
        PathBuf::from("file.txt"),
        FileMetadata {
            hash: [1; 32],
            modified_ts: prost_types::Timestamp {
                seconds: 1000,
                nanos: 0,
            },
        },
    );

    let state_now = SyncState {
        last_sync_timestamp_micros: 1000,
        tree: local_tree,
    };

    let remote_files = vec![FileStatus {
        path: String::from("file.txt"),
        timestamp: Some(prost_types::Timestamp{
            seconds: 2000,
            nanos: 0
        }),
        file_type: FileType::Other.into(),
        hash: vec![2; 32],
    }];

    let plan = generate_sync_plan(&state_now, &remote_files).unwrap();

    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].path, "file.txt");
    assert_eq!(plan[0].direction, TransferDirection::Upload as i32);
}
