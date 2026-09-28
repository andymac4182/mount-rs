use mount_rs_sdk::{Filesystem, SplitOptions, StorageContext, StoreConfig};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;
#[derive(Clone, Serialize, Deserialize)]
pub struct Backend {
    pub provider: String,
    #[serde(default = "default_block_provider")]
    pub block_provider: String,
    pub root: PathBuf,
    pub prefix: String,
}
fn default_block_provider() -> String {
    "metadata".into()
}

impl Backend {
    pub fn stores(&self, drive: usize) -> Result<(StoreConfig, StoreConfig), String> {
        let metadata = self.store(drive)?;
        let blocks = super::remote_blocks::resolve_blocks(
            &self.provider,
            &format!("{}/drive-{drive}/blocks", self.prefix),
            Some(&self.block_provider),
            |name| std::env::var(name).ok(),
        )?
        .unwrap_or_else(|| metadata.clone());
        Ok((metadata, blocks))
    }
    pub fn store(&self, drive: usize) -> Result<StoreConfig, String> {
        match self.provider.as_str() {
            "sqlite" => Ok(StoreConfig::Sqlite {
                path: self.root.join(format!("drive-{drive}.sqlite")),
            }),
            "tidb" => Ok(StoreConfig::Tidb {
                connection: std::env::var("MOUNT_RS_TIDB_URL").map_err(|_| "TiDB URL missing")?,
                volume_key: format!("{}-drive-{drive}", self.prefix),
                durable: true,
            }),
            _ => Err("unsupported target provider".into()),
        }
    }
    /// Resolve exact immutable construction choices without opening a provider.
    pub fn options(&self, drive: usize) -> Result<SplitOptions, String> {
        let (store, blocks) = self.stores(drive)?;
        Ok(self.split_options(drive, store, blocks))
    }
    fn split_options(
        &self,
        drive: usize,
        metadata: StoreConfig,
        blocks: StoreConfig,
    ) -> SplitOptions {
        let mut options = SplitOptions::memory(format!("{}-{drive}", self.prefix), 4096)
            .with_concurrent_writes(true)
            .with_inode_updates(true)
            .with_compact_inode_updates(true);
        options.metadata = metadata;
        options.blocks = blocks;
        options
    }
    /// Resolve private environment/backend options once for a worker lifetime.
    /// Per-drive keys remain exactly the existing keys; no provider is opened.
    pub fn prepared_options(&self, drives: usize) -> Result<Vec<SplitOptions>, String> {
        self.prepared_options_with(drives, |name| std::env::var(name).ok())
    }
    fn prepared_options_with(
        &self,
        drives: usize,
        mut lookup: impl FnMut(&str) -> Option<String>,
    ) -> Result<Vec<SplitOptions>, String> {
        if drives == 0 || drives > 10_000 {
            return Err("worker drive geometry invalid".into());
        }
        let connection = match self.provider.as_str() {
            "sqlite" => None,
            "tidb" => Some(lookup("MOUNT_RS_TIDB_URL").ok_or("TiDB URL missing")?),
            _ => return Err("unsupported target provider".into()),
        };
        let blocks = super::remote_blocks::resolve_blocks(
            &self.provider,
            &format!("{}/drive-0/blocks", self.prefix),
            Some(&self.block_provider),
            &mut lookup,
        )?;
        (0..drives)
            .map(|drive| {
                let metadata = match &connection {
                    None => StoreConfig::Sqlite {
                        path: self.root.join(format!("drive-{drive}.sqlite")),
                    },
                    Some(connection) => StoreConfig::Tidb {
                        connection: connection.clone(),
                        volume_key: format!("{}-drive-{drive}", self.prefix),
                        durable: true,
                    },
                };
                let blocks = match &blocks {
                    None => metadata.clone(),
                    Some(blocks) => {
                        let mut blocks = blocks.clone();
                        let StoreConfig::RustFs { prefix, .. } = &mut blocks else {
                            return Err("prepared block provider shape invalid".into());
                        };
                        *prefix = format!("{}/drive-{drive}/blocks", self.prefix);
                        blocks
                    }
                };
                Ok(self.split_options(drive, metadata, blocks))
            })
            .collect()
    }
    pub async fn open(&self, drive: usize, context: &StorageContext) -> Result<Filesystem, String> {
        Filesystem::split_with_context(self.options(drive)?, context)
            .await
            .map_err(|e| format!("provider open failed: {:?}", e.code))
    }
    pub async fn receipt(&self, drive: usize, context: &StorageContext) -> Result<Value, String> {
        let (store, blocks) = self.stores(drive)?;
        let mode = context
            .inspect_compact_layout(&store, &blocks)
            .await
            .map_err(|error| format!("receipt compact layout inspection failed: {:?}", error.code))?
            .ok_or("persistent MRC5 marker missing")?;
        let mut receipt = json!({"drive":drive,"mode":"MRC5","backing":mode.backing.to_hex(),"provider_backing_verified":true});
        if matches!(store, StoreConfig::Sqlite { .. }) {
            receipt["sqlite_version"] = json!(rusqlite::version());
        }
        Ok(receipt)
    }
}

