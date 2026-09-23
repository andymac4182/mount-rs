use std::collections::BTreeMap;

use mount_rs_core::storage::{
    BlockId, BlockStore, ConcurrentBackingId, ConcurrentModeState, MetadataStore, NodeData,
};
use mount_rs_core::{ErrorCode, FsError, Result};

/// Verify an offline MRC1 namespace and bind its selected block authority.
pub async fn migrate_mrc1_backing<M: MetadataStore, B: BlockStore>(
    metadata: &M,
    blocks: &B,
    expected_revision: u64,
) -> Result<ConcurrentBackingId> {
    if metadata.concurrent_mode_state().await? != ConcurrentModeState::Mrc1 {
        return Err(FsError::new(ErrorCode::Ebusy).with_syscall("migrate MRC1 backing"));
    }
    let loaded = metadata.load().await?;
    loaded.validate()?;
    if loaded.revision != expected_revision {
        return Err(FsError::new(ErrorCode::Eagain).with_syscall("migrate MRC1 backing"));
    }

    let backing = blocks.prepare_concurrent_backing().await?;
    let mut required = BTreeMap::<BlockId, u64>::new();
    if let Some(namespace) = &loaded.namespace {
        for node in namespace.nodes.values() {
            if let NodeData::File(layout) = &node.data {
                for extent in &layout.extents {
                    let end = extent
                        .block_offset
                        .checked_add(extent.length)
                        .ok_or_else(|| {
                            FsError::new(ErrorCode::Eoverflow)
                                .with_syscall("verify MRC1 block extent")
                        })?;
                    let prior = required.entry(extent.block.clone()).or_default();
                    *prior = (*prior).max(end);
                }
            }
        }
    }
    for (id, end) in required {
        let bytes = blocks.get_for_migration(&id).await?;
        let available = u64::try_from(bytes.len()).map_err(|_| {
            FsError::new(ErrorCode::Eoverflow).with_syscall("verify MRC1 block extent")
        })?;
        if available < end {
            return Err(FsError::new(ErrorCode::Eio)
                .with_syscall("verify MRC1 block extent")
                .with_message(format!(
                    "block {} needs {end} bytes but backing returned {available}",
                    id.0
                )));
        }
    }
    blocks.verify_concurrent_backing(backing).await?;
    metadata
        .migrate_mrc1_to_bound_mode(backing, expected_revision)
        .await?;
    Ok(backing)
}
