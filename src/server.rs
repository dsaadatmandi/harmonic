use std::path::Path;

use chrono::prelude::*;

use futures::StreamExt;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use harmonic::harmonic_server::{Harmonic, HarmonicServer};
use harmonic::{
    HarmonizePush, HarmonizePushResponse, StatusRequest, StatusResponse, UpdateStrategy,
};
use tonic::{Request, Response, Status, Streaming, transport::Server};

use crate::harmonic::HarmonizePushRequest;

mod watcher;

pub mod harmonic {
    tonic::include_proto!("harmonic");
}

#[derive(Default, Debug)]
pub struct HarmonicService {}

#[tonic::async_trait]
impl Harmonic for HarmonicService {
    type HarmonizeServerStreamUpdateStream = ReceiverStream<Result<HarmonizePush, Status>>;

    async fn harmonize_status(
        &self,
        request: Request<StatusRequest>,
    ) -> Result<Response<StatusResponse>, Status> {
        println!("Got a request: {:?}", request);

        let now = Utc::now().timestamp_micros();

        let reply = StatusResponse {
            timestamp_micro: now,
            strategy: UpdateStrategy::Skip.into(),
        };

        Ok(Response::new(reply))
    }

    async fn harmonize_update(
        &self,
        request: Request<HarmonizePush>,
    ) -> Result<Response<HarmonizePushResponse>, Status> {
        println!("Got request: {:?}", request);

        let reply = HarmonizePushResponse {
            output: "this is a response to push".to_string(),
        };

        Ok(Response::new(reply))
    }

    async fn harmonize_client_stream_update(
        &self,
        request: Request<Streaming<HarmonizePush>>,
    ) -> Result<Response<HarmonizePushResponse>, Status> {
        println!("Got request: {:?}", request);

        let reply = HarmonizePushResponse {
            output: "this is a response to stream push".to_string(),
        };

        Ok(Response::new(reply))
    }

    async fn harmonize_server_stream_update(
        &self,
        request: Request<HarmonizePushRequest>,
    ) -> Result<Response<Self::HarmonizeServerStreamUpdateStream>, Status> {
        let rq = request.into_inner().request;

        let (tx, rx) = mpsc::channel(10);

        tokio::spawn(async move {
            let response_arr = [
                format!("part 1 for {}", rq),
                format!("part 2 for {}", rq),
                format!("part 3 for {}", rq),
            ];

            for response in response_arr {
                if tx
                    .send(Ok(HarmonizePush { input: response }))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let address = "[::1]:42069"
        .parse()
        .expect("Somehow could not parse address..?");
    let harmonic = HarmonicService::default();

    let p = Path::new("/Users/milad/code/harmonic/test");

    tokio::spawn(async move {
        let (_watcher, mut rx) = watcher::async_watch(p).await.unwrap();

        while let Some(Ok(event)) = rx.next().await {
            println!("event: {:?}, kind: {:?}", event.kind, event.paths)
        }

    });

    Server::builder()
        .add_service(HarmonicServer::new(harmonic))
        .serve(address)
        .await?;

    Ok(())
}
