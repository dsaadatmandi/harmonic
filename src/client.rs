
use harmonic::harmonic_client::HarmonicClient;
use harmonic::{HarmonizePushRequest};
use tokio_stream::StreamExt;

pub mod common;


pub mod harmonic {
    tonic::include_proto!("harmonic");
}


fn main() {

    let config= common::load_config();

    let last_state = common::load_state(&config);

    // load last SyncState from disk ✅
    // generate new SyncState ✅
    // compare -> build into sync state struct ✅
    // for files which are different -> send hash and modified ts

    let now_state = common::generate_state(config.sync_path);

    let diffs = common::compare_states(last_state, now_state);

    

}


#[tokio::main]
async fn send_state_to_client() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = HarmonicClient::connect("http://[::1]:42069").await?;

    let request = tonic::Request::new(HarmonizePushRequest {
        request: "pls".to_string(),
    });

    let mut response = client.harmonize_server_stream_update(request).await?.into_inner();

    while let Some(r) = response.next().await {
        match r {
            Ok(reply) => {
                println!("received: {:?}", reply)
            }
            Err(e) => {
                println!("{:?}", e)
            }
        }
    }


    Ok(())
}