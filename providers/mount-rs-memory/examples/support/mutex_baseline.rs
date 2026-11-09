// Exact block-store implementation and identity function from 5e7e381404cd15d7779e22751d348185132919d6.
use async_trait::async_trait;
use mount_rs_core::storage::{BlockId, BlockStore};
use mount_rs_core::{ErrorCode, FsError, Result};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};
/// Volatile immutable block store. A clone shares the same block map.
#[derive(Debug, Clone, Default)]
pub struct MemoryBlockStore {
    blocks: Arc<Mutex<BTreeMap<BlockId, Vec<u8>>>>,
}

impl MemoryBlockStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock_blocks(&self) -> Result<MutexGuard<'_, BTreeMap<BlockId, Vec<u8>>>> {
        self.blocks
            .lock()
            .map_err(|_| FsError::new(ErrorCode::Eio).with_message("block store lock poisoned"))
    }
}

#[async_trait]
impl BlockStore for MemoryBlockStore {
    fn durable(&self) -> bool {
        false
    }

    async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
        let base_id = block_id(bytes);
        let mut id = base_id.clone();
        let mut collision = 0_u64;
        let mut blocks = self.lock_blocks()?;
        loop {
            match blocks.get(&id) {
                Some(existing) if existing == bytes => return Ok(id),
                Some(_) => {
                    collision = collision
                        .checked_add(1)
                        .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
                    id = BlockId(format!("{}-{collision}", base_id.0));
                }
                None => {
                    blocks.insert(id.clone(), bytes.to_vec());
                    return Ok(id);
                }
            }
        }
    }

    async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
        let blocks = self.lock_blocks()?;
        blocks
            .get(id)
            .cloned()
            .ok_or_else(|| FsError::new(ErrorCode::Enoent).with_message("block not found"))
    }

    async fn flush(&self) -> Result<()> {
        Ok(())
    }

    async fn delete(&self, id: &BlockId) -> Result<()> {
        let mut blocks = self.lock_blocks()?;
        if blocks.remove(id).is_some() {
            Ok(())
        } else {
            Err(FsError::new(ErrorCode::Enoent).with_message("block not found"))
        }
    }
}

fn block_id(bytes: &[u8]) -> BlockId {
    const OFFSETS: [u64; 4] = [
        0xcbf29ce484222325,
        0x84222325cbf29ce4,
        0x9e3779b185ebca87,
        0xd6e8feb86659fd93,
    ];
    const PRIMES: [u64; 4] = [0x100000001b3, 0x100000001b3, 0x100000001b3, 0x100000001b3];
    let mut lanes = OFFSETS;
    for (index, byte) in bytes.iter().copied().enumerate() {
        let index = u64::try_from(index).unwrap_or(u64::MAX);
        for (lane, value) in lanes.iter_mut().enumerate() {
            let lane_value = u64::try_from(lane).unwrap_or(u64::MAX);
            *value ^= u64::from(byte)
                .wrapping_add(index.wrapping_mul(0x9e37_79b9_7f4a_7c15 ^ lane_value));
            *value = value
                .wrapping_mul(PRIMES[lane])
                .rotate_left((lane as u32).wrapping_mul(11).wrapping_add(5));
        }
    }
    let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    for (lane, value) in lanes.iter_mut().enumerate() {
        let lane_value = u64::try_from(lane).unwrap_or(u64::MAX);
        *value ^= length.wrapping_add(lane_value.wrapping_mul(0x517c_c1b7_2722_0a95));
        *value = value
            .wrapping_mul(PRIMES[lane])
            .rotate_left((lane as u32).wrapping_mul(13).wrapping_add(7));
    }
    BlockId(format!(
        "mem-{length:016x}-{:016x}{:016x}{:016x}{:016x}",
        lanes[0], lanes[1], lanes[2], lanes[3]
    ))
}
