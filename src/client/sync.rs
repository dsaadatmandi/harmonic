use std::fmt::Debug;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use tokio::sync::mpsc::Sender;
use tokio::task::JoinHandle;
use tokio_util::sync::PollSender;
use tonic::codec::CompressionEncoding;
use tonic::transport::{Certificate, Channel};
use tracing::instrument::Instrumented;
use tracing::{Instrument, Span, debug, error, info, instrument};
use uuid::Uuid;

use crate::proto::{
    BlockSignatures, ClientSyncState, Delta, FileAction, FileChangeType, FileStatus, ServerSyncStateResponse,
    SyncRequest, TransferDirection, harmonic_client::HarmonicClient, sync_request,
};
use crate::sync::handler::{SyncStatus, delete_sync_file, handle_sync_payload};
use crate::sync::state::BlockCache;
use crate::sync::transfer::send_block_signatures_for_file;
use crate::sync::{self, Config};
use crate::utils::HarmonicError;
use crate::utils::tracing::send_trace;
use crate::utils::writer::delta_writer;

#[instrument(fields(sync_uuid = tracing::field::Empty))]
pub async fn run_sync(config: &Config, force_bootstrap: bool) -> Result<()> {
    debug!("Starting sync execution");
    let sync_uuid = Uuid::new_v4();
    tracing::Span::current().record("sync_uuid", tracing::field::display(&sync_uuid));

    let tls_config =
        tonic::transport::ClientTlsConfig::new()
            .ca_certificate(get_cert(config, force_bootstrap).await.clone());

    let channel = Channel::builder(
        config
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
        sync::generate_state(&config.sync_path, true).context("Failed to generate state")?;
    let status_list = sync::build_status_list(&last_state, &now_state);

    // wont check with server which is not ideal
    let change_count = status_list
        .iter()
        .filter(|s| s.change_type != FileChangeType::Unchanged as i32)
        .count();
    if change_count == 0 {
        info!("No updates to push");
        // return Ok(());
    }

    let response = send_state_to_server(
        &sync_uuid,
        last_state.last_sync_timestamp_micros,
        status_list,
        client.clone(),
    )
    .await
    .context("Error awaiting response from server to sync intiation.")?;

    let files_to_send = response.sync_plan;

    let result = send_data_to_server(
        client.clone(),
        files_to_send,
        &sync_uuid,
        config.sync_path.clone(),
        config.clone(),
    )
    .await;
    if let Err(e) = &result {
        error!("Sync failed due to: {:?}", e);
    }

    // the state is only persisted after a successful transfer, a partial sync
    // must be retried on the next run
    result?;

    info!("Completed Sync");

    sync::save_state(now_state).context("Failed to save state")?;

    Ok(())
}

#[instrument]
pub fn trigger_sync_task(config: Config, force_bootstrap: bool) -> Instrumented<JoinHandle<()>> {
    tokio::spawn(async move {
        if let Err(e) = run_sync(&config, force_bootstrap).await {
            error!("Sync task failed: {:#}", e)
        }
    })
    .instrument(Span::current())
}

#[instrument(skip(status_list, client), fields(sync_uuid = %sync_uuid, status_count = status_list.len()))]
async fn send_state_to_server<T: Debug>(
    sync_uuid: &Uuid,
    last_sync_timestamp: i64,
    status_list: Vec<FileStatus>,
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
        status_list,
    });

    let response = client
        .harmonize_client_initiate_sync(request)
        .await?
        .into_inner();

    Ok(response)
}

/// Skipped files need no transfer, running them through the stream would
/// stall both sides waiting for messages that never come
fn is_actionable(action: &FileAction) -> bool {
    action.direction != TransferDirection::Skip as i32
}

