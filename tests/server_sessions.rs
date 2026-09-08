// Server session lifecycle tests
//
// A session is created when a client initiates a sync and must be released
// once its transfer stream completes, otherwise the server session map grows
// without bound. The checks are behavioral: an unknown session is rejected
// with NotFound, a known session is accepted, and after its stream completed
// the same session is rejected again

use harmonic::proto::{
    harmonic_server::Harmonic, ClientSyncState, ServerSyncStateResponse, SyncRequest,
};
use harmonic::server::HarmonicService;
use harmonic::sync::Config;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::tempdir;
use tokio::sync::Mutex;
use tokio_stream::StreamExt;
use tonic::{Code, Request, Status};
use tonic_prost::ProstDecoder;

mod common;

fn test_service(config: Config) -> HarmonicService {
    HarmonicService {
        sync_sessions: Arc::new(Mutex::new(HashMap::new())),
        config,
    }
}

fn initiate_request(sync_uuid: &str) -> Request<ClientSyncState> {
    Request::new(ClientSyncState {
        sync_uuid: sync_uuid.to_string(),
        timestamp_last_sync_micro: 1000,
        status_list: vec![],
    })
}

fn stream_request(sync_uuid: &str) -> Request<tonic::codec::Streaming<SyncRequest>> {
    let streaming = tonic::codec::Streaming::new_request(
        ProstDecoder::<SyncRequest>::default(),
        tonic::body::Body::empty(),
        None,
        None,
    );

    let mut request = Request::new(streaming);
    request.metadata_mut().insert("session-uuid", sync_uuid.parse().unwrap());
    request
}

#[tokio::test]
async fn test_session_is_released_after_transfer_stream_completes() {
    let dir = tempdir().unwrap();
    let service = test_service(common::create_test_config(&PathBuf::from(dir.path())));

    let sync_uuid = uuid::Uuid::new_v4().to_string();

    // unknown session is rejected
    let before = service.harmonize_sync_request(stream_request(&sync_uuid)).await;
    assert_eq!(before.unwrap_err().code(), Code::NotFound);

    // initiating a sync registers the session
    let initiate: Result<tonic::Response<ServerSyncStateResponse>, Status> = service
        .harmonize_client_initiate_sync(initiate_request(&sync_uuid))
        .await;
    assert!(initiate.is_ok(), "initiate should register the session");

    // the registered session is accepted and its stream runs to completion
    let stream = service.harmonize_sync_request(stream_request(&sync_uuid)).await;
    assert!(stream.is_ok(), "registered session must be accepted");

    let mut response_stream = stream.unwrap().into_inner();
    while let Some(message) = response_stream.next().await {
        message.expect("empty stream should complete without errors");
    }

    // the session must be released once the transfer stream completed
    let after = service.harmonize_sync_request(stream_request(&sync_uuid)).await;
    assert_eq!(
        after.unwrap_err().code(),
        Code::NotFound,
        "session must be removed after its transfer stream completed"
    );
}

#[tokio::test]
async fn test_invalid_session_uuid_is_rejected() {
    let dir = tempdir().unwrap();
    let service = test_service(common::create_test_config(&PathBuf::from(dir.path())));

    let response = service.harmonize_sync_request(stream_request("not-a-uuid")).await;
    assert_eq!(response.unwrap_err().code(), Code::InvalidArgument);
}
