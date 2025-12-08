// Harmonic Sync Protocol Tests
//
// Protocol Flow Overview:
//
// UPLOAD (Client → Server):
// 1. Client sends FileAction with Upload direction
// 2. Server receives FileAction, generates and sends Signatures back
//    (empty signatures if file doesn't exist on server)
// 3. Server creates delta_writer with block cache to receive deltas
// 4. Client receives Signatures, calculates Delta from them
// 5. Client sends Delta message(s) to server
// 6. Server's delta_writer reconstructs the file from deltas + cached blocks
// 7. Client sends Complete message when done
//
// DOWNLOAD (Server → Client):
// 1. Client sends FileAction with Download direction
// 2. Client immediately generates and sends Signatures of its local file
//    (empty signatures if file doesn't exist locally)
// 3. Client creates delta_writer with block cache to receive deltas
// 4. Server receives Signatures, calculates Delta from them
// 5. Server sends Delta message(s) to client
// 6. Client's delta_writer reconstructs the file from deltas + cached blocks
// 7. Server sends Complete message when done

use harmonic::{sync::{*}, proto::{FileStatus, sync_request, FileAction, TransferDirection, Delta}, utils::HarmonicError};
use harmonic::sync::handler::handle_sync_payload;
use std::{fs, path::PathBuf};
use tempfile::tempdir;
use futures::{SinkExt};
use tokio_util::sync::PollSender;
use tokio::sync::mpsc;

mod common;

#[test]
fn test_generate_state_with_real_files() {
    // Create a temporary directory with some test files
    let dir = tempdir().unwrap();
    let root = PathBuf::from(dir.path());

    // Create test files
    let file1 = root.join("file1.txt");
    let file2 = root.join("file2.md");

    fs::write(&file1, "This is file 1 content").unwrap();
    fs::write(&file2, "# This is file 2 content").unwrap();

    // Create a subdirectory with a file
    let subdir = root.join("subdir");
    fs::create_dir(&subdir).unwrap();
    let file3 = subdir.join("file3.txt");
    fs::write(&file3, "This is file 3 in subdirectory").unwrap();

    // Generate state
    let state = generate_state(&root, false).unwrap();

    // Verify state was created with a valid timestamp
    assert!(state.last_sync_timestamp_micros > 0);

    // Create another state from empty directory to compare
    let empty_dir = tempdir().unwrap();
    let empty_root = PathBuf::from(empty_dir.path());
    let empty_state = generate_state(&empty_root, false).unwrap();

    // Comparing empty state with our state should show 3 additions
    let diffs = compare_states(&empty_state, &state);
    assert_eq!(diffs.len(), 3);
    assert_eq!(
        diffs
            .iter()
            .filter(|d| matches!(d.change, ChangeType::Added))
            .count(),
        3
    );
}

#[test]
fn test_generate_state_empty_directory() {
    let dir = tempdir().unwrap();
    let root = PathBuf::from(dir.path());

    let state = generate_state(&root, false).unwrap();

    // Verify timestamp is set
    assert!(state.last_sync_timestamp_micros > 0);

    // Verify empty by comparing states
    let state2 = generate_state(&root, false).unwrap();
    let diffs = compare_states(&state, &state2);
    assert_eq!(diffs.len(), 0);
}

#[test]
fn test_compare_states_with_real_file_addition() {
    let dir = tempdir().unwrap();
    let root = PathBuf::from(dir.path());

    // Generate initial state
    let state1 = generate_state(&root, false).unwrap();

    // Add a new file
    let new_file = root.join("new_file.txt");
    fs::write(&new_file, "New content").unwrap();

    // Generate new state
    let state2 = generate_state(&root, false).unwrap();

    // Compare states
    let diffs = compare_states(&state1, &state2);

    assert_eq!(diffs.len(), 1);
    assert!(matches!(diffs[0].change, ChangeType::Added));

    // Verify path by converting diffs to FileStatus vec
    let file_statuses: Vec<FileStatus> =
        diffs
        .into_iter()
        .map(|d| FileStatus::try_from(d))
        .collect::<Result<Vec<FileStatus>, _>>()
        .unwrap();
    assert_eq!(file_statuses[0].path, "new_file.txt");
}

