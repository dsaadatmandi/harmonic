use anyhow::{Context, Result};
use harmonic::error::HarmonicError;
use harmonic::harmonic::{FileStatus, FileSync};
use std::collections::VecDeque;
use std::error::Error;
use std::io::{ErrorKind};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc::Sender;

use futures::pin_mut;
use log::{error, info};
use notify::EventKind;
use once_cell::sync::Lazy;
use tokio::fs::File;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_stream::StreamExt;
use tonic::transport::Channel;
use uuid::Uuid;

use harmonic::common;
use harmonic::harmonic::{
    ClientSyncState, ServerSyncStateResponse, TransferDirection, harmonic_client::HarmonicClient,
};
use harmonic::watcher;

const QUEUE_CHECK_SEC_INTERVAL_SEC: u64 = 10;

static QUEUE: Lazy<Arc<Mutex<VecDeque<bool>>>> =
    Lazy::new(|| Arc::new(Mutex::new(VecDeque::new())));

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    let config = common::load_config().context("Failed to load config")?;
    let p = PathBuf::from(&config.sync_path);

    #[cfg(feature = "event-based")]
    let _watcher_task = start_watcher(p, &config);

    #[cfg(feature = "schedule-based")]
    let _scheduler_task = start_scheduler(config);

    let mut queue_check_interval = tokio::time::interval(tokio::time::Duration::from_secs(
        QUEUE_CHECK_SEC_INTERVAL_SEC,
    ));

    loop {
        let mut queue = QUEUE.lock().await;
        if let Some(_) = queue.pop_front() {
            queue.clear();
            drop(queue);
            trigger_sync_task().await;
        }
        queue_check_interval.tick().await;
    }
}

async fn trigger_sync_task() -> JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(e) = run_sync().await {
            error!("Sync task failed: {:#}", e)
        }
    })
}

async fn run_sync() -> Result<()> {
    let sync_uuid = Uuid::new_v4();
    let config = common::load_config().context("Failed to load config")?;
    let channel = Channel::builder(
        config
            .server_uri()
            .parse()
            .context("Unable to convert address to URI")?,
    )
    .connect()
    .await
    .context("Unable to connect")?;
    let client = HarmonicClient::new(channel);
    let client = client
        .send_compressed(CompressionEncoding::Zstd)
        .accept_compressed(CompressionEncoding::Zstd);

    let last_state = common::load_state().context("Unable to load previous state")?;
    let now_state =
        common::generate_state(&config.sync_path).context("Failed to generate state")?;
    let diffs = common::compare_states(&last_state, &now_state);

    if diffs.is_empty() {
        info!("No updates to push");
        return Ok(());
    }

    let response = send_state_to_server(
        &sync_uuid,
        last_state.last_sync_timestamp_micros,
        diffs,
        client.clone(),
    )
    .await
    .context("Error awaiting response from server to sync intiation.")?;

    let files_to_send = handle_response(response);

    let result =
        send_data_to_server(client.clone(), files_to_send, &sync_uuid, config.sync_path).await;
    match result {
        Ok(()) => info!("Completed Sync"),
        Err(e) => error!("Sync failed due to: {:?}", e),
    }

    common::save_state(now_state).context("Failed to save state")?;

    Ok(())
}