#[cfg(test)]
mod receipt_pooling_tests {
    use super::*;

    #[test]
    fn lazy_target_options_resolve_once_and_preserve_exact_drive_keys() {
        let backend = Backend {
            provider: "tidb".into(),
            block_provider: "rustfs".into(),
            root: "/unused".into(),
            prefix: "owned-target".into(),
        };
        let mut reads = std::collections::BTreeMap::<String, usize>::new();
        let options = backend
            .prepared_options_with(10, |name| {
                *reads.entry(name.into()).or_default() += 1;
                Some(
                    match name {
                        "MOUNT_RS_TIDB_URL" => "mysql://private@127.0.0.1:4000/db",
                        "MOUNT_RS_RUSTFS_ENDPOINT" => "http://127.0.0.1:9000",
                        "MOUNT_RS_RUSTFS_BUCKET" => "owned-bucket",
                        "MOUNT_RS_RUSTFS_REGION" => "us-east-1",
                        "MOUNT_RS_RUSTFS_ACCESS_KEY_ID" => "synthetic-fixture-key",
                        "MOUNT_RS_RUSTFS_SECRET_ACCESS_KEY" => "synthetic-fixture-secret",
                        "MOUNT_RS_RUSTFS_DURABLE" => "1",
                        _ => panic!("unexpected configuration lookup"),
                    }
                    .into(),
                )
            })
            .unwrap();
        assert_eq!(reads.len(), 7);
        assert!(reads.values().all(|count| *count == 1));
        assert_eq!(options.len(), 10);
        for (drive, options) in options.into_iter().enumerate() {
            assert_eq!(options.chunk_size_bytes, 4096);
            assert!(
                options.concurrent_writes && options.inode_updates && options.compact_inode_updates
            );
            assert!(!options.writeback && !options.delegated);
            assert_eq!(options.owner, format!("owned-target-{drive}"));
            assert!(
                matches!(options.metadata,StoreConfig::Tidb {volume_key,durable:true,..} if volume_key==format!("owned-target-drive-{drive}"))
            );
            assert!(
                matches!(options.blocks,StoreConfig::RustFs {prefix,durable:true,..} if prefix==format!("owned-target/drive-{drive}/blocks"))
            );
        }
        let mut calls = 0;
        assert!(
            backend
                .prepared_options_with(0, |_| {
                    calls += 1;
                    None
                })
                .is_err()
        );
        assert_eq!(calls, 0);
    }
    #[test]
    fn lazy_target_sqlite_plans_preserve_shared_metadata_blocks_without_env_reads() {
        let backend = Backend {
            provider: "sqlite".into(),
            block_provider: "metadata".into(),
            root: "/owned".into(),
            prefix: "keys".into(),
        };
        let options = backend
            .prepared_options_with(10, |_| panic!("SQLite metadata plans need no environment"))
            .unwrap();
        for (drive, options) in options.into_iter().enumerate() {
            let expected = std::path::PathBuf::from(format!("/owned/drive-{drive}.sqlite"));
            assert!(matches!(options.metadata,StoreConfig::Sqlite {path} if path==expected));
            assert!(matches!(options.blocks,StoreConfig::Sqlite {path} if path==expected));
        }
    }
    #[tokio::test]
    async fn receipt_pooling_context_preserves_fields_and_caller_usability() {
        let root = tempfile::tempdir().unwrap();
        let backend = Backend {
            provider: "sqlite".into(),
            block_provider: "metadata".into(),
            root: root.path().into(),
            prefix: "receipt-pooling".into(),
        };
        let context = StorageContext::new(2).unwrap();
        let fs = backend.open(3, &context).await.unwrap();
        let first = backend.receipt(3, &context).await.unwrap();
        assert_eq!(first["drive"], 3);
        assert_eq!(first["mode"], "MRC5");
        assert_eq!(first["provider_backing_verified"], true);
        assert_eq!(first["sqlite_version"], rusqlite::version());
        assert_eq!(backend.receipt(3, &context).await.unwrap(), first);
        fs.driver()
            .write_file("/after-receipt", b"usable caller")
            .await
            .unwrap();
        assert_eq!(backend.receipt(3, &context).await.unwrap(), first);
        fs.shutdown().await.unwrap();
        drop(fs);
        context.close().await.unwrap();
    }

    #[tokio::test]
    async fn receipt_pooling_context_rejects_terminal_owner_before_opening_provider() {
        let root = tempfile::tempdir().unwrap();
        let backend = Backend {
            provider: "sqlite".into(),
            block_provider: "metadata".into(),
            root: root.path().into(),
            prefix: "receipt-pooling".into(),
        };
        let context = StorageContext::new(2).unwrap();
        context.close().await.unwrap();
        assert!(backend.receipt(4, &context).await.is_err());
        assert!(
            !root.path().join("drive-4.sqlite").exists(),
            "receipt must use the supplied terminal context before provider opening"
        );
    }
}
