use std::path::Path;

use chrono::prelude::*;

use futures::StreamExt;
use notify::EventKind;
use tokio::fs::File;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use harmonic::harmonic_server::{Harmonic, HarmonicServer};
use harmonic::{FileSync, StatusResponse, UpdateStrategy};
use tonic::{Request, Response, Status, Streaming, transport::Server};
use log::{error, info};

mod common;
mod watcher;

pub mod harmonic {
    tonic::include_proto!("harmonic");
}

#[derive(Default, Debug)]
pub struct HarmonicService {}

#[tonic::async_trait]
impl Harmonic for HarmonicService {
    type HarmonizeSynchronizeStateStream = ReceiverStream<Result<FileSync, Status>>;


    async fn harmonize_client_initiate_sync() {
        
    }
    async fn harmonize_synchronize_state(
        &self,
        request: Request<Streaming<FileSync>>,
    ) -> Result<Response<Self::HarmonizeSynchronizeStateStream>, Status> {
        let mut request_stream = request.into_inner();
        let (tx, rx) = mpsc::channel(10);

        tokio::spawn(async move {
            let mut cur_file: String = Default::default();
            let mut file_currently_writing: Option<File> = None;
            while let Some(request) = request_stream.next().await {
                match request {
                    Ok(msg) => {
                        let path = msg.path.clone();
                        info!("Received data for file {}. Writing to path...", path);

                        if file_currently_writing.is_none() || cur_file != path {
                            file_currently_writing = Some(common::get_file(&msg).await);
                            cur_file = path;
                        }

                        common::write_data_to_offset(msg, file_currently_writing.as_mut().unwrap())
                            .await;
                    }
                    Err(e) => {
                        error!("Error in response stream from server: {:?}", e);
                        break;
                    }
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
            match event.kind {
                EventKind::Modify(_) => println!("Modification event to {:?}", event.paths),
                EventKind::Remove(_) => println!("Remove event to {:?}", event.paths),
                EventKind::Create(_) => println!("Create event to {:?}", event.paths),
                _ => println!(
                    "Unmatched event of type {:?} to {:?}",
                    event.kind, event.paths
                ),
            }
        }
    });

    Server::builder()
        .add_service(HarmonicServer::new(harmonic))
        .serve(address)
        .await?;

    Ok(())
}