async fn send_state_to_server(
    sync_uuid: &Uuid,
    last_sync_timestamp: i64,
    diffs: Vec<common::Diff>,
    mut client: HarmonicClient<Channel>,
) -> Result<ServerSyncStateResponse> {
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

fn handle_response(response: ServerSyncStateResponse) -> Vec<PathBuf> {
    info!("Handling server response after initial request");

    response
        .sync_plan
        .into_iter()
        .filter_map(
            |action| match TransferDirection::try_from(action.direction).ok()? {
                TransferDirection::ClientSend => Some(PathBuf::from(action.path)),
                _ => None,
            },
        )
        .collect()
}

async fn send_data_to_server(
    mut client: HarmonicClient<Channel>,
    files: Vec<PathBuf>,
    sync_uuid: &Uuid,
    sync_path: PathBuf,
) -> Result<()> {
    let (tx, rx) = tokio::sync::mpsc::channel(10);
    let out = tokio_stream::wrappers::ReceiverStream::new(rx);
    let mut request = tonic::Request::new(out);
    request.metadata_mut().insert(
        "session-uuid",
        sync_uuid
            .to_string()
            .parse()
            .context("Unable to add session-uuid metadata to grpc request")?,
    );
    let mut inc = client
        .harmonize_synchronize_state(request)
        .await?
        .into_inner();
    let sync_path_receiver = sync_path.clone();

    let send_task = tokio::spawn(async move {
        if let Err(e) = create_send_task(files, &sync_path, tx).await {
            error!("Send task failed: {:#}", e);
        };
    });

    let mut cur_file: String = Default::default();
    let mut file_currently_writing: Option<File> = None;
    while let Some(response) = inc.next().await {
        match response {
            Ok(msg) => {
                let path = msg.path.clone();
                info!("Received data for file {}. Writing to path...", path);

                if file_currently_writing.is_none() || cur_file != path {
                    file_currently_writing = Some(
                        common::get_file(&msg, &sync_path_receiver)
                            .await
                            .context("Unable to get file to write to")?,
                    );
                    cur_file = path;
                }

                match file_currently_writing.as_mut() {
                    Some(f) => common::write_data_to_offset(msg, f).await.context("Failed to write data to offset")?,
                    None => return Err(HarmonicError::Io(std::io::Error::new(ErrorKind::Other, "Unable to get mutable file to write to")).into())
                }
            }
            Err(e) => {
                error!("Error in response stream from server: {:?}", e);
                break;
            }
        }
    }

    send_task.await?;

    Ok(())
}

async fn create_send_task(
    files: Vec<PathBuf>,
    sync_path: &PathBuf,
    tx: Sender<FileSync>,
) -> Result<()> {
    for f in files {
        let stream = common::file_to_chunked_file_sync(&f, sync_path);
        pin_mut!(stream);
        while let Some(file_sync) = stream.next().await {
            let file_sync = file_sync.context("Could not unpack FileSync from Stream")?;
            let response = tx.send(file_sync.clone()).await;
            match response {
                Err(e) => {
                    error!(
                        "There was an error with send data for file {}: {:?}",
                        file_sync.path, e
                    );
                    break;
                }
                _ => {
                    continue;
                }
            }
        }
    }

    drop(tx);
    Ok(())
}

fn start_watcher(p: PathBuf, config: &common::Config) -> JoinHandle<()> {
    let c = config.clone();
    tokio::spawn(async move {
        let (_watcher, mut rx) = match watcher::async_watch(p).await {
            Ok((w, rx)) => (w, rx),
            Err(e) => {
                error!("Error in creating file watcher: {:#}", e);
                return
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
}


fn calculate_change_score(event_kind: EventKind, config: &common::Config) -> u64 {
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

fn should_trigger_sync(score: u64, config: &common::Config) -> bool {
    score > config.sync_threshold
}

#[cfg(feature = "schedule-based")]
fn start_scheduler(config: &common::Config) -> JoinHandle<()> {
    let mut delay_interval =
        tokio::time::interval(tokio::time::Duration::from_secs(config.schedule_delay));
    tokio::spawn(async move {
        loop {
            QUEUE.lock().await.push_back(true);
            delay_interval.tick().await;
        }
    })
}

#[cfg(test)]
mod tests {
    use harmonic::harmonic::FileAction;

    use super::*;

    #[test]
    fn test_handle_response() {
        let response: ServerSyncStateResponse = ServerSyncStateResponse {
            sync_uuid: String::from("sample-uuid"),
            timestamp_micro: 12345,
            sync_plan: vec![
                FileAction {
                    path: String::from("/file_client_send.txt"),
                    direction: TransferDirection::ClientSend as i32,
                },
                FileAction {
                    path: String::from("/file_server_send.txt"),
                    direction: TransferDirection::ServerSend as i32,
                },
                FileAction {
                    path: String::from("/file_skip.txt"),
                    direction: TransferDirection::Skip as i32,
                },
            ],
        };
        let generated_actions = handle_response(response);

        assert_eq!(generated_actions.len(), 1);
        assert_eq!(generated_actions[0], PathBuf::from("/file_client_send.txt"));
    }

    #[test]
    fn test_handle_response_empty_plan() {
        let response: ServerSyncStateResponse = ServerSyncStateResponse {
            sync_uuid: String::from("sample-uuid"),
            timestamp_micro: 12345,
            sync_plan: Vec::new(),
        };
        let generated_actions = handle_response(response);

        assert_eq!(generated_actions.len(), 0);
    }

    #[test]
    fn test_handle_response_invalid_direction() {
        let response: ServerSyncStateResponse = ServerSyncStateResponse {
            sync_uuid: String::from("sample-uuid"),
            timestamp_micro: 12345,
            sync_plan: vec![
                FileAction {
                    path: String::from("/file_client_send.txt"),
                    direction: TransferDirection::ClientSend as i32,
                },
                FileAction {
                    path: String::from("/file_invalid_direction.txt"),
                    direction: 9,
                },
            ],
        };
        let generated_actions = handle_response(response);

        assert_eq!(generated_actions.len(), 1);
        assert_eq!(generated_actions[0], PathBuf::from("/file_client_send.txt"));
    }

    #[test]
    fn test_handle_response_complex_many_directions() {
        let response: ServerSyncStateResponse = ServerSyncStateResponse {
            sync_uuid: String::from("sample-uuid"),
            timestamp_micro: 12345,
            sync_plan: vec![
                FileAction {
                    path: String::from("/file_client_send_1.txt"),
                    direction: TransferDirection::ClientSend as i32,
                },
                FileAction {
                    path: String::from("/file_server_send.txt"),
                    direction: TransferDirection::ServerSend as i32,
                },
                FileAction {
                    path: String::from("/file_skip.txt"),
                    direction: TransferDirection::Skip as i32,
                },
                FileAction {
                    path: String::from("/file_client_send_2.txt"),
                    direction: TransferDirection::ClientSend as i32,
                },
                FileAction {
                    path: String::from("/file_client_send_3.txt"),
                    direction: TransferDirection::ClientSend as i32,
                },
                FileAction {
                    path: String::from("/file_client_send_2.txt"),
                    direction: 999,
                },
                FileAction {
                    path: String::from("/file_client_send_4.txt"),
                    direction: TransferDirection::ClientSend as i32,
                },
            ],
        };
        let generated_actions = handle_response(response);

        assert_eq!(generated_actions.len(), 4);
        // note this will probably fail if parallelizing iteration
        assert_eq!(
            generated_actions[0],
            PathBuf::from("/file_client_send_1.txt")
        );
        assert_eq!(
            generated_actions[1],
            PathBuf::from("/file_client_send_2.txt")
        );
        assert_eq!(
            generated_actions[2],
            PathBuf::from("/file_client_send_3.txt")
        );
        assert_eq!(
            generated_actions[3],
            PathBuf::from("/file_client_send_4.txt")
        );
    }

    #[test]
    fn test_calculate_change_score() {
        let config = common::Config::default();

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
        let config = common::Config::default();

        assert!(!should_trigger_sync(config.sync_threshold, &config));
        assert!(should_trigger_sync(config.sync_threshold + 1, &config));
        assert!(!should_trigger_sync(config.sync_threshold - 1, &config));
        assert!(!should_trigger_sync(0, &config));
    }
}
