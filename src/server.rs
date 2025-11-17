use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use chrono::prelude::*;

use tokio::sync::Mutex;
use futures::{pin_mut, StreamExt};
use tokio::fs::File;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use tonic::{Request, Response, Status, Streaming, transport::Server};
use log::{debug, error, info};
use uuid::Uuid;

use harmonic::common::{self, SyncState};
use harmonic::harmonic::{
    ClientSyncState, ServerSyncStateResponse, FileAction, FileSync,
    TransferDirection, FileStatus,
    harmonic_server::{Harmonic, HarmonicServer},
};


#[derive(Clone, Debug)]
struct SessionData {
    timestamp_micros: i64,
    local_state: SyncState,
    sync_plan: Vec<FileAction>,
}

#[derive(Default, Debug)]
pub struct HarmonicService {
    sync_sessions: Arc<Mutex<HashMap<Uuid, SessionData>>>
}

#[tonic::async_trait]
impl Harmonic for HarmonicService {
    type HarmonizeSynchronizeStateStream = ReceiverStream<Result<FileSync, Status>>;

    async fn harmonize_client_initiate_sync(
        &self,
        request: Request<ClientSyncState>) -> Result<Response<ServerSyncStateResponse>, Status> {
            info!("Received request {:?}", request);

            let config = common::load_config();


            info!("Parsing request");
            let request_message = request.into_inner();
            let sync_uuid = request_message.sync_uuid;
            let request_timestamp = DateTime::from_timestamp_micros(request_message.timestamp_last_sync_micro).expect("Could not parse datetime");
            info!("Got time {:?} from timestamp", request_timestamp);
            let files_list: Vec<FileStatus> = request_message.status_list;

            let state_now = common::generate_state(&config.sync_path);

            let sync_plan = common::generate_sync_plan(&state_now, &files_list);


            self.sync_sessions.lock().await.insert(common::string_to_uuid(&sync_uuid),
            SessionData {
                timestamp_micros: Utc::now().timestamp_micros(),
                local_state: state_now,
                sync_plan: sync_plan.clone()
            });

            let response_strategy = ServerSyncStateResponse {
                sync_uuid: sync_uuid,
                timestamp_micro: Utc::now().timestamp_micros(),
                sync_plan: sync_plan
            };


            Ok(Response::new(response_strategy))

    }
    async fn harmonize_synchronize_state(
        &self,
        request: Request<Streaming<FileSync>>,
    ) -> Result<Response<Self::HarmonizeSynchronizeStateStream>, Status> {
        info!("Responding to state sync stream request");
        let session_uuid = request.metadata()
            .get("session-uuid")
            .and_then(|m| m.to_str().ok())
            .and_then(|s| Uuid::from_str(s).ok())
            .ok_or_else(|| Status::invalid_argument("Missing session uuid"))?;

        let session_state = self.sync_sessions.lock().await
            .get(&session_uuid)
            .cloned()
            .ok_or_else(|| Status::not_found("Session not found"))?;

        let mut request_stream = request.into_inner();
        let (tx, rx) = mpsc::channel::<Result<FileSync, Status>>(10);

        let _receiver_task = tokio::spawn(async move {
            let mut cur_file: String = Default::default();
            let mut file_currently_writing: Option<File> = None;
            while let Some(request) = request_stream.next().await {
                match request {
                    Ok(msg) => {
                        let path = msg.path.clone();
                        info!("Received data for file {}. Writing to path...", path);

                        if file_currently_writing.is_none() || cur_file != path {
                            file_currently_writing = Some(common::get_file(&msg).await);
                            cur_file = path;
                        }

                        common::write_data_to_offset(msg, file_currently_writing.as_mut().unwrap())
                            .await;
                    }
                    Err(e) => {
                        error!("Error in response stream from server: {:?}", e);
                        break;
                    }
                }
            }
        });

        let _sender_task = tokio::spawn(async move {
            for action in session_state.sync_plan.iter()
                .filter(|a| a.direction == TransferDirection::ServerSend as i32) {
                    let path = PathBuf::from(&action.path);
                    let stream = common::file_to_chunked_file_sync(&path);
                    pin_mut!(stream);
                    while let Some(file_sync) = stream.next().await {
                        if tx.send(Ok(file_sync)).await.is_err() {
                            error!(
                                    "There was an error with send data for file {:?}",
                                    path
                                );
                            break;
                        };
                    }
                }

        });

        info!("Completed sync.");

        Ok(Response::new(ReceiverStream::new(rx)))

    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    info!("Starting server");
    let config = common::load_config();

    debug!("Address from config: {:?}", config.socket_addr);
    let address: SocketAddr = config.socket_addr
        .parse()
        .expect("Somehow could not parse address..?");
    let harmonic = HarmonicService::default();

    Server::builder()
        .add_service(HarmonicServer::new(harmonic))
        .serve(address)
        .await?;

    Ok(())
}
