// Server session lifecycle tests
//
// A sync run transfers its files over separate bidirectional streams that all
// share one session, so a session must stay alive until its LAST stream
// completes and must not be evicted by the first one. One stream in this test
// is held open with a request body that only ends when its frame sender is
// dropped, making the in-flight accounting deterministic

use harmonic::proto::{
    harmonic_server::Harmonic, ClientSyncState, ServerSyncStateResponse, SyncRequest,
};
use harmonic::server::HarmonicService;
use harmonic::sync::Config;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::tempdir;
use tokio::sync::{mpsc, Mutex};
use tokio_stream::wrappers::ReceiverStream;
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

fn stream_request(
    sync_uuid: &str,
    body: tonic::body::Body,
) -> Request<tonic::codec::Streaming<SyncRequest>> {
    let streaming = tonic::codec::Streaming::new_request(
        ProstDecoder::<SyncRequest>::default(),
        body,
        None,
        None,
    );

    let mut request = Request::new(streaming);
    request.metadata_mut().insert("session-uuid", sync_uuid.parse().unwrap());
    request
}

fn empty_request(sync_uuid: &str) -> Request<tonic::codec::Streaming<SyncRequest>> {
    stream_request(sync_uuid, tonic::body::Body::empty())
}

// a request stream that stays open until the returned sender is dropped
type OpenEndedStream = (
    mpsc::Sender<Result<http_body::Frame<bytes::Bytes>, Status>>,
    Request<tonic::codec::Streaming<SyncRequest>>,
);

fn open_ended_request(sync_uuid: &str) -> OpenEndedStream {
    let (tx, rx) = mpsc::channel(1);
    let body = http_body_util::StreamBody::new(ReceiverStream::new(rx));

    (tx, stream_request(sync_uuid, tonic::body::Body::new(body)))
}

#[tokio::test]
async fn test_session_remains_valid_until_all_transfer_streams_complete() {
    let dir = tempdir().unwrap();
    let service = test_service(common::create_test_config(&PathBuf::from(dir.path())));

    let sync_uuid = uuid::Uuid::new_v4().to_string();

    // unknown session is rejected
    let before = service.harmonize_sync_request(empty_request(&sync_uuid)).await;
    assert_eq!(before.unwrap_err().code(), Code::NotFound);

    // initiating a sync registers the session
    let initiate: Result<tonic::Response<ServerSyncStateResponse>, Status> = service
        .harmonize_client_initiate_sync(initiate_request(&sync_uuid))
        .await;
    assert!(initiate.is_ok(), "initiate should register the session");

    // a sync run transfers its files over separate streams sharing one session
    let first = service.harmonize_sync_request(empty_request(&sync_uuid)).await.unwrap();
    let (second_tx, second) = open_ended_request(&sync_uuid);
    let second = service.harmonize_sync_request(second).await.unwrap();

    let active_streams = || async {
        service
            .sync_sessions
            .lock()
            .await
            .values()
            .map(|s| s.active_streams)
            .sum::<usize>()
    };

    assert_eq!(active_streams().await, 2, "both streams must be counted");

    // the first stream completing must not evict the session for the others
    drain(first.into_inner()).await;
    assert_eq!(
        active_streams().await,
        1,
        "session must survive with the remaining stream in flight"
    );

    let third = service.harmonize_sync_request(empty_request(&sync_uuid)).await;
    assert!(
        third.is_ok(),
        "session must accept new streams until every stream completed"
    );

    drain(third.unwrap().into_inner()).await;

    // releasing the last in flight stream releases the session
    drop(second_tx);
    drain(second.into_inner()).await;

    // once every stream has completed the session is released
    let after = service.harmonize_sync_request(empty_request(&sync_uuid)).await;
    assert_eq!(
        after.unwrap_err().code(),
        Code::NotFound,
        "session must be removed after its last transfer stream completed"
    );
    assert!(
        service.sync_sessions.lock().await.is_empty(),
        "session entry must be gone"
    );
}

async fn drain(mut stream: ReceiverStream<Result<SyncRequest, Status>>) {
    while let Some(message) = stream.next().await {
        message.expect("stream should complete without errors");
    }
}

#[tokio::test]
async fn test_invalid_session_uuid_is_rejected() {
    let dir = tempdir().unwrap();
    let service = test_service(common::create_test_config(&PathBuf::from(dir.path())));

    let response = service.harmonize_sync_request(empty_request("not-a-uuid")).await;
    assert_eq!(response.unwrap_err().code(), Code::InvalidArgument);
}