#[test]
fn test_compare_states_with_real_file_modification() {
    let dir = tempdir().unwrap();
    let root = PathBuf::from(dir.path());

    // Create initial file
    let file = root.join("modified_file.txt");
    fs::write(&file, "Original content").unwrap();

    // Generate initial state
    let state1 = generate_state(&root, false).unwrap();

    // Wait a bit to ensure timestamp changes
    std::thread::sleep(std::time::Duration::from_millis(10));

    // Modify the file
    fs::write(&file, "Modified content - different hash").unwrap();

    // Generate new state
    let state2 = generate_state(&root, false).unwrap();

    // Compare states
    let diffs = compare_states(&state1, &state2);

    assert_eq!(diffs.len(), 1);
    assert!(matches!(diffs[0].change, ChangeType::Modified));

    // Verify path by converting diffs to FileStatus vec
    let file_statuses: Vec<FileStatus> =
        diffs
        .into_iter()
        .map(|d| FileStatus::try_from(d))
        .collect::<Result<Vec<FileStatus>, _>>()
        .unwrap();
    assert_eq!(file_statuses[0].path, "modified_file.txt");
}

#[test]
fn test_compare_states_with_real_file_removal() {
    let dir = tempdir().unwrap();
    let root = PathBuf::from(dir.path());

    // Create initial file
    let file = root.join("to_be_removed.txt");
    fs::write(&file, "This will be removed").unwrap();

    // Generate initial state
    let state1 = generate_state(&root, false).unwrap();

    // Remove the file
    fs::remove_file(&file).unwrap();

    // Generate new state
    let state2 = generate_state(&root, false).unwrap();

    // Compare states
    let diffs = compare_states(&state1, &state2);

    assert_eq!(diffs.len(), 1);
    assert!(matches!(diffs[0].change, ChangeType::Removed));

    // Verify path by converting diffs to FileStatus vec
    let file_statuses: Vec<FileStatus> = diffs
        .into_iter()
        .map(|d| FileStatus::try_from(d))
        .collect::<Result<Vec<FileStatus>, _>>()
        .unwrap();
    assert_eq!(file_statuses[0].path, "to_be_removed.txt");
}

#[test]
fn test_compare_states_with_multiple_changes() {
    let dir = tempdir().unwrap();
    let root = PathBuf::from(dir.path());

    // Create initial files
    let file1 = root.join("keep_this.txt");
    let file2 = root.join("modify_this.txt");
    let file3 = root.join("remove_this.txt");

    fs::write(&file1, "Unchanged content").unwrap();
    fs::write(&file2, "Original content").unwrap();
    fs::write(&file3, "Will be removed").unwrap();

    // Generate initial state
    let state1 = generate_state(&root, false).unwrap();

    // Wait to ensure timestamp changes
    std::thread::sleep(std::time::Duration::from_millis(10));

    // Make changes
    fs::write(&file2, "Modified content!").unwrap(); // Modified
    fs::remove_file(&file3).unwrap(); // Removed
    let file4 = root.join("new_file.txt");
    fs::write(&file4, "New file added").unwrap(); // Added

    // Generate new state
    let state2 = generate_state(&root, false).unwrap();

    // Compare states
    let diffs = compare_states(&state1, &state2);

    // Should have 3 changes: 1 modified, 1 removed, 1 added
    assert_eq!(diffs.len(), 3);

    let added_count = diffs
        .iter()
        .filter(|d| matches!(d.change, ChangeType::Added))
        .count();
    let modified_count = diffs
        .iter()
        .filter(|d| matches!(d.change, ChangeType::Modified))
        .count();
    let removed_count = diffs
        .iter()
        .filter(|d| matches!(d.change, ChangeType::Removed))
        .count();

    assert_eq!(added_count, 1);
    assert_eq!(modified_count, 1);
    assert_eq!(removed_count, 1);
}

