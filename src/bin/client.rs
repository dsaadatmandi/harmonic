use anyhow::{Context, Result};
use clap::Parser;
use harmonic::proto::{ChangeType, FileAction, FileStatus};
use harmonic::sync::handler::{SyncStatus, delete_sync_file, handle_sync_payload};
use harmonic::utils::HarmonicError;
use harmonic::utils::tracing::{send_trace, tracing_orchestrator};
use harmonic::utils::writer::delta_writer;
use std::collections::VecDeque;
use std::fmt::Debug;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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

use harmonic::sync::{self, Config, from_protocol_path};

use harmonic::proto::SyncRequest;

#[derive(Parser, Debug)]
#[command(name = "harmonic-client")]
#[command(about = "Harmonic file synchronization client", long_about = None)]
struct Args {
    /// Bootstrap certificate from server
    #[arg(long)]
    bootstrap: bool,
    /// Use schedule-based sync mode
    #[arg(long)]
    schedule: bool,
    /// Use event-based sync mode
    #[arg(long)]
    event_based: bool,
}

const QUEUE_CHECK_SEC_INTERVAL_SEC: u64 = 10;

static QUEUE: Lazy<Arc<Mutex<VecDeque<bool>>>> =
    Lazy::new(|| Arc::new(Mutex::new(VecDeque::new())));

static CONFIG: Lazy<Config> = Lazy::new(|| sync::load_config().expect("Failed to load config"));

static FORCE_BOOTSTRAP: AtomicBool = AtomicBool::new(false);

static CERT: OnceCell<Certificate> = OnceCell::const_new();

#[tokio::main]
async fn main() -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let args = Args::parse();

    FORCE_BOOTSTRAP.store(args.bootstrap, Ordering::Relaxed);

    tracing_orchestrator(&CONFIG.log_level);

    let p = PathBuf::from(&CONFIG.sync_path);

    if args.event_based {
        let _watcher_task = start_watcher(p, &CONFIG);
    }

    if args.schedule {
        let _scheduler_task = start_scheduler(&CONFIG);
    }

    let manual = !args.event_based && !args.schedule;

    if manual {
        info!("Triggering manual sync");
        let result = trigger_sync_task()
            .await
            .await
            .context("Initial sync task failed");

        harmonic::utils::tracing::shutdown_tracer().await;

        result?;
        Ok(())
    } else {
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
    let status_list = sync::build_status_list(&last_state, &now_state)
        .context("Failed to build client status list")?;

    // wont check with server which is not ideal
    let change_count = status_list
        .iter()
        .filter(|s| s.change_type != ChangeType::Unchanged as i32)
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
        CONFIG.sync_path.clone(),
    )
    .await;
    if let Err(e) = &result {
        error!("Sync failed due to: {:?}", e);
    }

    // only persist the new state when the transfer succeeded, otherwise files
    // would be recorded as synced although they were not transferred
    sync::save_state_on_success(&result, now_state).context("Failed to save state")?;

    Ok(())
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
        async move { sync_file(client, action, sync_uuid, sync_path).await }
    })
    .await
}

