// Deletion propagation tests
//
// DELETE (either side initiates):
// 1. Client computes its full status list including REMOVED entries and
//    UNCHANGED entries via build_status_list
// 2. Server generates the sync plan, a file reported REMOVED by the client
//    becomes a Delete action unless the surviving copy is newer, a file
//    missing on the server that the client reports UNCHANGED also becomes a
//    Delete action
// 3. The client executing the plan deletes its local copy (no-op if already
//    deleted) and sends the FileAction through the stream
// 4. The side receiving the FileAction deletes its own copy (no-op if gone)
//
// Delete is idempotent on both sides so both copies always converge to deleted

use harmonic::proto::{FileChangeType, FileAction, FileStatus, FileType, TransferDirection};
use harmonic::sync::handler::{delete_sync_file, handle_sync_payload};
use harmonic::sync::*;
use std::{fs, path::PathBuf};
use tempfile::tempdir;
use filetime::FileTime;
use futures::SinkExt;
use tokio::sync::mpsc;
use tokio_util::sync::PollSender;
use harmonic::utils::HarmonicError;

mod common;

fn unchanged_status(path: &str, hash: [u8; 32], seconds: i64) -> FileStatus {
    FileStatus {
        path: path.to_string(),
        timestamp: Some(prost_types::Timestamp {
            seconds,
            nanos: 0,
        }),
        file_type: FileType::Other.into(),
        hash: hash.to_vec(),
        change_type: FileChangeType::Unchanged as i32,
    }
}

#[tokio::test]
async fn test_client_deletion_propagates_to_server() {
    let client_dir = tempdir().unwrap();
    let server_dir = tempdir().unwrap();
    let client_root = PathBuf::from(client_dir.path());
    let server_root = PathBuf::from(server_dir.path());

    let content = b"shared content";
    let client_file = client_root.join("gone.txt");
    fs::write(&client_file, content).unwrap();
    fs::write(server_root.join("gone.txt"), content).unwrap();

    // the deleted content is newer than the server copy, real mtimes are not
    // reliable for ordering across two separate writes
    filetime::set_file_mtime(&client_file, FileTime::from_unix_time(2_000_000_000, 0)).unwrap();

    let client_before = generate_state(&client_root, false).unwrap();
    fs::remove_file(client_root.join("gone.txt")).unwrap();
    let client_after = generate_state(&client_root, false).unwrap();
    let server_state = generate_state(&server_root, false).unwrap();

    let status_list = build_status_list(&client_before, &client_after);
    assert_eq!(
        status_list.len(),
        1,
        "the deleted file must appear in the status list"
    );
    assert_eq!(status_list[0].change_type, FileChangeType::Removed as i32);

    let sync_plan = generate_sync_plan(&server_state, &status_list).unwrap();

    assert_eq!(sync_plan.len(), 1);
    assert_eq!(sync_plan[0].path, "gone.txt");
    assert_eq!(
        sync_plan[0].direction,
        TransferDirection::Delete as i32,
        "client deletion must not resurrect the file via Download"
    );

    // server processes the Delete FileAction
    let server_config = common::create_test_config(&server_root);
    let (tx, _rx) = mpsc::channel(10);
    let mut sink = PollSender::new(tx).sink_map_err(|e| HarmonicError::SendError(e.to_string()));
    let mut server_file_path = PathBuf::new();
    let mut server_writer_tx = None;

    let payload = harmonic::proto::sync_request::Payload::FileAction(FileAction {
        path: sync_plan[0].path.clone(),
        direction: sync_plan[0].direction,
        timestamp_latest_modified: sync_plan[0].timestamp_latest_modified,
    });

    handle_sync_payload(
        payload,
        &mut sink,
        &mut server_file_path,
        server_config,
        &mut server_writer_tx,
    ).await.unwrap();

    assert!(
        !server_root.join("gone.txt").exists(),
        "server copy must be deleted"
    );
}

#[tokio::test]
async fn test_server_deletion_propagates_to_client() {
    let client_dir = tempdir().unwrap();
    let server_dir = tempdir().unwrap();
    let client_root = PathBuf::from(client_dir.path());
    let server_root = PathBuf::from(server_dir.path());

    let content = b"shared content";
    fs::write(client_root.join("gone.txt"), content).unwrap();
    fs::write(server_root.join("gone.txt"), content).unwrap();

    let client_before = generate_state(&client_root, false).unwrap();
    fs::remove_file(server_root.join("gone.txt")).unwrap();
    let client_after = generate_state(&client_root, false).unwrap();
    let server_state = generate_state(&server_root, false).unwrap();

    let status_list = build_status_list(&client_before, &client_after);
    assert_eq!(
        status_list.len(),
        1,
        "unchanged file must be reported as UNCHANGED"
    );
    assert_eq!(status_list[0].change_type, FileChangeType::Unchanged as i32);

    let sync_plan = generate_sync_plan(&server_state, &status_list).unwrap();

    assert_eq!(sync_plan.len(), 1);
    assert_eq!(sync_plan[0].path, "gone.txt");
    assert_eq!(
        sync_plan[0].direction,
        TransferDirection::Delete as i32,
        "server deletion must not resurrect the file via Upload"
    );

    // client executes the planned Delete action locally
    let client_config = common::create_test_config(&client_root);
    delete_sync_file(&from_protocol_path(&sync_plan[0].path), &client_config).await.unwrap();

    assert!(
        !client_root.join("gone.txt").exists(),
        "client copy must be deleted"
    );
}

