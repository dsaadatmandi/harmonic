use anyhow::{Context, Result};
use harmonic::proto::{FileAction, FileStatus};
use harmonic::sync::handler::{SyncStatus, handle_sync_payload};
use harmonic::utils::HarmonicError;
use harmonic::utils::tracing::{send_trace, tracing_orchestrator};
use harmonic::utils::writer::delta_writer;
use std::collections::VecDeque;
use std::fmt::Debug;

use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc::Sender;
use tonic::codec::CompressionEncoding;
use tracing::instrument::Instrumented;

use futures::SinkExt;
use notify::EventKind;
use once_cell::sync::Lazy;

use tokio::sync::{Mutex, OnceCell};
use tokio::task::JoinHandle;
use tokio_stream::StreamExt;
use tokio_util::sync::PollSender;
use tonic::transport::{Certificate, Channel};
use tracing::{Instrument, Span, debug, error, info, instrument};
use uuid::Uuid;

use harmonic::client::{async_watch, bootstrap_from_server, load_cert};
use harmonic::proto::{
    ClientSyncState, ServerSyncStateResponse, TransferDirection, harmonic_client::HarmonicClient,
};

use harmonic::sync::{self, Config};

use harmonic::proto::SyncRequest;

const QUEUE_CHECK_SEC_INTERVAL_SEC: u64 = 10;

static QUEUE: Lazy<Arc<Mutex<VecDeque<bool>>>> =
    Lazy::new(|| Arc::new(Mutex::new(VecDeque::new())));

static CONFIG: Lazy<Config> = Lazy::new(|| sync::load_config().expect("Failed to load config"));

static CERT: OnceCell<Certificate> = OnceCell::const_new();

#[tokio::main]
async fn main() -> Result<()> {
    tracing_orchestrator(&CONFIG.log_level);

    let p = PathBuf::from(&CONFIG.sync_path);

    #[cfg(feature = "event-based")]
    let _watcher_task = start_watcher(p, &CONFIG);

    #[cfg(feature = "schedule-based")]
    let _scheduler_task = start_scheduler(&CONFIG);

    #[cfg(feature = "manual-only")]
    {
        info!("Triggering manual sync");
        let result = trigger_sync_task()
            .await
            .await
            .context("Initial sync task failed");

        harmonic::utils::tracing::shutdown_tracer().await;

        result?;
        Ok(())
    }

    #[cfg(not(feature = "manual-only"))]
    {
        let mut queue_check_interval = tokio::time::interval(tokio::time::Duration::from_secs(
            QUEUE_CHECK_SEC_INTERVAL_SEC,
        ));

        loop {
            let mut queue = QUEUE.lock().await;
            if let Some(_) = queue.pop_front() {
                queue.clear();
                drop(queue);
                trigger_sync_task()
                    .await
                    .await
                    .context("Sync task failed")?;
            }
            queue_check_interval.tick().await;
        }
    }
}

#[instrument]
async fn trigger_sync_task() -> Instrumented<JoinHandle<()>> {
    tokio::spawn(async move {
        if let Err(e) = run_sync().await {
            error!("Sync task failed: {:#}", e)
        }
    })
    .instrument(Span::current())
}

