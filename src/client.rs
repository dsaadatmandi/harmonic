use harmonic::ClientSyncState;
use harmonic::harmonic_client::HarmonicClient;
use log::info;
use tokio_stream::StreamExt;
use uuid::Uuid;

use crate::common::ChangeType;
use crate::harmonic::ServerSyncStateResponse;

pub mod common;

pub mod harmonic {
    tonic::include_proto!("harmonic");
}

const ADDR: &str = "http://[::1]:42069";

#[tokio::main]
async fn main() {
    let sync_uuid = Uuid::new_v4();

    let config = common::load_config();

    let last_state = common::load_state(&config);

    // load last SyncState from disk ✅
    // generate new SyncState ✅
    // compare -> build into sync state struct ✅
    // for files which are different -> send hash and modified ts

    let now_state = common::generate_state(config.sync_path);

    let diffs = common::compare_states(&last_state, &now_state);

    if diffs.len() == 0 {
        info!("No updates to push");
        ()
    }

    let response = send_state_to_server(&sync_uuid, last_state.last_sync_timestamp_micros, diffs)
        .await
        .expect("Error awaiting response from server to sync intiation.");
}

async fn send_state_to_server(
    sync_uuid: &Uuid,
    last_sync_timestamp: i64,
    diffs: Vec<common::Diff>,
) -> Result<ServerSyncStateResponse, Box<dyn std::error::Error>> {
    let mut client = HarmonicClient::connect(ADDR).await?;

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
    // while let Some(r) = response.next().await {
    //     match r {
    //         Ok(reply) => {
    //             println!("received: {:?}", reply)
    //         }
    //         Err(e) => {
    //             println!("{:?}", e)
    //         }
    //     }
    // }
}
