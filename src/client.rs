use std::collections::VecDeque;
use std::error::Error;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Mutex;
use futures::pin_mut;
use log::{error, info};
use notify::EventKind;
use once_cell::sync::Lazy;
use tokio::fs::File;
use tokio::task::JoinHandle;
use tokio_stream::{StreamExt};
use tonic::transport::Channel;
use uuid::Uuid;

use harmonic::common;
use harmonic::watcher;
use harmonic::harmonic::{ServerSyncStateResponse, TransferDirection,
    ClientSyncState, harmonic_client::HarmonicClient };


const QUEUE_CHECK_SEC_INTERVAL_SEC: u64 = 10;

static QUEUE: Lazy<Arc<Mutex<VecDeque<bool>>>> = Lazy::new(|| {
    Arc::new(Mutex::new(VecDeque::new()))
});

#[tokio::main]
async fn main() {
    env_logger::init();
    let config = common::load_config();
    let p = PathBuf::from(config.sync_path);
    
    #[cfg(feature = "event-based")]
    let _watcher_task = start_watcher(p);

    #[cfg(feature = "schedule-based")]
    let _scheduler_task = start_scheduler(config);

    let mut queue_check_interval = tokio::time::interval(tokio::time::Duration::from_secs(QUEUE_CHECK_SEC_INTERVAL_SEC));

    loop {
        let mut queue = QUEUE.lock().await;
        if let Some(_) = queue.pop_front() {
            queue.clear();
            drop(queue);
            trigger_sync().await;
        }
        queue_check_interval.tick().await;
    }
}


async fn trigger_sync() -> JoinHandle<()> {
    tokio::spawn(async move {
        let sync_uuid = Uuid::new_v4();
        let config = common::load_config();
        let client = HarmonicClient::connect(config.socket_addr)
            .await
            .expect("Error in awaiting client creation.");
        let last_state = common::load_state();
        let now_state = common::generate_state(&config.sync_path);
        let diffs = common::compare_states(&last_state, &now_state);

        if diffs.is_empty() {
            info!("No updates to push");
            return
        }

        let response = send_state_to_server(
            &sync_uuid,
            last_state.last_sync_timestamp_micros,
            diffs,
            client.clone(),
        )
        .await
        .expect("Error awaiting response from server to sync intiation.");

        let files_to_send = handle_response(response);

        let result = send_data_to_server(client.clone(), files_to_send, &sync_uuid).await;
        match result {
            Ok(()) => info!("Completed Sync"),
            Err(e) => error!("Sync failed due to: {:?}", e),
        };

        common::save_state(now_state);
    })

}

async fn send_state_to_server(
    sync_uuid: &Uuid,
    last_sync_timestamp: i64,
    diffs: Vec<common::Diff>,
    mut client: HarmonicClient<Channel>,
) -> Result<ServerSyncStateResponse, Box<dyn Error>> {
    let request = tonic::Request::new(ClientSyncState {
        sync_uuid: sync_uuid.to_string(),
        timestamp_last_sync_micro: last_sync_timestamp,
        status_list: diffs.into_iter().map(Into::into).collect(),
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
    sync_uuid: &Uuid
) -> Result<(), Box<dyn Error>> {
    let (tx, rx) = tokio::sync::mpsc::channel(10);
    let out = tokio_stream::wrappers::ReceiverStream::new(rx);
    let mut request = tonic::Request::new(out);
    request.metadata_mut().insert(
        "session-uuid",
        sync_uuid.to_string().parse().unwrap());
    let mut inc = client
        .harmonize_synchronize_state(request)
        .await?
        .into_inner();

    let send_task = tokio::spawn(async move {
        for f in files {
            let stream = common::file_to_chunked_file_sync(&f);
            pin_mut!(stream);
            while let Some(file_sync) = stream.next().await {
                let response = tx.send(file_sync.clone()).await;
                match response {
                    Err(e) => {
                        error!(
                            "There was an error with send data for file {}: {:?}",
                            file_sync.path, e
                        );
                        break;
                    }
                    _ => continue,
                }
            }
        }
        drop(tx);
    });

    let mut cur_file: String = Default::default();
    let mut file_currently_writing: Option<File> = None;
    while let Some(response) = inc.next().await {
        match response {
            Ok(msg) => {
                let path = msg.path.clone();
                info!("Received data for file {}. Writing to path...", path);

                if file_currently_writing.is_none() || cur_file != path {
                    file_currently_writing = Some(common::get_file(&msg).await);
                    cur_file = path;
                }

                common::write_data_to_offset(msg, file_currently_writing.as_mut().unwrap()).await;

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

fn start_watcher(p: PathBuf) -> JoinHandle<()> {
    tokio::spawn(async move {
            let (_watcher, mut rx) = watcher::async_watch(p).await.unwrap();

            let mut points: u64 = 0;

            while let Some(Ok(event)) = rx.next().await {
                match event.kind {
                    EventKind::Modify(_) => {
                        info!("Modification event to {:?}", event.paths);
                        points += 1;
                    },
                    EventKind::Remove(_) => {
                        info!("Remove event to {:?}", event.paths);
                        points += 5;
                    },
                    EventKind::Create(_) => {
                        info!("Create event to {:?}", event.paths);
                        points += 10;
                    },
                    _ => info!(
                        "Unmatched event of type {:?} to {:?}",
                        event.kind, event.paths
                    ),
                }
                if points > 20 {
                    info!("Sufficient changes accrued. Creating sync job.");
                    QUEUE.lock().await.push_back(true);
                    points = 0;
                }
            }
        })
}

#[cfg(feature = "schedule-based")]
fn start_scheduler(config: &common::Config) -> JoinHandle<()> {
    let mut delay_interval = tokio::time::interval(tokio::time::Duration::from_secs(config.schedule_delay));
    tokio::spawn(async move {
        loop {
            QUEUE.lock().await.push_back(true);
            delay_interval.tick().await;
        }
    })
}