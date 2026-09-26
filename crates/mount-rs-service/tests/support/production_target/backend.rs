use mount_rs_core::storage::{BlockStore, MetadataStore};
use mount_rs_sdk::{Filesystem, SplitOptions, StorageContext, StoreConfig};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;
#[derive(Clone, Serialize, Deserialize)]
pub struct Backend {
    pub provider: String,
    pub root: PathBuf,
    pub prefix: String,
}
impl Backend {
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
        let store = self.store(drive)?;
        let mut options = SplitOptions::memory(format!("{}-{drive}", self.prefix), 4096)
            .with_concurrent_writes(true)
            .with_inode_updates(true)
            .with_compact_inode_updates(true);
        options.metadata = store.clone();
        options.blocks = store;
        Filesystem::split_with_context(options, context)
            .await
            .map_err(|e| format!("provider open failed: {:?}", e.code))
    }
    pub async fn receipt(&self, drive: usize) -> Result<Value, String> {
        match self.store(drive)? {
            StoreConfig::Sqlite { path } => {
                let meta = mount_rs_sqlite::SqliteMetadataStore::open(&path)
                    .map_err(|_| "receipt metadata open failed")?;
                let blocks = mount_rs_sqlite::SqliteBlockStore::open(&path)
                    .map_err(|_| "receipt block open failed")?;
                let mode = meta
                    .compact_inode_mode_state()
                    .await
                    .map_err(|_| "MRC5 mode read failed")?
                    .ok_or("persistent MRC5 marker missing")?;
                blocks
                    .verify_concurrent_backing(mode.backing)
                    .await
                    .map_err(|_| "persistent backing mismatch")?;
                Ok(
                    json!({"drive":drive,"mode":"MRC5","backing":mode.backing.to_hex(),"provider_backing_verified":true,"sqlite_version":rusqlite::version()}),
                )
            }
            StoreConfig::Tidb {
                connection,
                volume_key,
                durable,
            } => {
                let options =
                    mount_rs_tidb::TidbStorageOptions::new(&volume_key).with_durable(durable);
                let meta = mount_rs_tidb::TidbMetadataStore::connect_with_options(
                    &connection,
                    options.clone(),
                )
                .await
                .map_err(|_| "receipt metadata open failed")?;
                let result=async{
                    let blocks=mount_rs_tidb::TidbBlockStore::connect_with_options(&connection,options).await.map_err(|_|"receipt block open failed")?;
                    let checked=async {let mode=meta.compact_inode_mode_state().await.map_err(|_|"MRC5 mode read failed")?.ok_or("persistent MRC5 marker missing")?;
                        blocks.verify_concurrent_backing(mode.backing).await.map_err(|_|"persistent backing mismatch")?;
                        Ok::<_,String>(json!({"drive":drive,"mode":"MRC5","backing":mode.backing.to_hex(),"provider_backing_verified":true}))}.await;
                    let closed=blocks.close().await.map_err(|_|"receipt blocks close failed");closed?;checked
                }.await;
                meta.close()
                    .await
                    .map_err(|_| "receipt metadata close failed")?;
                result
            }
            _ => Err("unsupported receipt provider".into()),
        }
    }
}
