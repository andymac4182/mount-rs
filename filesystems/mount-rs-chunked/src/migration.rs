use std::collections::BTreeMap;

use mount_rs_core::storage::{
    BlockId, BlockStore, ConcurrentBackingId, ConcurrentModeState, MetadataStore, NodeData,
};
use mount_rs_core::versioning::VolumeId;
use mount_rs_core::{ErrorCode, FsError, Result};

enum Mrc1Transition {
    Ordinary,
    TrustedUnstamped(VolumeId),
}

/// Verify an offline MRC1 namespace and bind its selected block authority.
pub async fn migrate_mrc1_backing<M: MetadataStore, B: BlockStore>(
    metadata: &M,
    blocks: &B,
    expected_revision: u64,
) -> Result<ConcurrentBackingId> {
    migrate_mrc1_backing_inner(
        metadata,
        blocks,
        expected_revision,
        Mrc1Transition::Ordinary,
    )
    .await
}

/// Explicit recovery of an old unstamped SQLite metadata file. The caller
/// must assert that every writer is stopped and this is the sole authoritative
/// metadata copy. The expected volume ID cannot prove those conditions.
pub async fn migrate_trusted_unstamped_mrc1_backing<M: MetadataStore, B: BlockStore>(
    metadata: &M,
    blocks: &B,
    expected_revision: u64,
    expected_volume: VolumeId,
) -> Result<ConcurrentBackingId> {
    migrate_mrc1_backing_inner(
        metadata,
        blocks,
        expected_revision,
        Mrc1Transition::TrustedUnstamped(expected_volume),
    )
    .await
}

async fn migrate_mrc1_backing_inner<M: MetadataStore, B: BlockStore>(
    metadata: &M,
    blocks: &B,
    expected_revision: u64,
    transition: Mrc1Transition,
) -> Result<ConcurrentBackingId> {
    if metadata.concurrent_mode_state().await? != ConcurrentModeState::Mrc1 {
        return Err(FsError::new(ErrorCode::Ebusy).with_syscall("migrate MRC1 backing"));
    }
    let loaded = metadata.load().await?;
    loaded.validate()?;
    if loaded.revision != expected_revision {
        return Err(FsError::new(ErrorCode::Eagain).with_syscall("migrate MRC1 backing"));
    }
    match &transition {
        Mrc1Transition::Ordinary => {
            metadata
                .preflight_mrc1_to_bound_mode(expected_revision)
                .await?
        }
        Mrc1Transition::TrustedUnstamped(volume) => {
            metadata
                .preflight_trusted_unstamped_mrc1(expected_revision, volume.clone())
                .await?
        }
    }
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
    let backing = blocks.prepare_concurrent_backing().await?;
    blocks.verify_concurrent_backing(backing).await?;
    match transition {
        Mrc1Transition::Ordinary => {
            metadata
                .migrate_mrc1_to_bound_mode(backing, expected_revision)
                .await?
        }
        Mrc1Transition::TrustedUnstamped(volume) => {
            metadata
                .migrate_trusted_unstamped_mrc1(backing, expected_revision, volume)
                .await?
        }
    }
    Ok(backing)
}
