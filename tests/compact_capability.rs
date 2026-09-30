use mount_rs_core::storage::{MetadataStore, compact::CompactInodeCapability};

#[test]
fn existing_provider_does_not_opt_into_compact_layout() {
    let store = mount_rs_memory::MemoryMetadataStore::new();
    assert_eq!(
        store.compact_inode_capability(),
        CompactInodeCapability::Unsupported
    );
    assert_eq!(
        CompactInodeCapability::default(),
        CompactInodeCapability::Unsupported
    );
}

#[tokio::test]
async fn existing_provider_rejects_compact_operational_bridge() {
    use mount_rs_core::{ErrorCode, storage::ConcurrentBackingId};
    let store = mount_rs_memory::MemoryMetadataStore::new();
    let backing = ConcurrentBackingId::from_bytes([1; 16]).unwrap();
    assert!(
        store
            .compact_inode_mode_state()
            .await
            .unwrap_err()
            .is(ErrorCode::Enotsup)
    );
    assert!(
        store
            .prepare_compact_inode_mode(backing, 1)
            .await
            .unwrap_err()
            .is(ErrorCode::Enotsup)
    );
    assert!(
        store
            .load_compact_snapshot(backing)
            .await
            .unwrap_err()
            .is(ErrorCode::Enotsup)
    );
    assert!(
        store
            .load_compact_inode(backing, 1)
            .await
            .unwrap_err()
            .is(ErrorCode::Enotsup)
    );
}
