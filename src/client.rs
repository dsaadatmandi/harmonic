use std::error::Error;
use std::path::PathBuf;

use futures::pin_mut;
use harmonic::ClientSyncState;
use harmonic::harmonic_client::HarmonicClient;
use log::{error, info};
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_stream::{Stream, StreamExt};
use tonic::transport::Channel;
use uuid::Uuid;

use crate::common::ChangeType;
use crate::harmonic::{FileSync, ServerSyncStateResponse, UpdateStrategy};

pub mod common;

pub mod harmonic {
    tonic::include_proto!("harmonic");
}

const ADDR: &str = "http://[::1]:42069";
const ROOT_PATH: &str = "/opt/sync";

#[tokio::main]
async fn main() {
    let sync_uuid = Uuid::new_v4();
    let config = common::load_config();
    let mut client = HarmonicClient::connect(ADDR)
        .await
        .expect("Error in awaiting client creation.");
    let last_state = common::load_state(&config);
    let now_state = common::generate_state(&config.sync_path);
    let diffs = common::compare_states(&last_state, &now_state);

    if diffs.len() == 0 {
        info!("No updates to push");
        ()
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

    let result = send_data_to_server(client.clone(), files_to_send).await;
    match result {
        Ok(()) => info!("Completed Sync"),
        Err(e) => error!("Sync failed due to: {:?}", e),
    };

    common::save_state(now_state, &config);
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
        .strategy
        .into_iter()
        .filter_map(
            |strat| match UpdateStrategy::try_from(strat.strategy).ok()? {
                UpdateStrategy::ClientSend => Some(PathBuf::from(strat.path)),
                _ => None,
            },
        )
        .collect()
}

async fn send_data_to_server(
    mut client: HarmonicClient<Channel>,
    files: Vec<PathBuf>,
) -> Result<(), Box<dyn Error>> {
    let (tx, rx) = tokio::sync::mpsc::channel(10);
    let out = tokio_stream::wrappers::ReceiverStream::new(rx);
    let mut inc = client
        .harmonize_synchronize_date(tonic::Request::new(out))
        .await?
        .into_inner();

    let send_task = tokio::spawn(async move {
        for f in files {
            let stream = file_to_chunked_file_sync(&f);
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

    let mut cur_file = String::new();
    let mut file_currently_writing: Option<File> = None;
    while let Some(response) = inc.next().await {
        match response {
            Ok(msg) => {
                let path = msg.path.as_str();
                info!("Received data from server for file {}", path);
                if cur_file != path {
                    file_currently_writing = Some(
                        tokio::fs::File::create(path)
                            .await
                            .expect("Unable to create file to write data to"),
                    );
                    cur_file = path.to_string();
                }

                if let Some(f) = file_currently_writing.as_mut() {
                    f.write_all(&msg.chunk)
                        .await
                        .expect("Unable to write data to file")
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

fn file_to_chunked_file_sync(path: &PathBuf) -> impl Stream<Item = FileSync> {
    async_stream::stream! {
        let mut file = tokio::fs::File::open(&path).await.unwrap();
        let mut buffer = vec![0u8; 8192];
        let mut offset = 0;

        while let Ok(n) = file.read(&mut buffer).await {
            if n == 0 { break; }

            yield FileSync {
                sync_uuid: "TBD".to_string(),
                path: path.to_str().expect("Could not convert PathBuf to string slice").to_string(),
                chunk: buffer[..n].to_vec(),
                offset: offset,
                is_final: false,
            };
            offset += n as i64;
        }
    }
}
