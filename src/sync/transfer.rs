use path_clean::PathClean;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{info, instrument};

use crate::Config;
use crate::proto::{BlockSignatures, Delta, SyncRequest, delta, sync_request};
use crate::sync::state::{BlockCache, generate_blocks_signatures};
use crate::utils::{HarmonicError, Result, hash};

pub fn get_absolute_path(relative_path: &Path, sync_path: &Path) -> Result<PathBuf> {
    if relative_path.is_absolute() {
        return Err(HarmonicError::PathError {
            path: relative_path.to_path_buf(),
        });
    }

    let cleaned = relative_path.clean();

    if cleaned.starts_with("..") {
        return Err(HarmonicError::PathError {
            path: relative_path.to_path_buf(),
        });
    }

    let abs = sync_path.join(cleaned);
    if !abs.starts_with(sync_path) {
        return Err(HarmonicError::PathIntegrityError {
            path: abs,
            sync_path: sync_path.to_path_buf(),
        });
    }

    Ok(abs)
}

use futures::{Sink, SinkExt};

#[instrument(skip(tx, config), fields(file_path = %file_path.display()))]
pub async fn send_block_signatures_for_file<S>(
    file_path: &PathBuf,
    tx: &mut S,
    config: &Config,
) -> Result<BlockCache>
where
    S: Sink<SyncRequest, Error = HarmonicError> + Unpin + Send,
{
    // sender always decides block size based on config and sends
    let (sig, cache) = generate_blocks_signatures(file_path, &config)
        .await
        .map_err(|e| HarmonicError::SendError(e.to_string()))?;
    tx.send(SyncRequest {
        payload: Some(sync_request::Payload::Signatures(sig)),
    })
    .await?;

    Ok(cache)
}

#[instrument(skip(tx, block_signatures), fields(file_path = %file_path.display(), num_blocks = block_signatures.blocks.len()))]
pub async fn send_delta_from_block_signatures<S>(
    file_path: &PathBuf,
    block_signatures: BlockSignatures,
    tx: &mut S,
) -> Result<()>
where
    S: Sink<SyncRequest, Error = HarmonicError> + Unpin + Send,
{
    info!("Generating delta for block signatures");

    let map: HashMap<u64, HashMap<[u8; 32], u32>> = block_signatures
        .blocks
        .iter()
        .enumerate()
        .fold(HashMap::new(), |mut acc, (index, b)| {
            if let Ok(strong) = b.strong_checksum[..].try_into() {
                acc.entry(b.weak_checksum)
                    .or_insert_with(HashMap::new)
                    .insert(strong, index as u32);
            }
            acc
        });

    let data = tokio::fs::read(file_path).await?;
    let mut buffer: Vec<u8> = Vec::new();
    let mut buffer_start_index: u64 = 0;
    let block_size = block_signatures.block_size as usize;

    let mut i = 0;

    let mut buz_hasher = hash::BuzHash::new(block_size);
    let data_slice: &[u8];
    if block_size >= data.len() {
        data_slice = &data;
    } else {
        data_slice = &data[0..block_size];
    }
    buz_hasher.compute(data_slice);

    info!("Starting loop to generate deltas from block signatures");

    loop {
        if i + block_size <= data.len() {
            let data_slice = &data[i..i + block_size];
            if let Some(strong_checksum) = map.get(&buz_hasher.hash) {
                let strong_hash = blake3::hash(data_slice);
                if let Some(index) = strong_checksum.get(strong_hash.as_bytes()) {
                    if !buffer.is_empty() {
                        tx.send(SyncRequest {
                            payload: Some(sync_request::Payload::Delta(Delta {
                                index: buffer_start_index,
                                instruction: Some(delta::Instruction::Literal(buffer.clone())),
                            })),
                        })
                        .await?;
                        buffer.clear();
                    }

                    tx.send(SyncRequest {
                        payload: Some(sync_request::Payload::Delta(Delta {
                            index: i as u64,
                            instruction: Some(delta::Instruction::BlockIndex(*index)),
                        })),
                    })
                    .await?;

                    i += block_size;
                    if i + block_size <= data.len() {
                        buz_hasher.compute(&data[i..i + block_size]);
                    }

                    continue;
                }
            }
        }

        if i >= data.len() {
            break;
        }

        // If buffer is empty, record where this literal sequence starts
        if buffer.is_empty() {
            buffer_start_index = i as u64;
        }

        buffer.push(data[i]);
        i += 1;

        if i + block_size <= data.len() {
            buz_hasher.roll(data[i + block_size - 1]);
        }
    }

    if !buffer.is_empty() {
        tx.send(SyncRequest {
            payload: Some(sync_request::Payload::Delta(Delta {
                index: buffer_start_index,
                instruction: Some(delta::Instruction::Literal(buffer)),
            })),
        })
        .await?;
    }

    info!("Completed sending deltas for block signatures");

    tx.send(SyncRequest {
        payload: Some(sync_request::Payload::Complete(true)),
    })
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_absolute_path_traversal() {
        let sync_path = PathBuf::from("/tmp/sync");

        // Abs path traversal
        let malicious_abs = PathBuf::from("/etc/passwd");
        let result_abs = get_absolute_path(&malicious_abs, &sync_path);
        assert!(result_abs.is_err(), "Absolute path traversal should fail!");

        // Rel path traversal
        let malicious_rel = PathBuf::from("../../etc/passwd");
        let result_rel = get_absolute_path(&malicious_rel, &sync_path);
        assert!(result_rel.is_err(), "Relative path traversal should fail!");
    }

    #[tokio::test]
    async fn test_generate_delta_panic_repro() {
        use crate::proto::{BlockSignature, BlockSignatures};
        use std::io::Write;

        // Create a temp file
        let mut temp_file = tempfile::NamedTempFile::new().unwrap();
        let block_size = 4;
        // Data len 9. Block size 4.
        // Block 1: [0, 1, 2, 3]
        // Block 2: [4, 5, 6, 7]
        // Tail: [8]
        let data: Vec<u8> = vec![0, 1, 2, 3, 4, 5, 6, 7, 8];
        temp_file.write_all(&data).unwrap();
        let file_path = temp_file.path().to_path_buf();

        // Create block signatures that match the second block [4, 5, 6, 7]
        let mut buz_hasher = crate::utils::hash::BuzHash::new(block_size);
        let weak = buz_hasher.compute(&data[4..8]);
        let strong = blake3::hash(&data[4..8]);

        let block_sig = BlockSignature {
            weak_checksum: weak,
            strong_checksum: strong.as_bytes().to_vec(),
        };

        let signatures = BlockSignatures {
            block_size: block_size as u64,
            blocks: vec![block_sig],
        };

        let (tx, _rx) = futures::channel::mpsc::channel(10);
        let mut sink = tx.sink_map_err(|e| HarmonicError::SendError(e.to_string()));

        // This should panic if the bug exists
        let result = send_delta_from_block_signatures(&file_path, signatures, &mut sink).await;

        assert!(result.is_ok());
    }
}
