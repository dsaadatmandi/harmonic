use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use futures::{SinkExt, StreamExt};
use tokio::sync::mpsc::{self, Sender};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::PollSender;
use tonic::async_trait;
use tonic::{Request, Response, Status, Streaming};
use tracing::{debug, error, info, instrument, Instrument, Span};
use uuid::Uuid;

use crate::proto::{
    ClientSyncState, Delta, FileAction, ServerSyncStateResponse, SyncRequest,
    harmonic_server::Harmonic,
};
use crate::sync::handler::{handle_sync_payload, SyncStatus};
use crate::sync::{self, Config, SyncState};
use crate::utils::HarmonicError;

// a session that has not opened a stream is only kept alive for this long,
// this bounds sessions from sync runs that transferred nothing (all skips)
const IDLE_SESSION_TTL_MICROS: i64 = 300 * 1_000_000;

#[derive(Clone, Debug)]
pub struct SessionData {
    // allow for resume in future
    pub timestamp_micros: i64,
    pub local_state: SyncState,
    pub sync_plan: Vec<FileAction>,
    pub active_streams: usize,
}

#[derive(Debug)]
pub struct HarmonicService {
    pub sync_sessions: Arc<Mutex<HashMap<Uuid, SessionData>>>,
    pub config: sync::Config,
}

#[async_trait]
impl Harmonic for HarmonicService {
    type HarmonizeSyncRequestStream = ReceiverStream<Result<SyncRequest, Status>>;

    #[instrument(skip(self, request), fields(sync_uuid = tracing::field::Empty))]
    async fn harmonize_client_initiate_sync(
        &self,
        request: Request<ClientSyncState>,
    ) -> Result<Response<ServerSyncStateResponse>, Status> {
        info!("Received request {:?}", request);

        info!("Parsing request");
        let request_message = request.into_inner();
        let sync_uuid = request_message.sync_uuid;
        tracing::Span::current().record("sync_uuid", &sync_uuid);
        let request_timestamp =
            chrono::DateTime::from_timestamp_micros(request_message.timestamp_last_sync_micro)
                .ok_or_else(|| Status::invalid_argument("Could not parse datetime timestamp"))?;
        info!("Got time {:?} from timestamp", request_timestamp);
        let files_list: Vec<crate::proto::FileStatus> = request_message.status_list;

        let state_now = sync::generate_state(&self.config.sync_path, true)
            .map_err(|e| Status::internal(format!("Unable to generate current state: {}", e)))?;

        let sync_plan = sync::generate_sync_plan(&state_now, &files_list)
            .map_err(|e| Status::internal(format!("Unable to generate sync plan: {}", e)))?;

        {
            let mut sessions = self.sync_sessions.lock().await;
            evict_idle_sessions(&mut sessions, chrono::Utc::now().timestamp_micros());

            sessions.insert(
                Uuid::from_str(&sync_uuid).map_err(|e| {
                    Status::invalid_argument(format!(
                        "Did not receive valid uuid from client. Conversion failed: {}",
                        e
                    ))
                })?,
                SessionData {
                    timestamp_micros: chrono::Utc::now().timestamp_micros(),
                    local_state: state_now,
                    sync_plan: sync_plan.clone(),
                    active_streams: 0,
                },
            );
        }

        let response_strategy = ServerSyncStateResponse {
            sync_uuid,
            timestamp_micro: chrono::Utc::now().timestamp_micros(),
            sync_plan,
        };

        Ok(Response::new(response_strategy))
    }

    #[instrument(skip(self, request), fields(session_uuid = tracing::field::Empty))]
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

        tracing::Span::current().record("session_uuid", tracing::field::display(&session_uuid));

        // a sync run transfers its files over separate streams sharing this
        // session, so the session must stay alive until the last one finishes
        {
            let mut sessions = self.sync_sessions.lock().await;
            let session = sessions
                .get_mut(&session_uuid)
                .ok_or_else(|| Status::not_found("Session not found"))?;
            session.active_streams += 1;
        }

        let request_stream = request.into_inner();
        let (tx, rx) = mpsc::channel::<Result<SyncRequest, Status>>(256);

        handle_sync_request_stream(
            request_stream,
            tx,
            self.config.clone(),
            self.sync_sessions.clone(),
            session_uuid,
        )
        .await
        .map_err(|e| {
            error!("Failed to handle sync request stream: {:?}", e);
            Status::internal(format!("Failed to handle sync request stream: {}", e))
        })?;

        Ok(Response::new(ReceiverStream::new(rx)))
    }
}