#[tokio::test]
async fn test_full_sync_new_file() {
    // Setup: Simulate server receiving a new file from client via Upload
    let dir = tempdir().unwrap();
    let root_path = PathBuf::from(dir.path());
    let file_path = root_path.join("new_file.txt");
    let config = common::create_test_config(&root_path);

    // Content that client will send
    let content = "Hello, World!".as_bytes().to_vec();

    // Server side: Prepare to receive
    let (tx, mut rx) = mpsc::channel(10);
    let mut sink = PollSender::new(tx).sink_map_err(|e| HarmonicError::SendError(e.to_string()));
    let mut server_file_path = PathBuf::new();
    let mut writer_tx = None;

    // Step 1: Server receives FileAction with Upload direction
    // This means: client wants to upload a file to server
    // Server responds with Signatures (empty if file doesn't exist on server)
    let payload = sync_request::Payload::FileAction(FileAction {
        path: "new_file.txt".to_string(),
        direction: TransferDirection::Upload as i32,
        timestamp_latest_modified: Default::default(),
    });

    handle_sync_payload(
        payload,
        &mut sink,
        &mut server_file_path,
        config.clone(),
        &mut writer_tx,
    ).await.unwrap();

    // Verify server set path and sent signatures
    assert_eq!(server_file_path, PathBuf::from("new_file.txt"));
    let response = rx.recv().await.unwrap();
    match response.payload.unwrap() {
        sync_request::Payload::Signatures(sigs) => {
            assert!(sigs.blocks.is_empty()); // File doesn't exist on server yet
        }
        _ => panic!("Expected Signatures"),
    }

    // Step 2: Client calculates Delta from signatures and sends it
    // Since file is new (empty signatures), client sends entire content as literal
    let payload = sync_request::Payload::Delta(Delta {
        index: 0,
        instruction: Some(harmonic::proto::delta::Instruction::Literal(content.clone())),
    });

    handle_sync_payload(
        payload,
        &mut sink,
        &mut server_file_path,
        config.clone(),
        &mut writer_tx,
    ).await.unwrap();

    // Close the writer channel to flush writes
    drop(writer_tx);

    // Wait for async write to complete
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    assert!(file_path.exists());
    let written_content = fs::read(&file_path).unwrap();
    assert_eq!(written_content, content);
}

#[tokio::test]
async fn test_delta_sync_modified_file() {
    // Setup: Simulate server receiving a modified file from client via Upload
    // Server already has an old version of the file
    let dir = tempdir().unwrap();
    let root_path = PathBuf::from(dir.path());
    let file_path = root_path.join("modified.txt");
    let config = common::create_test_config(&root_path);

    // Create initial file on server
    let initial_content = "Hello World".as_bytes();
    fs::write(&file_path, initial_content).unwrap();

    // Server side setup
    let (tx, mut rx) = mpsc::channel(10);
    let mut sink = PollSender::new(tx).sink_map_err(|e| HarmonicError::SendError(e.to_string()));
    let mut server_file_path = PathBuf::new();
    let mut writer_tx = None;

    // Step 1: Server receives FileAction with Upload direction
    // Client wants to upload modified file to server
    // Server sends back signatures of its current file version
    let payload = sync_request::Payload::FileAction(FileAction {
        path: "modified.txt".to_string(),
        direction: TransferDirection::Upload as i32,
        timestamp_latest_modified: Default::default(),
    });

    handle_sync_payload(
        payload,
        &mut sink,
        &mut server_file_path,
        config.clone(),
        &mut writer_tx,
    ).await.unwrap();

    // Verify server sent signatures for existing file
    let response = rx.recv().await.unwrap();
    let signatures = match response.payload.unwrap() {
        sync_request::Payload::Signatures(sigs) => sigs,
        _ => panic!("Expected Signatures"),
    };
    assert!(!signatures.blocks.is_empty()); // File exists on server
    assert!(writer_tx.is_some()); // delta_writer created with block cache

    // Step 2: Client receives signatures and calculates delta
    // For this test, we simulate client sending new content as literal
    // In reality, client would use send_delta_from_block_signatures to calculate
    // optimal delta (reusing matching blocks, sending literals for differences)
    // Since block size (8192) > file size (11 bytes), entire file is 1 block
    let new_content = "Hello Rust World".as_bytes().to_vec();
    let payload = sync_request::Payload::Delta(Delta {
        index: 0,
        instruction: Some(harmonic::proto::delta::Instruction::Literal(new_content.clone())),
    });

    handle_sync_payload(
        payload,
        &mut sink,
        &mut server_file_path,
        config.clone(),
        &mut writer_tx,
    ).await.unwrap();

    // Close the writer channel to flush writes
    drop(writer_tx);

    // Wait for async write to complete
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    let written_content = fs::read(&file_path).unwrap();
    assert_eq!(written_content, new_content);
}