#[instrument(fields(sync_uuid = tracing::field::Empty))]
async fn run_sync() -> Result<()> {
    debug!("Starting sync execution");
    let sync_uuid = Uuid::new_v4();
    tracing::Span::current().record("sync_uuid", tracing::field::display(&sync_uuid));

    let tls_config =
        tonic::transport::ClientTlsConfig::new().ca_certificate(get_cert().await.clone());

    let channel = Channel::builder(
        CONFIG
            .server_uri()
            .parse()
            .context("Unable to convert address to URI")?,
    )
    .tls_config(tls_config)?
    .connect()
    .await
    .context("Unable to connect")?;
    let client = HarmonicClient::with_interceptor(channel, send_trace);

    #[cfg(feature = "compression-zstd")]
    let client = client
        .send_compressed(CompressionEncoding::Zstd)
        .accept_compressed(CompressionEncoding::Zstd);

    let last_state = sync::load_state().context("Unable to load previous state")?;
    let now_state =
        sync::generate_state(&CONFIG.sync_path, true).context("Failed to generate state")?;
    let diffs = sync::compare_states(&last_state, &now_state);

    // wont check with server which is not ideal
    if diffs.is_empty() {
        info!("No updates to push");
        // return Ok(());
    }

    let response = send_state_to_server(
        &sync_uuid,
        last_state.last_sync_timestamp_micros,
        diffs,
        client.clone(),
    )
    .await
    .context("Error awaiting response from server to sync intiation.")?;

    let files_to_send = response.sync_plan;

    let result = send_data_to_server(
        client.clone(),
        files_to_send,
        &sync_uuid,
        CONFIG.sync_path.clone(),
    )
    .await;
    match result {
        Ok(()) => info!("Completed Sync"),
        Err(e) => error!("Sync failed due to: {:?}", e),
    }

    sync::save_state(now_state).context("Failed to save state")?;

    Ok(())
}

#[instrument(skip(diffs, client), fields(sync_uuid = %sync_uuid, diff_count = diffs.len()))]
async fn send_state_to_server<T: Debug>(
    sync_uuid: &Uuid,
    last_sync_timestamp: i64,
    diffs: Vec<sync::Diff>,
    mut client: HarmonicClient<T>,
) -> Result<ServerSyncStateResponse>
where
    T: tonic::client::GrpcService<tonic::body::Body> + Send + 'static,
    T::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    T::ResponseBody: tonic::codegen::Body<Data = tonic::codegen::Bytes> + Send + 'static,
    <T::ResponseBody as tonic::codegen::Body>::Error:
        Into<Box<dyn std::error::Error + Send + Sync>> + Send,
{
    let request = tonic::Request::new(ClientSyncState {
        sync_uuid: sync_uuid.to_string(),
        timestamp_last_sync_micro: last_sync_timestamp,
        status_list: diffs
            .into_iter()
            .map(|d| FileStatus::try_from(d))
            .collect::<Result<Vec<_>, _>>()
            .context("Unable to convert Diff into FileStatus")?,
    });

    let response = client
        .harmonize_client_initiate_sync(request)
        .await?
        .into_inner();

    Ok(response)
}

#[instrument(skip(client, file_actions, sync_path), fields(sync_uuid = %sync_uuid, action_count = file_actions.len()))]
async fn send_data_to_server<T: Debug>(
    client: HarmonicClient<T>,
    file_actions: Vec<FileAction>,
    sync_uuid: &Uuid,
    sync_path: PathBuf,
) -> Result<()>
where
    T: tonic::client::GrpcService<tonic::body::Body> + Send + Clone + 'static,
    T::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    T::ResponseBody: tonic::codegen::Body<Data = tonic::codegen::Bytes> + Send + 'static,
    <T::ResponseBody as tonic::codegen::Body>::Error:
        Into<Box<dyn std::error::Error + Send + Sync>> + Send,
    <T as tonic::client::GrpcService<tonic::body::Body>>::Future: Send,
{
    let semaphore = Arc::new(tokio::sync::Semaphore::new(10)); // Limit concurrency
    let mut join_set = tokio::task::JoinSet::new();

    for action in file_actions {
        let client = client.clone();
        let sync_uuid = *sync_uuid;
        let sync_path = sync_path.clone();
        let permit = semaphore.clone().acquire_owned().await.unwrap();

        join_set.spawn(async move {
            let _permit = permit;
            if let Err(e) = sync_file(client, action, sync_uuid, sync_path).await {
                error!("Failed to sync file: {:?}", e);
            }
        });
    }

    while let Some(res) = join_set.join_next().await {
        if let Err(e) = res {
            error!("Task join error: {:?}", e);
        }
    }

    Ok(())
}

