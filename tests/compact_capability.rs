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