#[tokio::test]
async fn test_download_new_file() {
    // Setup: Simulate client downloading a new file from server
    // Server has the file, client doesn't
    let dir = tempdir().unwrap();
    let root_path = PathBuf::from(dir.path());
    let server_file_path = root_path.join("download_new.txt");
    let config = common::create_test_config(&root_path);

    // Create file on server
    let server_content = "Content from server".as_bytes();
    fs::write(&server_file_path, server_content).unwrap();

    // Server side setup
    let (tx, mut rx) = mpsc::channel(10);
    let mut sink = PollSender::new(tx).sink_map_err(|e| HarmonicError::SendError(e.to_string()));
    let mut file_path = PathBuf::new();
    let mut writer_tx = None;

    // Step 1: Server receives FileAction with Download direction
    // This means: client wants to download a file from server
    // Server just sets the file path (doesn't send signatures)
    let payload = sync_request::Payload::FileAction(FileAction {
        path: "download_new.txt".to_string(),
        direction: TransferDirection::Download as i32,
        timestamp_latest_modified: Default::default(),
    });

    handle_sync_payload(
        payload,
        &mut sink,
        &mut file_path,
        config.clone(),
        &mut writer_tx,
    ).await.unwrap();

    // Verify server set path but didn't send anything (no signatures for Download)
    assert_eq!(file_path, PathBuf::from("download_new.txt"));
    assert!(writer_tx.is_none()); // No writer created for Download

    // Step 2: Client sends Signatures (empty since file doesn't exist on client)
    // The handler will convert the relative file_path to absolute internally
    let payload = sync_request::Payload::Signatures(harmonic::proto::BlockSignatures {
        block_size: config.block_size,
        blocks: vec![],
    });

    handle_sync_payload(
        payload,
        &mut sink,
        &mut file_path,
        config.clone(),
        &mut writer_tx,
    ).await.unwrap();

    // Step 3: Server should have sent deltas
    // Collect all delta messages
    let mut deltas = vec![];
    while let Ok(response) = rx.try_recv() {
        if let Some(sync_request::Payload::Delta(delta)) = response.payload {
            deltas.push(delta);
        } else if let Some(sync_request::Payload::Complete(_)) = response.payload {
            break;
        }
    }

    // Verify we got at least one delta with the full content
    assert!(!deltas.is_empty());

    // Since file is new, should be a literal with full content
    let first_delta = &deltas[0];
    if let Some(harmonic::proto::delta::Instruction::Literal(content)) = &first_delta.instruction {
        assert_eq!(content, server_content);
    } else {
        panic!("Expected Literal instruction for new file");
    }
}