async fn sync_file<T: Debug>(
    mut client: HarmonicClient<T>,
    action: FileAction,
    sync_uuid: Uuid,
    sync_path: PathBuf,
) -> Result<()>
where
    T: tonic::client::GrpcService<tonic::body::Body> + Send + 'static,
    T::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    T::ResponseBody: tonic::codegen::Body<Data = tonic::codegen::Bytes> + Send + 'static,
    <T::ResponseBody as tonic::codegen::Body>::Error:
        Into<Box<dyn std::error::Error + Send + Sync>> + Send,
    <T as tonic::client::GrpcService<tonic::body::Body>>::Future: Send,
{
    let (tx, rx) = tokio::sync::mpsc::channel::<SyncRequest>(10);
    let out_stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    let mut request = tonic::Request::new(out_stream);
    request.metadata_mut().insert(
        "session-uuid",
        sync_uuid.to_string().parse().context("Invalid UUID")?,
    );

    let mut response_stream = client.harmonize_sync_request(request).await?.into_inner();

    // rewrite this to use handler

    tx.send(SyncRequest {
        payload: Some(harmonic::proto::sync_request::Payload::FileAction(
            action.clone(),
        )),
    })
    .await
    .map_err(|e| HarmonicError::SendError(e.to_string()))?;

    let mut file_path = sync_path.join(&action.path);

    if action.direction == TransferDirection::Download as i32 {
        let mut sink =
            PollSender::new(tx.clone()).sink_map_err(|e| HarmonicError::SendError(e.to_string()));

        if file_path.exists() {
            let _ = harmonic::sync::transfer::send_block_signatures_for_file(
                &file_path, &mut sink, &CONFIG,
            )
            .await;
        } else {
            // Send empty signatures if file missing
            let _ = sink
                .send(SyncRequest {
                    payload: Some(harmonic::proto::sync_request::Payload::Signatures(
                        harmonic::proto::BlockSignatures {
                            block_size: CONFIG.block_size,
                            blocks: vec![],
                        },
                    )),
                })
                .await;
        }
    }

    let mut writer_tx: Option<Sender<harmonic::proto::Delta>> = None;
    if action.direction == TransferDirection::Download as i32 {
        debug!("Download mode: preparing delta writer for {:?}", file_path);
        if file_path.exists() {
            debug!("File exists, generating block signatures");
            let (_sig, cache) =
                harmonic::sync::state::generate_blocks_signatures(&file_path, &CONFIG)
                    .await
                    .unwrap_or((
                        harmonic::proto::BlockSignatures::default(),
                        harmonic::sync::state::BlockCache { blocks: vec![] },
                    ));
            writer_tx = Some(
                delta_writer(
                    &file_path,
                    cache,
                    action.timestamp_latest_modified.unwrap_or_default(),
                )
                .await,
            );
        } else {
            debug!("File doesn't exist, creating delta writer with empty cache");
            writer_tx = Some(
                delta_writer(
                    &file_path,
                    harmonic::sync::state::BlockCache { blocks: vec![] },
                    action.timestamp_latest_modified.unwrap_or_default(),
                )
                .await,
            );
        }
        debug!("Delta writer created for download");
    }

    while let Some(response) = response_stream.next().await {
        match response {
            Ok(req) => {
                if let Some(payload) = req.payload {
                    let sink = PollSender::new(tx.clone())
                        .sink_map_err(|e| HarmonicError::SendError(e.to_string()));

                    match handle_sync_payload(
                        payload,
                        sink,
                        &mut file_path,
                        CONFIG.clone(),
                        &mut writer_tx,
                    )
                    .await
                    {
                        Ok(status) => {
                            if matches!(status, SyncStatus::Completed) {
                                // same as server - ensure flushing is complete before closing stream
                                drop(writer_tx);
                                drop(tx);
                                break;
                            };
                        }
                        Err(e) => {
                            error!("Error routing sync request: {:?}", e);
                            return Err(anyhow::anyhow!("Failed to route sync request: {}", e));
                        }
                    }
                }
            }
            Err(e) => return Err(anyhow::anyhow!("Error in response stream: {}", e)),
        }
    }

    // same as server - to fix if possible although overhead is once per sync, not per file. hence small
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    info!("Completed sync for {:?}", &file_path);

    Ok(())
}

