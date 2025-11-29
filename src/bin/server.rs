use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::prelude::*;

use futures::{StreamExt};
use harmonic::Config;
use harmonic::proto::Delta;
use harmonic::proto::SyncRequest;
use harmonic::proto::sync_request;
use harmonic::sync::transfer::send_block_signatures_for_file;
use harmonic::sync::transfer::send_delta_from_block_signatures;
use harmonic::utils::HarmonicError;
use harmonic::utils::tracing::*;
use harmonic::utils::writer::delta_writer;
use tokio::sync::Mutex;
use tokio::sync::mpsc::{self, Sender};
use tokio::task::JoinHandle;
use tokio_stream::wrappers::ReceiverStream;
#[cfg(feature = "compression-zstd")]
use tonic::codec::CompressionEncoding;
use tower::ServiceBuilder;
use tower_http::trace::TraceLayer;
use tracing::Instrument;
use tracing::Span;
use tracing::{debug, error, info, instrument};

use tonic::{Request, Response, Status, Streaming, transport::Server};
use uuid::Uuid;

use harmonic::proto::{
    ClientSyncState, FileAction, FileStatus, ServerSyncStateResponse, TransferDirection,
    harmonic_server::{Harmonic, HarmonicServer},
};
use harmonic::sync::{self, SyncState};

#[derive(Clone, Debug)]
struct SessionData {
    // allow for resume in future
    timestamp_micros: i64,
    local_state: SyncState,
    sync_plan: Vec<FileAction>,
}

#[derive(Debug)]
pub struct HarmonicService {
    sync_sessions: Arc<Mutex<HashMap<Uuid, SessionData>>>,
    config: sync::Config,
}

#[tonic::async_trait]
impl Harmonic for HarmonicService {
    type HarmonizeSyncRequestStream = ReceiverStream<Result<SyncRequest, Status>>;

    #[instrument]
    async fn harmonize_client_initiate_sync(
        &self,
        request: Request<ClientSyncState>,
    ) -> Result<Response<ServerSyncStateResponse>, Status> {
        info!("Received request {:?}", request);

        info!("Parsing request");
        let request_message = request.into_inner();
        let sync_uuid = request_message.sync_uuid;
        let request_timestamp =
            DateTime::from_timestamp_micros(request_message.timestamp_last_sync_micro)
                .ok_or_else(|| Status::invalid_argument("Could not parse datetime timestamp"))?;
        info!("Got time {:?} from timestamp", request_timestamp);
        let files_list: Vec<FileStatus> = request_message.status_list;

        let state_now = sync::generate_state(&self.config.sync_path)
            .map_err(|e| Status::internal(format!("Unable to generate current state: {}", e)))?;

        let sync_plan = sync::generate_sync_plan(&state_now, &files_list)
            .map_err(|e| Status::internal(format!("Unable to generate sync plan: {}", e)))?;

        self.sync_sessions.lock().await.insert(
            Uuid::from_str(&sync_uuid).map_err(|e| {
                Status::invalid_argument(format!(
                    "Did not receive valid uuid from client. Conversion failed: {}",
                    e
                ))
            })?,
            SessionData {
                timestamp_micros: Utc::now().timestamp_micros(),
                local_state: state_now,
                sync_plan: sync_plan.clone(),
            },
        );

        let response_strategy = ServerSyncStateResponse {
            sync_uuid: sync_uuid,
            timestamp_micro: Utc::now().timestamp_micros(),
            sync_plan: sync_plan,
        };

        Ok(Response::new(response_strategy))
    }

