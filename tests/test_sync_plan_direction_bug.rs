// due to a bug with the file transfer direction, adding extensive tests
//
// Protocol definition (from file_system_operations.rs):
// - Upload: Client → Server
// - Download: Server → Client

use harmonic::sync::*;
use harmonic::proto::{FileChangeType, FileStatus, FileType, TransferDirection};
use std::path::PathBuf;
use tempfile::tempdir;

#[test]
fn test_sync_plan_new_file_on_client() {
    // Scenario: Client has a new file that doesn't exist on server
    // Expected: Direction should be Upload (Client → Server)

    let server_dir = tempdir().unwrap();
    let server_root = PathBuf::from(server_dir.path());

    // Server has no files (empty state)
    let server_state = generate_state(&server_root, false).unwrap();

    // Client has a file
    let client_file = FileStatus {
        path: "new_file.txt".to_string(),
        hash: vec![1, 2, 3],
        timestamp: Some(prost_types::Timestamp::default()),
        file_type: FileType::Other as i32,
        change_type: FileChangeType::Added as i32,
    };
    let client_files = vec![client_file];

    // Generate sync plan from server's perspective
    let sync_plan = generate_sync_plan(&server_state, &client_files).unwrap();

    // Verify: When file exists on client but not server, direction should be Upload
    assert_eq!(sync_plan.len(), 1);
    assert_eq!(sync_plan[0].path, "new_file.txt");
    assert_eq!(
        sync_plan[0].direction,
        TransferDirection::Upload as i32,
        "When file exists on client but not on server, direction should be Upload (Client → Server)"
    );
}

#[test]
fn test_sync_plan_new_file_on_server() {
    // Scenario: Server has a file that doesn't exist on client
    // Expected: Direction should be Download (Server → Client)

    let server_dir = tempdir().unwrap();
    let server_root = PathBuf::from(server_dir.path());

    // Server has a file
    std::fs::write(server_root.join("server_file.txt"), "server content").unwrap();
    let server_state = generate_state(&server_root, false).unwrap();

    // Client has no files (empty state)
    let client_files: Vec<FileStatus> = vec![];

    // Generate sync plan (from server's perspective)
    let sync_plan = generate_sync_plan(&server_state, &client_files).unwrap();

    // Verify: When file exists on server but not client, direction should be Download
    assert_eq!(sync_plan.len(), 1);
    assert_eq!(sync_plan[0].path, "server_file.txt");
    assert_eq!(
        sync_plan[0].direction,
        TransferDirection::Download as i32,
        "When file exists on server but not on client, direction should be Download (Server → Client)"
    );
}

#[test]
fn test_sync_plan_client_file_newer() {
    // Scenario: Both have the file, but client's version is newer
    // Expected: Direction should be Upload (Client → Server)

    let server_dir = tempdir().unwrap();
    let server_root = PathBuf::from(server_dir.path());

    // Server has old version
    std::fs::write(server_root.join("modified.txt"), "old content").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(10)); // Ensure timestamp difference

    let server_state = generate_state(&server_root, false).unwrap();
    let server_file_metadata = server_state.tree.get(&PathBuf::from("modified.txt")).unwrap();

    // Client has newer version
    let new_timestamp = prost_types::Timestamp {
        seconds: server_file_metadata.modified_ts.seconds + 10,
        nanos: server_file_metadata.modified_ts.nanos,
    };

    let client_file = FileStatus {
        path: "modified.txt".to_string(),
        hash: vec![1, 2, 3], // Different hash
        timestamp: Some(new_timestamp),
        file_type: FileType::Other as i32,
        change_type: FileChangeType::Modified as i32,
    };
    let client_files = vec![client_file];

    // Generate sync plan (from server's perspective)
    let sync_plan = generate_sync_plan(&server_state, &client_files).unwrap();

    // Verify: When client file is newer, direction should be Upload
    assert_eq!(sync_plan.len(), 1);
    assert_eq!(sync_plan[0].path, "modified.txt");
    assert_eq!(
        sync_plan[0].direction,
        TransferDirection::Upload as i32,
        "When client file is newer, direction should be Upload (Client → Server)"
    );
}

#[test]
fn test_sync_plan_server_file_newer() {
    // Scenario: Both have the file, but server's version is newer
    // Expected: Direction should be Download (Server → Client)

    let server_dir = tempdir().unwrap();
    let server_root = PathBuf::from(server_dir.path());

    // Server has new version
    std::fs::write(server_root.join("modified.txt"), "new content").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(10)); // Ensure timestamp difference

    let server_state = generate_state(&server_root, false).unwrap();
    let server_file_metadata = server_state.tree.get(&PathBuf::from("modified.txt")).unwrap();

    // Client has older version
    let old_timestamp = prost_types::Timestamp {
        seconds: server_file_metadata.modified_ts.seconds - 10,
        nanos: server_file_metadata.modified_ts.nanos,
    };

    let client_file = FileStatus {
        path: "modified.txt".to_string(),
        hash: vec![4, 5, 6], // Different hash
        timestamp: Some(old_timestamp),
        file_type: FileType::Other as i32,
        change_type: FileChangeType::Modified as i32,
    };
    let client_files = vec![client_file];

    // Generate sync plan (from server's perspective)
    let sync_plan = generate_sync_plan(&server_state, &client_files).unwrap();

    // Verify: When server file is newer, direction should be Download
    assert_eq!(sync_plan.len(), 1);
    assert_eq!(sync_plan[0].path, "modified.txt");
    assert_eq!(
        sync_plan[0].direction,
        TransferDirection::Download as i32,
        "When server file is newer, direction should be Download (Server → Client)"
    );
}