#[instrument(skip(config), fields(watch_path = %p.display()))]
fn start_watcher(p: PathBuf, config: &sync::Config) -> Instrumented<JoinHandle<()>> {
    info!("Starting file system watcher");
    let c = config.clone();
    tokio::spawn(async move {
        let (_watcher, mut rx) = match async_watch(p).await {
            Ok((w, rx)) => (w, rx),
            Err(e) => {
                error!("Error in creating file watcher: {:#}", e);
                return;
            }
        };

        let mut change_score: u64 = 0;

        while let Some(Ok(event)) = rx.next().await {
            change_score += calculate_change_score(event.kind, &c);
            if should_trigger_sync(change_score, &c) {
                info!("Sufficient changes accrued. Triggering sync job.");
                QUEUE.lock().await.push_back(true);
                change_score = 0;
            }
        }
    })
    .instrument(Span::current())
}

fn calculate_change_score(event_kind: EventKind, config: &sync::Config) -> u64 {
    match event_kind {
        EventKind::Modify(_) => config.modify_weight,
        EventKind::Remove(_) => config.remove_weight,
        EventKind::Create(_) => config.create_weight,
        _ => {
            info!("Unmatched event of type {:?}", event_kind);
            0
        }
    }
}

fn should_trigger_sync(score: u64, config: &sync::Config) -> bool {
    score > config.sync_threshold
}

async fn get_cert() -> &'static Certificate {
    CERT.get_or_init(|| async {
        match load_cert() {
            Ok(c) => c,
            Err(_) => bootstrap_from_server(&CONFIG.socket_addr)
                .await
                .expect("Failed to bootstrap certificate from server"),
        }
    })
    .await
}

#[cfg(feature = "schedule-based")]
fn start_scheduler(config: &sync::Config) -> Instrumented<JoinHandle<()>> {
    debug!("Starting scheduler");
    let mut delay_interval =
        tokio::time::interval(tokio::time::Duration::from_secs(config.schedule_delay));
    tokio::spawn(async move {
        loop {
            QUEUE.lock().await.push_back(true);
            delay_interval.tick().await;
        }
    })
    .instrument(Span::current())
}

// #[cfg(test)]
// mod tests {
//     use harmonic::proto::FileAction;

//     use super::*;

//     #[test]
//     fn test_handle_response() {
//         let response: ServerSyncStateResponse = ServerSyncStateResponse {
//             sync_uuid: String::from("sample-uuid"),
//             timestamp_micro: 12345,
//             sync_plan: vec![
//                 FileAction {
//                     path: String::from("/file_client_send.txt"),
//                     direction: TransferDirection::ClientSendFile as i32,
//                 },
//                 FileAction {
//                     path: String::from("/file_server_send.txt"),
//                     direction: TransferDirection::ServerRequestFile as i32,
//                 },
//                 FileAction {
//                     path: String::from("/file_skip.txt"),
//                     direction: TransferDirection::Skip as i32,
//                 },
//             ],
//         };
//         let generated_actions = handle_response(response);

//         assert_eq!(generated_actions.len(), 1);
//         assert_eq!(generated_actions[0], PathBuf::from("/file_client_send.txt"));
//     }

//     #[test]
//     fn test_handle_response_empty_plan() {
//         let response: ServerSyncStateResponse = ServerSyncStateResponse {
//             sync_uuid: String::from("sample-uuid"),
//             timestamp_micro: 12345,
//             sync_plan: Vec::new(),
//         };
//         let generated_actions = handle_response(response);