#[tokio::test]
async fn test_download_modified_file() {
    // Setup: Simulate client downloading a modified file from server
    // Both have the file but server's version is newer
    let dir = tempdir().unwrap();
    let root_path = PathBuf::from(dir.path());
    let server_file_path = root_path.join("download_modified.txt");
    let config = common::create_test_config(&root_path);

    // Create file on server with new content
    let server_content = "Updated content from server".as_bytes();
    fs::write(&server_file_path, server_content).unwrap();

    // Server side setup
    let (tx, mut rx) = mpsc::channel(10);
    let mut sink = PollSender::new(tx).sink_map_err(|e| HarmonicError::SendError(e.to_string()));
    let mut file_path = PathBuf::new();
    let mut writer_tx = None;

    // Step 1: Server receives FileAction with Download direction
    let payload = sync_request::Payload::FileAction(FileAction {
        path: "download_modified.txt".to_string(),
        direction: TransferDirection::Download as i32,
        timestamp_latest_modified: Default::default(),
    });

    handle_sync_payload(
        payload,
        &mut sink,
        &mut file_path,
        config.clone(),
        &mut writer_tx,
    ).await.unwrap();

    assert_eq!(file_path, PathBuf::from("download_modified.txt"));

    // Step 2: Client sends Signatures of its old version
    // For this test, we'll send empty signatures to simulate client doesn't have it
    // The handler will convert the relative file_path to absolute internally
    let payload = sync_request::Payload::Signatures(harmonic::proto::BlockSignatures {
        block_size: config.block_size,
        blocks: vec![],
    });

    handle_sync_payload(
        payload,
        &mut sink,
        &mut file_path,
        config.clone(),
        &mut writer_tx,
    ).await.unwrap();

    // Step 3: Verify server sent deltas
    let mut received_complete = false;
    while let Ok(response) = rx.try_recv() {
        if let Some(sync_request::Payload::Complete(_)) = response.payload {
            received_complete = true;
            break;
        }
    }

    assert!(received_complete, "Server should send Complete message");
}

#[tokio::test]
async fn test_delta_sync_with_block_reuse() {
    // Setup: Test that delta sync actually reuses matching blocks
    // This requires a larger file with partial modifications
    let dir = tempdir().unwrap();
    let root_path = PathBuf::from(dir.path());
    let file_path = root_path.join("block_reuse.txt");

    // Use a smaller block size for this test to ensure multiple blocks
    let mut config = common::create_test_config(&root_path);
    config.block_size = 16; // Small block size for testing

    // Create initial file on server with content that spans multiple blocks
    // Block 0: "Block 0 content!"  (16 bytes)
    // Block 1: "Block 1 content!"  (16 bytes)
    // Block 2: "Block 2 content!"  (16 bytes)
    let initial_content = "Block 0 content!Block 1 content!Block 2 content!";
    fs::write(&file_path, initial_content).unwrap();

    // Server side setup
    let (tx, mut rx) = mpsc::channel(10);
    let mut sink = PollSender::new(tx).sink_map_err(|e| HarmonicError::SendError(e.to_string()));
    let mut server_file_path = PathBuf::new();
    let mut writer_tx = None;

    // Step 1: Server receives FileAction with Upload direction
    let payload = sync_request::Payload::FileAction(FileAction {
        path: "block_reuse.txt".to_string(),
        direction: TransferDirection::Upload as i32,
        timestamp_latest_modified: Default::default(),
    });

    handle_sync_payload(
        payload,
        &mut sink,
        &mut server_file_path,
        config.clone(),
        &mut writer_tx,
    ).await.unwrap();

    // Verify server sent signatures
    let response = rx.recv().await.unwrap();
    let signatures = match response.payload.unwrap() {
        sync_request::Payload::Signatures(sigs) => sigs,
        _ => panic!("Expected Signatures"),
    };

    // Should have 3 blocks (48 bytes / 16 bytes per block)
    assert_eq!(signatures.blocks.len(), 3);
    assert!(writer_tx.is_some());

    // Step 2: Simulate client sending delta with block reuse
    // Client will reuse block 0 and 2, but send new literal for block 1
    // Delta 0: Reuse block 0
    let delta_0 = sync_request::Payload::Delta(Delta {
        index: 0,
        instruction: Some(harmonic::proto::delta::Instruction::BlockIndex(0)),
    });

    handle_sync_payload(
        delta_0,
        &mut sink,
        &mut server_file_path,
        config.clone(),
        &mut writer_tx,
    ).await.unwrap();

    // Delta 1: New content for middle block
    let new_middle_content = "MODIFIED CONTENT".as_bytes().to_vec();
    let delta_1 = sync_request::Payload::Delta(Delta {
        index: 16,
        instruction: Some(harmonic::proto::delta::Instruction::Literal(new_middle_content.clone())),
    });

    handle_sync_payload(
        delta_1,
        &mut sink,
        &mut server_file_path,
        config.clone(),
        &mut writer_tx,
    ).await.unwrap();

    // Delta 2: Reuse block 2
    let delta_2 = sync_request::Payload::Delta(Delta {
        index: 32,
        instruction: Some(harmonic::proto::delta::Instruction::BlockIndex(2)),
    });

    handle_sync_payload(
        delta_2,
        &mut sink,
        &mut server_file_path,
        config.clone(),
        &mut writer_tx,
    ).await.unwrap();

    // Close writer and wait for flush
    drop(writer_tx);
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // Verify reconstructed file
    let written_content = fs::read_to_string(&file_path).unwrap();
    let expected = "Block 0 content!MODIFIED CONTENTBlock 2 content!";
    assert_eq!(written_content, expected);
}

