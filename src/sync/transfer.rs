use std::path::{Path, PathBuf};
use tokio::fs::{File, OpenOptions};
use tokio::io::AsyncReadExt;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio_stream::Stream;
use tracing::{debug, info, instrument};

use crate::proto::FileSync;
use crate::utils::{HarmonicError, Result};

fn get_absolute_path(relative_path: &Path, sync_path: &Path) -> PathBuf {
    sync_path.join(relative_path)
}

pub async fn write_data_to_offset(data: FileSync, file: &mut File) -> Result<()> {
    file.seek(std::io::SeekFrom::Start(data.offset)).await?;

    file.write_all(&data.chunk).await?;

    Ok(())
}

pub async fn get_file(data: &FileSync, sync_path: &Path) -> Result<File> {
    let relative_path = PathBuf::from(&data.path);
    let absolute_path = get_absolute_path(&relative_path, sync_path);

    if let Some(parent_path) = absolute_path.parent() {
        tokio::fs::create_dir_all(parent_path).await?;
    }
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .open(absolute_path)
        .await?;

    file.set_len(data.file_size).await?;

    Ok(file)
}

#[instrument]
pub fn file_to_chunked_file_sync(
    relative_path: &PathBuf,
    sync_path: &Path,
) -> impl Stream<Item = Result<FileSync>> {
    let absolute_path = get_absolute_path(relative_path, sync_path);
    let relative_path_c = relative_path.clone();

    debug!(?absolute_path, "Writing to file");
    async_stream::stream! {
        let mut file = match tokio::fs::File::open(&absolute_path).await {
            Ok(f) => f,
            Err(e) => {
                yield Err(HarmonicError::Io(e));
                return
            }
        };

        let file_size = match file.metadata().await {
            Ok(m) => m.len(),
            Err(e) => {
                yield Err(HarmonicError::Io(e));
                return
            }
        };

        let path_str = match relative_path_c.to_str() {
            Some(s) => s.to_string(),
            None => {
                yield Err(HarmonicError::StringInvalid);
                return
            }
        };

        let mut buffer = vec![0u8; 8192];
        let mut offset = 0;

        debug!(%path_str, "Begin yielding chunks for file");

        loop {
            match file.read(&mut buffer).await {
            Ok(0) => break,
            Ok(n) => {
                yield Ok(FileSync {
                    path: path_str.clone(),
                    chunk: buffer[..n].to_vec(),
                    offset: offset,
                    is_final: false,
                    file_size: file_size,
                });
                offset += n as u64;
            },
            Err(e) => {
                yield Err(HarmonicError::Io(e));
                return
            }
            }
        }
        info!(%path_str, "Completed yielding chunks for file");

    }
}
