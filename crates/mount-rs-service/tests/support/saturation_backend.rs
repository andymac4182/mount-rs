use mount_rs_sdk::{Filesystem, FoundationDbLeaseAuthority, SplitOptions, StoreConfig};
use mysql_async::{Pool, prelude::Queryable};
use std::path::PathBuf;
use tempfile::TempDir;

pub struct Backend {
    pub name: String,
    pub identity: String,
    pub version: String,
    pub topology: Option<String>,
    store: StoreConfig,
    // Keep disposable local state until the fresh coordinator has verified it.
    _directory: Option<TempDir>,
}

impl Backend {
    pub async fn from_environment(key: &str) -> Result<Self, String> {
        let name =
            std::env::var("MOUNT_RS_REMOTE_SATURATION_PROVIDER").unwrap_or_else(|_| "tidb".into());
        let mut directory = None;
        let (store, identity, version, topology) = match name.as_str() {
            "tidb" => {
                let url = required("MOUNT_RS_TIDB_URL")?;
                let pool = Pool::from_url(&url).map_err(|_| "invalid TiDB URL (redacted)")?;
                let queried = async {
                    let mut connection = pool
                        .get_conn()
                        .await
                        .map_err(|_| "TiDB identity connection failed")?;
                    let (identity, version): (String, String) = connection
                        .query_first("SELECT tidb_version(), VERSION()")
                        .await
                        .map_err(|_| "TiDB identity query failed")?
                        .ok_or("missing TiDB identity")?;
                    if !identity.to_ascii_lowercase().contains("release version:")
                        || !version.to_ascii_lowercase().contains("tidb")
                    {
                        return Err("actual TiDB required");
                    }
                    Ok((identity, version))
                }
                .await;
                let closed = pool
                    .disconnect()
                    .await
                    .map_err(|_| "TiDB identity disconnect failed");
                let (identity, version) = queried?;
                closed?;
                (
                    StoreConfig::Tidb {
                        connection: url,
                        volume_key: key.into(),
                        durable: true,
                    },
                    identity,
                    version,
                    std::env::var("MOUNT_RS_TIDB_TOPOLOGY").ok(),
                )
            }
            "sqlite" => {
                let owned = tempfile::tempdir().map_err(|_| "SQLite temporary directory failed")?;
                let store = StoreConfig::Sqlite {
                    path: owned.path().join("drive.sqlite"),
                };
                directory = Some(owned);
                (
                    store,
                    "embedded SQLite, local filesystem".into(),
                    rusqlite::version().into(),
                    Some("local-file".into()),
                )
            }
            "pglite" => {
                // Require the owned PGlite harness identity marker; a generic
                // PostgreSQL URL is not evidence of a PGlite measurement.
                let identity = required("MOUNT_RS_PGLITE_BENCH_IDENTITY")?;
                let store = StoreConfig::Pglite {
                    connection: required("PGLITE_DATABASE_URL")?,
                    volume_key: key.into(),
                    durable: true,
                };
                (
                    store,
                    identity,
                    "see harness package lock".into(),
                    Some("single-persistent-pglite".into()),
                )
            }
            "foundationdb" => {
                if !cfg!(feature = "saturation-foundationdb") {
                    return Err(
                        "FoundationDB benchmark requires saturation-foundationdb feature".into(),
                    );
                }
                let store = StoreConfig::FoundationDb {
                    cluster_file: PathBuf::from(required("MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE")?),
                    volume_key: key.into(),
                    durable: true,
                    lease_authority: FoundationDbLeaseAuthority::RevisionCas,
                };
                (
                    store,
                    required("MOUNT_RS_FOUNDATIONDB_BENCH_IDENTITY")?,
                    "see native harness identity".into(),
                    Some(
                        std::env::var("MOUNT_RS_FOUNDATIONDB_TOPOLOGY")
                            .unwrap_or_else(|_| "single".into()),
                    ),
                )
            }
            _ => return Err("provider must be sqlite, pglite, foundationdb, or tidb".into()),
        };
        Ok(Self {
            name,
            identity,
            version,
            topology,
            store,
            _directory: directory,
        })
    }

    pub async fn open(&self, index: usize) -> Result<Filesystem, String> {
        let mut options = SplitOptions::memory(format!("remote-saturation-{index}"), 4096)
            .with_concurrent_writes(true);
        if std::env::var("MOUNT_RS_REMOTE_SATURATION_INODE_UPDATES").as_deref() == Ok("1") {
            options = options.with_inode_updates(true);
        }
        options.metadata = self.store.clone();
        options.blocks = self.store.clone();
        Filesystem::split(options)
            .await
            .map_err(|_| "benchmark provider open failed (redacted)".into())
    }

    pub async fn namespace_bytes(&self) -> Result<Option<u64>, String> {
        let StoreConfig::Tidb {
            connection,
            volume_key,
            ..
        } = &self.store
        else {
            return Ok(None);
        };
        let pool = Pool::from_url(connection).map_err(|_| "invalid namespace probe URL")?;
        let queried = async {
            let mut connection = pool
                .get_conn()
                .await
                .map_err(|_| "namespace probe connect failed")?;
            let bytes: Option<Option<u64>> = connection
                .exec_first(
                    "SELECT OCTET_LENGTH(namespace) FROM mount_rs_tidb_metadata WHERE volume_key=?",
                    (volume_key,),
                )
                .await
                .map_err(|_| "namespace size query failed")?;
            bytes
                .flatten()
                .map(Some)
                .ok_or("namespace missing after setup")
        }
        .await;
        let closed = pool
            .disconnect()
            .await
            .map_err(|_| "namespace probe disconnect failed");
        let bytes = queried?;
        closed?;
        Ok(bytes)
    }
}

fn required(name: &str) -> Result<String, String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("{name} required"))
}