//         assert_eq!(generated_actions.len(), 0);
//     }

//     #[test]
//     fn test_handle_response_invalid_direction() {
//         let response: ServerSyncStateResponse = ServerSyncStateResponse {
//             sync_uuid: String::from("sample-uuid"),
//             timestamp_micro: 12345,
//             sync_plan: vec![
//                 FileAction {
//                     path: String::from("/file_client_send.txt"),
//                     direction: TransferDirection::ClientSendFile as i32,
//                 },
//                 FileAction {
//                     path: String::from("/file_invalid_direction.txt"),
//                     direction: 9,
//                 },
//             ],
//         };
//         let generated_actions = handle_response(response);

//         assert_eq!(generated_actions.len(), 1);
//         assert_eq!(generated_actions[0], PathBuf::from("/file_client_send.txt"));
//     }

//     #[test]
//     fn test_handle_response_complex_many_directions() {
//         let response: ServerSyncStateResponse = ServerSyncStateResponse {
//             sync_uuid: String::from("sample-uuid"),
//             timestamp_micro: 12345,
//             sync_plan: vec![
//                 FileAction {
//                     path: String::from("/file_client_send_1.txt"),
//                     direction: TransferDirection::ClientSendFile as i32,
//                 },
//                 FileAction {
//                     path: String::from("/file_server_send.txt"),
//                     direction: TransferDirection::ServerRequestFile as i32,
//                 },
//                 FileAction {
//                     path: String::from("/file_skip.txt"),
//                     direction: TransferDirection::Skip as i32,
//                 },
//                 FileAction {
//                     path: String::from("/file_client_send_2.txt"),
//                     direction: TransferDirection::ClientSendFile as i32,
//                 },
//                 FileAction {
//                     path: String::from("/file_client_send_3.txt"),
//                     direction: TransferDirection::ClientSendFile as i32,
//                 },
//                 FileAction {
//                     path: String::from("/file_client_send_2.txt"),
//                     direction: 999,
//                 },
//                 FileAction {
//                     path: String::from("/file_client_send_4.txt"),
//                     direction: TransferDirection::ClientSendFile as i32,
//                 },
//             ],
//         };
//         let generated_actions = handle_response(response);

//         assert_eq!(generated_actions.len(), 4);
//         // note this will probably fail if parallelizing iteration
//         assert_eq!(
//             generated_actions[0],
//             PathBuf::from("/file_client_send_1.txt")
//         );
//         assert_eq!(
//             generated_actions[1],
//             PathBuf::from("/file_client_send_2.txt")
//         );
//         assert_eq!(
//             generated_actions[2],
//             PathBuf::from("/file_client_send_3.txt")
//         );
//         assert_eq!(
//             generated_actions[3],
//             PathBuf::from("/file_client_send_4.txt")
//         );
//     }

//     #[test]
//     fn test_calculate_change_score() {
//         let config = sync::Config::default();

//         assert_eq!(
//             calculate_change_score(EventKind::Modify(notify::event::ModifyKind::Any), &config),
//             config.modify_weight
//         );
//         assert_eq!(
//             calculate_change_score(EventKind::Remove(notify::event::RemoveKind::Any), &config),
//             config.remove_weight
//         );
//         assert_eq!(
//             calculate_change_score(EventKind::Create(notify::event::CreateKind::Any), &config),
//             config.create_weight
//         );
//         assert_eq!(
//             calculate_change_score(EventKind::Access(notify::event::AccessKind::Any), &config),
//             0
//         );
//     }

//     #[test]
//     fn test_should_trigger_sync() {
//         let config = sync::Config::default();

//         assert!(!should_trigger_sync(config.sync_threshold, &config));
//         assert!(should_trigger_sync(config.sync_threshold + 1, &config));
//         assert!(!should_trigger_sync(config.sync_threshold - 1, &config));
//         assert!(!should_trigger_sync(0, &config));
//     }
// }