#[instrument(skip(stream, tx, config, sync_sessions, session_uuid))]
async fn handle_sync_request_stream(
    stream: Streaming<SyncRequest>,
    tx: Sender<Result<SyncRequest, Status>>,
    config: Config,
    sync_sessions: Arc<Mutex<HashMap<Uuid, SessionData>>>,
    session_uuid: Uuid,
) -> Result<JoinHandle<Result<()>>> {
    let handle = tokio::spawn(
        async move {
            // held open so the response stream only ends after the session
            // bookkeeping below has run
            let keepalive_tx = tx.clone();

            let result = route_sync_request_stream(stream, tx, config).await;

            {
                let mut sessions = sync_sessions.lock().await;
                if let Some(session) = sessions.get_mut(&session_uuid) {
                    session.active_streams -= 1;
                    if session.active_streams == 0 {
                        sessions.remove(&session_uuid);
                    }
                }
            }

            if result.is_ok() {
                // wait for flushing to complete. maybe this can be made asynchronous
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

                info!("Sync request stream completed successfully");
            }

            drop(keepalive_tx);
            result
        }
        .instrument(Span::current()),
    );

    Ok(handle)
}

fn evict_idle_sessions(sessions: &mut HashMap<Uuid, SessionData>, now_micros: i64) {
    sessions.retain(|_, session| {
        session.active_streams > 0
            || now_micros - session.timestamp_micros < IDLE_SESSION_TTL_MICROS
    });
}

async fn route_sync_request_stream(
    mut stream: Streaming<SyncRequest>,
    tx: Sender<Result<SyncRequest, Status>>,
    config: Config,
) -> Result<()> {
    let mut file_path: PathBuf = Default::default();
    let mut writer_tx: Option<Sender<Delta>> = Default::default();

    while let Some(sync_request_result) = stream.next().await {
        match sync_request_result {
            Ok(sync_request) => {
                let Some(payload) = sync_request.payload else {
                    debug!("Received SyncRequest with no payload, skipping");
                    continue;
                };

                let sink = Box::pin(
                    PollSender::new(tx.clone())
                        .sink_map_err(|e| HarmonicError::SendError(e.to_string()))
                        .with(|req| async move { Ok::<_, HarmonicError>(Ok(req)) }),
                );

                match handle_sync_payload(
                    payload,
                    sink,
                    &mut file_path,
                    config.clone(),
                    &mut writer_tx,
                )
                .await
                {
                    Ok(status) => {
                        if matches!(status, SyncStatus::Completed) {
                            // drop writer_tx first to close the delta writer channel to ensure flush
                            drop(writer_tx);
                            drop(tx);
                            break;
                        };
                    }
                    Err(e) => {
                        error!("Error routing sync request: {:?}", e);
                        return Err(anyhow!("Failed to route sync request: {}", e));
                    }
                }
            }
            Err(status_err) => {
                error!("Error receiving sync request from stream: {:?}", status_err);
                return Err(anyhow!("Stream error: {}", status_err));
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn session_with(active_streams: usize, age_micros: i64, now_micros: i64) -> SessionData {
        SessionData {
            timestamp_micros: now_micros - age_micros,
            local_state: SyncState {
                last_sync_timestamp_micros: 0,
                tree: BTreeMap::new(),
            },
            sync_plan: vec![],
            active_streams,
        }
    }

    #[test]
    fn test_evict_idle_sessions_removes_expired_sessions_without_streams() {
        let now = 10_000_000;
        let mut sessions = HashMap::new();
        sessions.insert(Uuid::new_v4(), session_with(0, IDLE_SESSION_TTL_MICROS, now));
        sessions.insert(
            Uuid::new_v4(),
            session_with(0, IDLE_SESSION_TTL_MICROS + 1, now),
        );

        evict_idle_sessions(&mut sessions, now);

        assert!(
            sessions.is_empty(),
            "idle sessions past the ttl must be evicted"
        );
    }

    #[test]
    fn test_evict_idle_sessions_keeps_sessions_with_active_streams() {
        let now = 10_000_000;
        let mut sessions = HashMap::new();
        // expired by age but still transferring
        sessions.insert(Uuid::new_v4(), session_with(1, IDLE_SESSION_TTL_MICROS + 1, now));

        evict_idle_sessions(&mut sessions, now);

        assert_eq!(
            sessions.len(),
            1,
            "sessions with in flight streams must never be evicted"
        );
    }

    #[test]
    fn test_evict_idle_sessions_keeps_recent_sessions() {
        let now = 10_000_000;
        let mut sessions = HashMap::new();
        sessions.insert(Uuid::new_v4(), session_with(0, IDLE_SESSION_TTL_MICROS - 1, now));

        evict_idle_sessions(&mut sessions, now);

        assert_eq!(sessions.len(), 1, "recent sessions must be kept");
    }
}
