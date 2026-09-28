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
    pub async fn open(&self, drive: usize, context: &StorageContext) -> Result<Filesystem, String> {
        let (store, blocks) = self.stores(drive)?;
        let mut options = SplitOptions::memory(format!("{}-{drive}", self.prefix), 4096)
            .with_concurrent_writes(true)
            .with_inode_updates(true)
            .with_compact_inode_updates(true);
        options.metadata = store.clone();
        options.blocks = blocks;
        Filesystem::split_with_context(options, context)
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
