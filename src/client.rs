
use harmonic::harmonic_client::HarmonicClient;
use harmonic::{HarmonizePushRequest};
use tokio_stream::StreamExt;


pub mod harmonic {
    tonic::include_proto!("harmonic");
}



#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
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