    #[instrument]
    async fn harmonize_sync_request(
        &self,
        request: Request<Streaming<SyncRequest>>,
    ) -> Result<Response<Self::HarmonizeSyncRequestStream>, Status> {
        info!("Responding to sync request stream");
        let session_uuid = request
            .metadata()
            .get("session-uuid")
            .and_then(|m| m.to_str().ok())
            .and_then(|s| Uuid::from_str(s).ok())
            .ok_or_else(|| Status::invalid_argument("Missing session uuid"))?;

        // check session
        self.sync_sessions
            .lock()
            .await
            .get(&session_uuid)
            .ok_or_else(|| Status::not_found("Session not found"))?;

        let request_stream = request.into_inner();
        let (tx, rx) = mpsc::channel::<Result<SyncRequest, Status>>(10);

        handle_sync_request_stream(request_stream, tx, self.config.clone())
            .await
            .map_err(|e| {
                error!("Failed to handle sync request stream: {:?}", e);
                Status::internal(format!("Failed to handle sync request stream: {}", e))
            })?;

        Ok(Response::new(ReceiverStream::new(rx)))
    }
}

async fn handle_sync_request_stream(
    mut stream: Streaming<SyncRequest>,
    tx: Sender<Result<SyncRequest, Status>>,
    config: Config,
) -> Result<JoinHandle<Result<()>>> {
    let handle = tokio::spawn(
        async move {
            let mut file_path: PathBuf = Default::default();
            let mut writer_tx: Option<Sender<Delta>> = Default::default();

            while let Some(sync_request_result) = stream.next().await {
                match sync_request_result {
                    Ok(sync_request) => {
                        let Some(payload) = sync_request.payload else {
                            debug!("Received SyncRequest with no payload, skipping");
                            continue;
                        };

                        if let Err(e) = route_sync_request(
                            payload,
                            tx.clone(),
                            &mut file_path,
                            config.clone(),
                            &mut writer_tx,
                        ).await {
                            error!("Error routing sync request: {:?}", e);
                            return Err(anyhow::anyhow!("Failed to route sync request: {}", e));
                        }
                    }
                    Err(status_err) => {
                        error!("Error receiving sync request from stream: {:?}", status_err);
                        return Err(anyhow::anyhow!("Stream error: {}", status_err));
                    }
                }
            }

            info!("Sync request stream completed successfully");
            Ok(())
        }
        .instrument(Span::current()),
    );

    Ok(handle)
}

async fn route_sync_request(
    payload: sync_request::Payload,
    tx: Sender<Result<SyncRequest, Status>>,
    file_path: &mut PathBuf,
    config: Config,
    writer_tx: &mut Option<Sender<Delta>>,
) -> Result<()> {
    match payload {
        sync_request::Payload::FileAction(file_action) => {
            match file_action.direction {
                d if d == TransferDirection::ServerRequestFile as i32 => {
                    *file_path = PathBuf::from(file_action.path);
                }
                d if d == TransferDirection::ClientSendFile as i32 => {
                    let cache = send_block_signatures_for_file(file_path, tx, &config).await?;
                    *writer_tx = Some(delta_writer(&file_path, cache).await)
                }
                _ => {}
            }
        },
        sync_request::Payload::Signatures(signatures) => {
            send_delta_from_block_signatures(file_path, signatures, tx).await?;
        },
        sync_request::Payload::Delta(delta) => {
            if let Some(tx) = writer_tx {
                tx.send(delta).await.map_err(|e| HarmonicError::SendError(e.to_string()))?;
            }
        },
        sync_request::Payload::Complete(_complete) => {
            return Ok(())
        },
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    info!("Starting server");
    let config = sync::load_config().context("Failed to load config")?;

    tracing_orchestrator(&config.log_level);

    debug!("Address from config: {:?}", config.socket_addr);
    let address: SocketAddr = config
        .socket_addr
        .parse()
        .context("Somehow could not parse address..?")?;
    let harmonic = HarmonicService {
        sync_sessions: Arc::new(Mutex::new(HashMap::new())),
        config,
    };

    let service = HarmonicServer::new(harmonic);

    #[cfg(feature = "compression-zstd")]
    let service = service
    .send_compressed(CompressionEncoding::Zstd)
    .accept_compressed(CompressionEncoding::Zstd);


    let app = Server::builder()
        .layer(
            ServiceBuilder::new()
                .layer(TraceLayer::new_for_grpc().make_span_with(make_span))
                .map_request(accept_trace),
        )
        .add_service(service)
        .serve(address);

    info!("Server started");
    app.await?;

    Ok(())
}
