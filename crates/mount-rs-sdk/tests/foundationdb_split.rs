#![cfg(all(
    feature = "foundationdb",
    any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
    )
))]
use mount_rs_core::storage::{BlockStore, MetadataStore};
use mount_rs_core::{Loopback, Result};
use mount_rs_foundationdb::{
    FoundationDbBlockAuthorityPolicy, FoundationDbStorage, FoundationDbStorageOptions,
};
use mount_rs_sdk::{
    BlockStoreDecorator, Filesystem, FoundationDbLeaseAuthority, SplitOptions, StorageContext,
    StoreConfig,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
struct Decorator(AtomicUsize);
impl BlockStoreDecorator for Decorator {
    fn decorate(&self, _: &StoreConfig, store: Arc<dyn BlockStore>) -> Result<Arc<dyn BlockStore>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(store)
    }
}
#[tokio::test]
#[ignore = "requires actual FoundationDB and MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE"]
async fn actual_foundationdb_split_assembly_binds_policy_through_context_and_decorator() {
    let cluster = std::env::var("MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE").unwrap();
    let run = format!(
        "sdk-split-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let context = StorageContext::default();
    let decorator = Decorator(AtomicUsize::new(0));
    for external in [false, true] {
        let prefix = format!("{run}/{external}");
        let config = |key: String| StoreConfig::FoundationDb {
            cluster_file: cluster.clone().into(),
            volume_key: key,
            durable: true,
            lease_authority: FoundationDbLeaseAuthority::RevisionCas,
        };
        let metadata_key = format!("{prefix}/metadata");
        let blocks_key = if external {
            format!("{prefix}/blocks")
        } else {
            metadata_key.clone()
        };
        let options = || {
            let mut o = SplitOptions::memory("sdk-fdb-split", 4096).with_inode_updates(true);
            o.metadata = config(metadata_key.clone());
            o.blocks = config(blocks_key.clone());
            o
        };
        let fs =
            Filesystem::split_with_context_and_block_decorator(options(), &context, &decorator)
                .await
                .unwrap();
        let view = Loopback::from_arc(fs.driver());
        let bytes = vec![73u8; 4096];
        view.write_file("/oracle", &bytes).await.unwrap();
        assert_eq!(view.read_file("/oracle").await.unwrap(), bytes);
        fs.shutdown().await.unwrap();
        drop(view);
        drop(fs);
        let reopened = Filesystem::split(options()).await.unwrap();
        assert_eq!(
            Loopback::from_arc(reopened.driver())
                .read_file("/oracle")
                .await
                .unwrap(),
            bytes
        );
        reopened.driver().unlink("/oracle").await.unwrap();
        reopened.shutdown().await.unwrap();
        drop(reopened);
        let policy = if external {
            FoundationDbBlockAuthorityPolicy::ExternalBlockStore
        } else {
            FoundationDbBlockAuthorityPolicy::SameKeyspace
        };
        let store = FoundationDbStorage::connect(
            &cluster,
            FoundationDbStorageOptions::new(&metadata_key).with_block_authority_policy(policy),
        )
        .unwrap();
        assert!(store.metadata().inode_mode_state().await.unwrap().is_some());
        drop(store);
        {
            let mut wrong = options();
            wrong.blocks = config(format!("{prefix}/wrong"));
            assert!(
                Filesystem::split_with_context(wrong, &context)
                    .await
                    .is_err(),
                "changed pair must not downgrade local policy or change external backing"
            );
        }
    }
    assert_eq!(decorator.0.load(Ordering::SeqCst), 2);
    context.close().await.unwrap();
    mount_rs_foundationdb::shutdown_client_network().unwrap();
}