#[tokio::test]
async fn test_deletion_of_file_missing_on_both_sides_is_noop() {
    let client_dir = tempdir().unwrap();
    let server_dir = tempdir().unwrap();
    let client_root = PathBuf::from(client_dir.path());
    let server_root = PathBuf::from(server_dir.path());

    fs::write(client_root.join("gone.txt"), b"content").unwrap();
    let client_before = generate_state(&client_root, false).unwrap();
    fs::remove_file(client_root.join("gone.txt")).unwrap();
    let client_after = generate_state(&client_root, false).unwrap();
    let server_state = generate_state(&server_root, false).unwrap();

    let status_list = build_status_list(&client_before, &client_after);
    let sync_plan = generate_sync_plan(&server_state, &status_list).unwrap();

    assert_eq!(sync_plan.len(), 1);
    assert_eq!(sync_plan[0].direction, TransferDirection::Delete as i32);

    let server_config = common::create_test_config(&server_root);
    let (tx, _rx) = mpsc::channel(10);
    let mut sink = PollSender::new(tx).sink_map_err(|e| HarmonicError::SendError(e.to_string()));
    let mut server_file_path = PathBuf::new();
    let mut server_writer_tx = None;

    let payload = harmonic::proto::sync_request::Payload::FileAction(FileAction {
        path: sync_plan[0].path.clone(),
        direction: sync_plan[0].direction,
        timestamp_latest_modified: sync_plan[0].timestamp_latest_modified,
    });

    handle_sync_payload(
        payload,
        &mut sink,
        &mut server_file_path,
        server_config,
        &mut server_writer_tx,
    ).await.unwrap();
}

#[tokio::test]
async fn test_unmodified_files_are_not_replanned() {
    let client_dir = tempdir().unwrap();
    let server_dir = tempdir().unwrap();
    let client_root = PathBuf::from(client_dir.path());
    let server_root = PathBuf::from(server_dir.path());

    fs::write(client_root.join("same.txt"), b"identical").unwrap();
    fs::write(server_root.join("same.txt"), b"identical").unwrap();

    let client_state = generate_state(&client_root, false).unwrap();
    let server_state = generate_state(&server_root, false).unwrap();

    let status_list: Vec<FileStatus> = client_state
        .tree
        .iter()
        .map(|(path, meta)| FileStatus {
            path: to_protocol_path(path),
            timestamp: Some(meta.modified_ts),
            file_type: FileType::Other.into(),
            hash: meta.hash.to_vec(),
            change_type: FileChangeType::Unchanged as i32,
        })
        .collect();

    let sync_plan = generate_sync_plan(&server_state, &status_list).unwrap();

    assert_eq!(sync_plan.len(), 1);
    assert_eq!(
        sync_plan[0].direction,
        TransferDirection::Skip as i32,
        "identical files must not be transferred"
    );
}

#[test]
fn test_unchanged_and_removed_in_one_status_list() {
    // deleted file as REMOVED with its previous metadata
    let dir = tempdir().unwrap();
    let root = PathBuf::from(dir.path());

    fs::write(root.join("keep.txt"), b"kept").unwrap();
    fs::write(root.join("gone.txt"), b"deleted").unwrap();

    let before_state = generate_state(&root, false).unwrap();
    let old_meta = before_state.tree.get(&PathBuf::from("gone.txt")).unwrap();
    let old_ts = old_meta.modified_ts;

    fs::remove_file(root.join("gone.txt")).unwrap();
    let now_state = generate_state(&root, false).unwrap();

    let status_list = build_status_list(&before_state, &now_state);

    assert_eq!(status_list.len(), 2);

    let keep = status_list.iter().find(|s| s.path == "keep.txt").unwrap();
    assert_eq!(keep.change_type, FileChangeType::Unchanged as i32);

    let gone = status_list.iter().find(|s| s.path == "gone.txt").unwrap();
    assert_eq!(gone.change_type, FileChangeType::Removed as i32);
    assert_eq!(gone.timestamp, Some(old_ts), "removed entry carries previous metadata");
}

#[test]
fn test_server_deletion_planned_for_unchanged_client_file() {
    let dir = tempdir().unwrap();
    let server_root = PathBuf::from(dir.path());

    // empty server state, file was deleted here before the sync
    let server_state = generate_state(&server_root, false).unwrap();

    let status_list = vec![unchanged_status("gone.txt", [1; 32], 1000)];

    let sync_plan = generate_sync_plan(&server_state, &status_list).unwrap();

    assert_eq!(sync_plan.len(), 1);
    assert_eq!(
        sync_plan[0].direction,
        TransferDirection::Delete as i32,
        "unchanged file missing on the server means the server deleted it"
    );
}
