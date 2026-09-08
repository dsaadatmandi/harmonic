// Integration test to verify actual file transfer works correctly
// This test simulates the full client->server upload flow

use harmonic::sync::*;
use harmonic::proto::{sync_request, TransferDirection};
use harmonic::sync::handler::handle_sync_payload;
use harmonic::utils::HarmonicError;
use std::fs;
use std::path::PathBuf;
use tempfile::tempdir;
use futures::SinkExt;
use tokio_util::sync::PollSender;
use tokio::sync::mpsc;

mod common;

#[tokio::test]
async fn test_client_upload_full_flow() {
    // This test simulates the full flow of a client uploading a new file to the server

    // Setup client and server directories
    let client_dir = tempdir().unwrap();
    let server_dir = tempdir().unwrap();
    let client_root = PathBuf::from(client_dir.path());
    let server_root = PathBuf::from(server_dir.path());

    // Client has a file
    let test_content = b"Hello from client!";
    fs::write(client_root.join("test.txt"), test_content).unwrap();

    // Server doesn't have the file yet
    assert!(!server_root.join("test.txt").exists());

    // Generate states
    let client_state = generate_state(&client_root, false).unwrap();
    let server_state = generate_state(&server_root, false).unwrap();

    // Convert client state to FileStatus list (what server receives)
    let client_files: Vec<harmonic::proto::FileStatus> = client_state.tree.iter().map(|(path, meta)| {
        harmonic::proto::FileStatus {
            path: path.to_str().unwrap().to_string(),
            hash: meta.hash.to_vec(),
            timestamp: Some(meta.modified_ts),
            file_type: harmonic::proto::FileType::Other as i32,
            change_type: harmonic::proto::FileChangeType::Added as i32,
        }
    }).collect();

    // Server generates sync plan
    let sync_plan = generate_sync_plan(&server_state, &client_files).unwrap();

    assert_eq!(sync_plan.len(), 1);
    let action = &sync_plan[0];
    println!("Sync plan action: path={}, direction={}", action.path, action.direction);

    // Verify direction is Upload (Client → Server)
    assert_eq!(action.direction, TransferDirection::Upload as i32);

    // Now simulate the server receiving this FileAction
    let server_config = common::create_test_config(&server_root);
    let (server_tx, mut server_rx) = mpsc::channel(100);
    let mut server_sink = PollSender::new(server_tx).sink_map_err(|e| HarmonicError::SendError(e.to_string()));
    let mut server_file_path = PathBuf::new();
    let mut server_writer_tx = None;

    // Server receives FileAction
    let payload = sync_request::Payload::FileAction(action.clone());
    handle_sync_payload(
        payload,
        &mut server_sink,
        &mut server_file_path,
        server_config.clone(),
        &mut server_writer_tx,
    ).await.unwrap();

    // Server should have sent Signatures
    let signatures_msg = server_rx.recv().await.expect("Server should send signatures");
    let signatures = match signatures_msg.payload.unwrap() {
        sync_request::Payload::Signatures(sigs) => sigs,
        _ => panic!("Expected Signatures"),
    };

    println!("Server sent signatures with {} blocks", signatures.blocks.len());
    assert_eq!(signatures.blocks.len(), 0, "Server should send empty signatures for new file");

    // Server should have created a writer
    assert!(server_writer_tx.is_some(), "Server should create delta writer");

    // Now simulate client receiving these signatures and sending deltas
    let client_config = common::create_test_config(&client_root);
    let (client_tx, mut client_rx) = mpsc::channel(100);
    let mut client_sink = PollSender::new(client_tx).sink_map_err(|e| HarmonicError::SendError(e.to_string()));
    let mut client_file_path = PathBuf::from("test.txt");
    let mut client_writer_tx = None;

    // Client processes the signatures
    let sig_payload = sync_request::Payload::Signatures(signatures);
    handle_sync_payload(
        sig_payload,
        &mut client_sink,
        &mut client_file_path,
        client_config.clone(),
        &mut client_writer_tx,
    ).await.unwrap();

    // Client should have sent deltas
    let mut delta_count = 0;
    let mut received_complete = false;

    while let Ok(msg) = client_rx.try_recv() {
        match msg.payload.unwrap() {
            sync_request::Payload::Delta(_) => {
                delta_count += 1;
                println!("Received delta #{}", delta_count);
            }
            sync_request::Payload::Complete(_) => {
                received_complete = true;
                println!("Received Complete message");
            }
            _ => {}
        }
    }

    assert!(delta_count > 0, "Client should send at least one delta");
    assert!(received_complete, "Client should send Complete message");

    println!("Client sent {} deltas and Complete message", delta_count);

    // Verify client's original file is still intact
    let client_file_content = fs::read(client_root.join("test.txt")).unwrap();
    assert_eq!(client_file_content, test_content, "Client's original file should not be modified");
}