#[instrument(skip(client, sync_path), fields(sync_uuid = %sync_uuid, action))]
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
        payload: Some(harmonic::proto::sync_request::Payload::FileAction(
            action.clone(),
        )),
    })
    .await
    .map_err(|e| HarmonicError::SendError(e.to_string()))?;

    let mut file_path = from_protocol_path(&action.path);
    let abs_file_path = sync_path.join(&file_path);

    if action.direction == TransferDirection::Delete as i32 {
        debug!("Delete mode: removing local copy of {:?}", abs_file_path);
        delete_sync_file(&file_path, &CONFIG).await?;
        tx.send(SyncRequest {
            payload: Some(harmonic::proto::sync_request::Payload::Complete(true)),
        })
        .await
        .map_err(|e| HarmonicError::SendError(e.to_string()))?;
    }

    if action.direction == TransferDirection::Download as i32 {
        let mut sink =
            PollSender::new(tx.clone()).sink_map_err(|e| HarmonicError::SendError(e.to_string()));

        if abs_file_path.exists() {
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
        debug!("Download mode: preparing delta writer for {:?}", abs_file_path);
        if abs_file_path.exists() {
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

    info!("Completed sync for {:?}", &abs_file_path);

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
        if FORCE_BOOTSTRAP.load(Ordering::Relaxed) {
            let cert = bootstrap_from_server(&CONFIG.socket_addr)
                .await
                .expect("Failed to bootstrap certificate from server");

            FORCE_BOOTSTRAP.store(false, Ordering::Relaxed);

            cert
        } else {
            match load_cert() {
                Ok(c) => c,
                Err(_) => bootstrap_from_server(&CONFIG.socket_addr)
                    .await
                    .expect("Failed to bootstrap certificate from server"),
            }
        }
    })
    .await
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

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
        // Scenario: one file transfer fails while others succeed
        // Expected: the whole transfer reports failure so the sync state is
        // not persisted, matching save_state_on_success semantics
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
        // Scenario: a transfer fails part way through the sync plan
        // Expected: remaining files are still attempted
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

    #[test]
    fn test_calculate_change_score() {
        let config = sync::Config::default();

        assert_eq!(
            calculate_change_score(EventKind::Modify(notify::event::ModifyKind::Any), &config),
            config.modify_weight
        );
        assert_eq!(
            calculate_change_score(EventKind::Remove(notify::event::RemoveKind::Any), &config),
            config.remove_weight
        );
        assert_eq!(
            calculate_change_score(EventKind::Create(notify::event::CreateKind::Any), &config),
            config.create_weight
        );
        assert_eq!(
            calculate_change_score(EventKind::Access(notify::event::AccessKind::Any), &config),
            0
        );
    }

    #[test]
    fn test_should_trigger_sync() {
        let config = sync::Config::default();

        assert!(!should_trigger_sync(config.sync_threshold, &config));
        assert!(should_trigger_sync(config.sync_threshold + 1, &config));
        assert!(!should_trigger_sync(config.sync_threshold - 1, &config));
        assert!(!should_trigger_sync(0, &config));
    }

    #[test]
    fn test_file_path_is_relative() {
        // Test that file_path initialization creates a relative path, not absolute
        let action_path = "test/file.txt";
        let file_path = PathBuf::from(action_path);

        assert!(!file_path.is_absolute(), "file_path should be relative, not absolute");
        assert_eq!(file_path.to_str().unwrap(), action_path);
    }

    #[test]
    fn test_abs_file_path_construction() {
        // Test that abs_file_path is correctly constructed from sync_path + relative path
        let sync_path = PathBuf::from("/Users/test/sync");
        let action_path = "test/file.txt";
        let file_path = PathBuf::from(action_path);
        let abs_file_path = sync_path.join(&file_path);

        assert!(abs_file_path.is_absolute(), "abs_file_path should be absolute");
        assert_eq!(abs_file_path.to_str().unwrap(), "/Users/test/sync/test/file.txt");
    }

    #[test]
    fn test_relative_path_with_get_absolute_path() {
        // Test that relative paths work correctly with get_absolute_path
        use harmonic::sync::transfer::get_absolute_path;
        use std::path::Path;

        let sync_path = Path::new("/Users/test/sync");
        let relative_path = Path::new("test/file.txt");

        let result = get_absolute_path(relative_path, sync_path);
        assert!(result.is_ok(), "get_absolute_path should accept relative paths");

        let abs = result.unwrap();
        assert!(abs.is_absolute());
        assert!(abs.to_str().unwrap().ends_with("test/file.txt"));
    }

    #[test]
    fn test_absolute_path_rejected_by_get_absolute_path() {
        // Test that absolute paths are rejected by get_absolute_path (security check)
        use harmonic::sync::transfer::get_absolute_path;
        use std::path::Path;

        let sync_path = Path::new("/Users/test/sync");
        let absolute_path = Path::new("/Users/test/sync/test/file.txt");

        let result = get_absolute_path(absolute_path, sync_path);
        assert!(result.is_err(), "get_absolute_path should reject absolute paths for security");
    }

    #[test]
    fn test_path_traversal_rejected_by_get_absolute_path() {
        // Test that path traversal attempts are rejected
        use harmonic::sync::transfer::get_absolute_path;
        use std::path::Path;

        let sync_path = Path::new("/Users/test/sync");
        let traversal_path = Path::new("../../../etc/passwd");

        let result = get_absolute_path(traversal_path, sync_path);
        assert!(result.is_err(), "get_absolute_path should reject path traversal attempts");
    }

    #[tokio::test]
    async fn test_empty_file_path_handling() {
        // Test path handling for empty files (the original bug scenario)
        use harmonic::sync::state::generate_blocks_signatures;

        let config = sync::Config::default();
        let temp_dir = std::env::temp_dir();
        let test_file = temp_dir.join("harmonic_test_empty.txt");

        // Create an empty file
        std::fs::write(&test_file, b"").expect("Failed to create test file");

        // Get relative path
        let relative_path = PathBuf::from("harmonic_test_empty.txt");

        // This should work with relative path
        let mut test_config = config.clone();
        test_config.sync_path = temp_dir.clone();

        let result = generate_blocks_signatures(&relative_path, &test_config).await;
        assert!(result.is_ok(), "generate_blocks_signatures should work with relative path");

        // Cleanup
        let _ = std::fs::remove_file(&test_file);
    }

    #[tokio::test]
    async fn test_nonexistent_file_path_handling() {
        // Test path handling for non-existent files
        use harmonic::sync::state::generate_blocks_signatures;

        let config = sync::Config::default();
        let temp_dir = std::env::temp_dir();

        // Get relative path for non-existent file
        let relative_path = PathBuf::from("harmonic_test_nonexistent_file_12345.txt");

        let mut test_config = config.clone();
        test_config.sync_path = temp_dir.clone();

        let result = generate_blocks_signatures(&relative_path, &test_config).await;
        assert!(result.is_ok(), "generate_blocks_signatures should handle non-existent files");

        let (signatures, cache) = result.unwrap();
        assert_eq!(signatures.blocks.len(), 0, "Non-existent file should have no blocks");
        assert_eq!(cache.blocks.len(), 0, "Non-existent file should have empty cache");
    }
}