/// Runs the file actions of a sync plan in parallel, bounded by a semaphore.
/// Every file is attempted even if one fails and the combined result fails so
/// the caller does not persist state for a partially transferred sync
async fn execute_file_transfers<F, Fut>(file_actions: Vec<FileAction>, transfer: F) -> Result<()>
where
    F: Fn(FileAction) -> Fut,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    let semaphore = Arc::new(tokio::sync::Semaphore::new(10)); // Limit concurrency
    let mut join_set = tokio::task::JoinSet::new();

    for action in file_actions.into_iter().filter(is_actionable) {
        let permit = semaphore.clone().acquire_owned().await.unwrap();

        let task = transfer(action);
        join_set.spawn(async move {
            let _permit = permit;
            task.await
        });
    }

    let mut failure: Option<anyhow::Error> = None;
    while let Some(res) = join_set.join_next().await {
        match res {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                error!("Failed to sync file: {:#}", e);
                failure.get_or_insert(e);
            }
            Err(e) => {
                error!("Task join error: {:?}", e);
                failure.get_or_insert(anyhow::anyhow!("File transfer task failed: {:?}", e));
            }
        }
    }

    match failure {
        Some(e) => Err(e.context("One or more file transfers failed")),
        None => Ok(()),
    }
}

#[instrument(skip(client, file_actions, sync_path), fields(sync_uuid = %sync_uuid, action_count = file_actions.len()))]
async fn send_data_to_server<T: Debug>(
    client: HarmonicClient<T>,
    file_actions: Vec<FileAction>,
    sync_uuid: &Uuid,
    sync_path: PathBuf,
    config: Config,
) -> Result<()>
where
    T: tonic::client::GrpcService<tonic::body::Body> + Send + Clone + 'static,
    T::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    T::ResponseBody: tonic::codegen::Body<Data = tonic::codegen::Bytes> + Send + 'static,
    <T::ResponseBody as tonic::codegen::Body>::Error:
        Into<Box<dyn std::error::Error + Send + Sync>> + Send,
    <T as tonic::client::GrpcService<tonic::body::Body>>::Future: Send,
{
    execute_file_transfers(file_actions, move |action| {
        let client = client.clone();
        let sync_path = sync_path.clone();
        let sync_uuid = *sync_uuid;
        let config = config.clone();
        async move { sync_file(config, client, action, sync_uuid, sync_path).await }
    })
    .await
}

