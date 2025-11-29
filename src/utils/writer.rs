use std::{io::SeekFrom, path::PathBuf};

use tokio::{fs::File, sync::mpsc::{self, Sender}};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

use crate::{proto::{Delta, delta}, sync::state::BlockCache};

pub async fn delta_writer(path: &PathBuf, cache: BlockCache) -> Sender<Delta> { 
    let (tx, mut rx) = mpsc::channel::<Delta>(100);
    let path = path.clone();

    tokio::spawn(async move{
        let mut file = File::create(path).await?;

        while let Some(delta) = rx.recv().await {
            file.seek(SeekFrom::Start(delta.index)).await?;
            match delta.instruction {
                Some(delta::Instruction::BlockIndex(block_index)) => {
                    // send cached block index
                    file.write_all(&cache.blocks[block_index as usize]).await?;
        },
        Some(delta::Instruction::Literal(bytes)) => {
            file.write_all(&bytes).await?;
        },
        _ => unreachable!()
            }
        }

        file.flush().await?;
        Ok::<_, std::io::Error>(())
    });

    tx
}