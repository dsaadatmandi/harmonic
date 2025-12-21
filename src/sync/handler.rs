use futures::{Sink, SinkExt};
use std::path::PathBuf;
use tokio::sync::mpsc::Sender;
use anyhow::Result;
use tracing::{debug, info, instrument};

use crate::proto::{Delta, SyncRequest, TransferDirection, sync_request};
use crate::sync::Config;
use crate::sync::transfer::{send_block_signatures_for_file, send_delta_from_block_signatures};
use crate::utils::writer::delta_writer;
use crate::utils::HarmonicError;

pub enum SyncStatus {
    Continue,
    Completed,
}

#[instrument(skip(payload, tx, file_path, config, writer_tx), fields(payload_type = ?std::mem::discriminant(&payload)))]
pub async fn handle_sync_payload<S>(
    payload: sync_request::Payload,
    mut tx: S,
    file_path: &mut PathBuf,
    config: Config,
    writer_tx: &mut Option<Sender<Delta>>,
) -> Result<SyncStatus> 
where
    S: Sink<SyncRequest, Error = HarmonicError> + Unpin + Send,
{
    match payload {
        sync_request::Payload::FileAction(file_action) => {
            match file_action.direction {
                d if d == TransferDirection::Download as i32 => {
                    *file_path = PathBuf::from(file_action.path);
                }
                d if d == TransferDirection::Upload as i32 => {
                    *file_path = PathBuf::from(file_action.path);
                    let cache = send_block_signatures_for_file(file_path, &mut tx, &config).await?;
                    let abs_path = crate::sync::transfer::get_absolute_path(file_path, &config.sync_path)?;
                    *writer_tx = Some(delta_writer(&abs_path, cache, file_action.timestamp_latest_modified.unwrap_or_default()).await)
                }
                _ => {}
            }
        },
        sync_request::Payload::Signatures(signatures) => {
            let abs_path = crate::sync::transfer::get_absolute_path(file_path, &config.sync_path)?;
            send_delta_from_block_signatures(&abs_path, signatures, &mut tx).await?;
        },
        sync_request::Payload::Delta(delta) => {
            if let Some(tx) = writer_tx {
                debug!("Sending delta to writer_tx: index={}, has_instruction={}", delta.index, delta.instruction.is_some());
                tx.send(delta).await.map_err(|e| HarmonicError::SendError(e.to_string()))?;
            } else {
                debug!("Received delta but writer_tx is None!");
            }
        },
        sync_request::Payload::Complete(_complete) => {
            info!("Received Complete message, sending Complete response back");
            tx.send(SyncRequest {
                payload: Some(sync_request::Payload::Complete(true)),
            }).await?;
            return Ok(SyncStatus::Completed)
        },
    }

    Ok(SyncStatus::Continue)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{FileAction, TransferDirection};
    use futures::channel::mpsc;
    use futures::{SinkExt};

    #[tokio::test]
    async fn test_handle_sync_payload_server_request_file() {
        let (tx, _rx) = mpsc::channel(1);
        let mut sink = tx.sink_map_err(|e| HarmonicError::SendError(e.to_string()));
        
        let mut file_path = PathBuf::new();
        let config = Config::default();
        let mut writer_tx = None;

        let payload = sync_request::Payload::FileAction(FileAction {
            path: "test/path".to_string(),
            direction: TransferDirection::Download as i32,
            timestamp_latest_modified: Default::default(),
        });

        let result = handle_sync_payload(
            payload,
            &mut sink,
            &mut file_path,
            config,
            &mut writer_tx,
        ).await;

        assert!(result.is_ok());
        assert_eq!(file_path, PathBuf::from("test/path"));
    }

    #[tokio::test]
    async fn test_handle_sync_payload_delta() {
        let (tx, _rx) = mpsc::channel(1);
        let mut sink = tx.sink_map_err(|e| HarmonicError::SendError(e.to_string()));
        
        let mut file_path = PathBuf::new();
        let config = Config::default();
        
        let (writer_tx_sender, mut writer_rx) = tokio::sync::mpsc::channel(1);
        let mut writer_tx = Some(writer_tx_sender);

        let payload = sync_request::Payload::Delta(Delta {
            index: 0,
            instruction: None,
        });

        let result = handle_sync_payload(
            payload,
            &mut sink,
            &mut file_path,
            config,
            &mut writer_tx,
        ).await;

        assert!(result.is_ok());
        
        // Verify delta was sent to writer_tx
        let received = writer_rx.recv().await;
        assert!(received.is_some());
        assert_eq!(received.unwrap().index, 0);
    }
}