#[instrument(skip(client, sync_path, config), fields(sync_uuid = %sync_uuid, action))]
async fn sync_file<T: Debug>(
    config: Config,
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
    let (tx, rx) = tokio::sync::mpsc::channel::<SyncRequest>(256);
    let out_stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    let mut request = tonic::Request::new(out_stream);
    request.metadata_mut().insert(
        "session-uuid",
        sync_uuid.to_string().parse().context("Invalid UUID")?,
    );

    let mut response_stream = client.harmonize_sync_request(request).await?.into_inner();

    // rewrite this to use handler

    tx.send(SyncRequest {
        payload: Some(sync_request::Payload::FileAction(action.clone())),
    })
    .await
    .map_err(|e| HarmonicError::SendError(e.to_string()))?;

    let mut file_path = sync::from_protocol_path(&action.path);
    let abs_file_path = sync_path.join(&file_path);

    if action.direction == TransferDirection::Delete as i32 {
        debug!("Delete mode: removing local copy of {:?}", abs_file_path);
        delete_sync_file(&file_path, &config).await?;
        tx.send(SyncRequest {
            payload: Some(sync_request::Payload::Complete(true)),
        })
        .await
        .map_err(|e| HarmonicError::SendError(e.to_string()))?;
    }

    if action.direction == TransferDirection::Download as i32 {
        let mut sink =
            PollSender::new(tx.clone()).sink_map_err(|e| HarmonicError::SendError(e.to_string()));

        if abs_file_path.exists() {
            let _ = send_block_signatures_for_file(&file_path, &mut sink, &config).await;
        } else {
            // Send empty signatures if file missing
            let _ = sink
                .send(SyncRequest {
                    payload: Some(sync_request::Payload::Signatures(BlockSignatures {
                        block_size: config.block_size,
                        blocks: vec![],
                    })),
                })
                .await;
        }
    }

    let mut writer_tx: Option<Sender<Delta>> = None;
    if action.direction == TransferDirection::Download as i32 {
        debug!("Download mode: preparing delta writer for {:?}", abs_file_path);
        if abs_file_path.exists() {
            debug!("File exists, generating block signatures");
            let (_sig, cache) = crate::sync::state::generate_blocks_signatures(&file_path, &config)
                .await
                .unwrap_or((
                    BlockSignatures::default(),
                    BlockCache { blocks: vec![] },
                ));
            writer_tx = Some(
                delta_writer(
                    &abs_file_path,
                    cache,
                    action.timestamp_latest_modified.unwrap_or_default(),
                )
                .await,
            );
        } else {
            debug!("File doesn't exist, creating delta writer with empty cache");
            writer_tx = Some(
                delta_writer(
                    &abs_file_path,
                    BlockCache { blocks: vec![] },
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
                        config.clone(),
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

    info!("Completed sync for {:?}", &abs_file_path);

    Ok(())
}

async fn get_cert(config: &Config, force_bootstrap: bool) -> &'static Certificate {
    static CERT: tokio::sync::OnceCell<Certificate> = tokio::sync::OnceCell::const_new();

    CERT.get_or_init(|| async {
        if force_bootstrap {
            crate::client::bootstrap_from_server(&config.socket_addr)
                .await
                .expect("Failed to bootstrap certificate from server")
        } else {
            match crate::client::load_cert() {
                Ok(c) => c,
                Err(_) => crate::client::bootstrap_from_server(&config.socket_addr)
                    .await
                    .expect("Failed to bootstrap certificate from server"),
            }
        }
    })
    .await
}


#[cfg(test)]
mod tests {
    use super::*;

    fn mk_transfer_action(name: &str) -> FileAction {
        FileAction {
            path: String::from(name),
            direction: TransferDirection::Upload as i32,
            timestamp_latest_modified: None,
        }
    }

    #[tokio::test]
    async fn test_execute_file_transfers_succeeds_when_all_files_succeed() {
        let actions = vec![
            mk_transfer_action("a.txt"),
            mk_transfer_action("b.txt"),
        ];

        let result = execute_file_transfers(actions, |_| async move {
            Ok(())
        })
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_execute_file_transfers_fails_when_any_file_fails() {
        let actions = vec![
            mk_transfer_action("a.txt"),
            mk_transfer_action("b.txt"),
            mk_transfer_action("c.txt"),
        ];

        let result = execute_file_transfers(actions, |action| async move {
            if action.path == "b.txt" {
                Err(anyhow::anyhow!("transfer failed"))
            } else {
                Ok(())
            }
        })
        .await;

        assert!(result.is_err(), "one failed file must fail the whole transfer");
    }

    #[tokio::test]
    async fn test_execute_file_transfers_attempts_all_files_despite_failure() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_in_task = attempts.clone();

        let actions = vec![
            mk_transfer_action("a.txt"),
            mk_transfer_action("b.txt"),
            mk_transfer_action("c.txt"),
        ];

        let result = execute_file_transfers(actions, move |action| {
            let attempts = attempts_in_task.clone();
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                if action.path == "a.txt" {
                    Err(anyhow::anyhow!("transfer failed"))
                } else {
                    Ok(())
                }
            }
        })
        .await;

        assert!(result.is_err());
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            3,
            "all files must be attempted despite one failing"
        );
    }

    #[test]
    fn test_is_actionable() {
        let mk_action = |direction: TransferDirection| FileAction {
            path: String::from("file.txt"),
            direction: direction as i32,
            timestamp_latest_modified: None,
        };

        assert!(is_actionable(&mk_action(TransferDirection::Upload)));
        assert!(is_actionable(&mk_action(TransferDirection::Download)));
        assert!(is_actionable(&mk_action(TransferDirection::Delete)));
        assert!(!is_actionable(&mk_action(TransferDirection::Skip)));
    }
}