#[tokio::test]
async fn test_complete_message_handling() {
    // Setup: Test that Complete message returns proper status
    let dir = tempdir().unwrap();
    let root_path = PathBuf::from(dir.path());
    let config = common::create_test_config(&root_path);

    let (tx, _rx) = mpsc::channel(10);
    let mut sink = PollSender::new(tx).sink_map_err(|e| HarmonicError::SendError(e.to_string()));
    let mut file_path = PathBuf::new();
    let mut writer_tx = None;

    // Send Complete message
    let payload = sync_request::Payload::Complete(true);

    let status = handle_sync_payload(
        payload,
        &mut sink,
        &mut file_path,
        config.clone(),
        &mut writer_tx,
    ).await.unwrap();

    // Verify it returns Completed status
    assert!(matches!(status, harmonic::sync::handler::SyncStatus::Completed));
}

#[tokio::test]
async fn test_large_file_multiple_blocks() {
    // Setup: Test syncing a file larger than block size with actual content
    let dir = tempdir().unwrap();
    let root_path = PathBuf::from(dir.path());
    let file_path = root_path.join("large_file.bin");

    let mut config = common::create_test_config(&root_path);
    config.block_size = 1024; // 1KB blocks

    // Create a 3KB file (3 blocks)
    let block1 = vec![1u8; 1024];
    let block2 = vec![2u8; 1024];
    let block3 = vec![3u8; 1024];
    let mut content = Vec::new();
    content.extend_from_slice(&block1);
    content.extend_from_slice(&block2);
    content.extend_from_slice(&block3);

    // Initially don't write to disk (simulating new file)

    // Server side setup
    let (tx, mut rx) = mpsc::channel(10);
    let mut sink = PollSender::new(tx).sink_map_err(|e| HarmonicError::SendError(e.to_string()));
    let mut server_file_path = PathBuf::new();
    let mut writer_tx = None;

    // Step 1: Server receives FileAction
    let payload = sync_request::Payload::FileAction(FileAction {
        path: "large_file.bin".to_string(),
        direction: TransferDirection::Upload as i32,
        timestamp_latest_modified: Default::default(),
    });

    handle_sync_payload(
        payload,
        &mut sink,
        &mut server_file_path,
        config.clone(),
        &mut writer_tx,
    ).await.unwrap();

    // Get signatures (should be empty)
    let response = rx.recv().await.unwrap();
    match response.payload.unwrap() {
        sync_request::Payload::Signatures(sigs) => {
            assert!(sigs.blocks.is_empty());
        }
        _ => panic!("Expected Signatures"),
    }

    // Step 2: Send the entire file as one large literal
    let payload = sync_request::Payload::Delta(Delta {
        index: 0,
        instruction: Some(harmonic::proto::delta::Instruction::Literal(content.clone())),
    });

    handle_sync_payload(
        payload,
        &mut sink,
        &mut server_file_path,
        config.clone(),
        &mut writer_tx,
    ).await.unwrap();

    // Close writer and wait
    drop(writer_tx);
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // Verify file was written correctly
    assert!(file_path.exists());
    let written_content = fs::read(&file_path).unwrap();
    assert_eq!(written_content.len(), 3072);
    assert_eq!(&written_content[0..1024], &block1[..]);
    assert_eq!(&written_content[1024..2048], &block2[..]);
    assert_eq!(&written_content[2048..3072], &block3[..]);
}


