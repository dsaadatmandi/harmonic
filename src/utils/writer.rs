use std::{io::SeekFrom, path::PathBuf};

use filetime::FileTime;
use tokio::fs;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::{
    fs::File,
    sync::mpsc::{self, Sender},
};
use tracing::{debug, error, info};

use crate::{
    proto::{Delta, delta},
    sync::state::BlockCache,
};

pub async fn delta_writer(
    path: &PathBuf,
    cache: BlockCache,
    modified_ts: prost_types::Timestamp,
) -> Sender<Delta> {
    let (tx, mut rx) = mpsc::channel::<Delta>(512);
    let path_clone = path.clone();

    tokio::spawn(async move {
        let mut tmp_file = path_clone.clone();
        let tmp_ext = tmp_file
            .extension()
            .and_then(|e| e.to_str())
            .map_or("tmp".to_string(), |e| format!("{}.tmp", e));
        tmp_file.set_extension(tmp_ext);

        let path_for_error = path_clone.clone();

        let result = async {
            let mut file = File::create(&tmp_file).await?;
            let mut delta_count = 0;

            while let Some(delta) = rx.recv().await {
                delta_count += 1;
                debug!("Processing delta #{} at index {}", delta_count, delta.index);

                file.seek(SeekFrom::Start(delta.index)).await?;
                match delta.instruction {
                    Some(delta::Instruction::BlockIndex(block_index)) => {
                        let block_size = cache.blocks[block_index as usize].len();
                        debug!("Writing block {} ({} bytes)", block_index, block_size);
                        file.write_all(&cache.blocks[block_index as usize]).await?;
                    }
                    Some(delta::Instruction::Literal(bytes)) => {
                        debug!("Writing literal ({} bytes)", bytes.len());
                        file.write_all(&bytes).await?;
                    }
                    _ => {
                        error!("Received delta with no instruction");
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "Delta has no instruction",
                        ));
                    }
                }
            }

            info!(
                "Flushing file after processing {} deltas: {:?}",
                delta_count, path_clone
            );
            file.flush().await?;
            info!("Successfully flushed file: {:?}", tmp_file);
            fs::rename(&tmp_file, &path_clone).await?;

            info!("Moved {:?} to {:?}", tmp_file, path_clone);

            let ft = FileTime::from_unix_time(modified_ts.seconds, modified_ts.nanos as u32);
            tokio::task::spawn_blocking(move || filetime::set_file_mtime(&path_clone, ft));

            Ok::<_, std::io::Error>(())
        }
        .await;

        if let Err(e) = result {
            error!("Delta writer error for {:?}: {}", path_for_error, e);
        }
    });

    tx
}
