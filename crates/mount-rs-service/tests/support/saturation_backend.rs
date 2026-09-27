use mount_rs_sdk::{Filesystem, FoundationDbLeaseAuthority, SplitOptions, StoreConfig};
use mysql_async::{Pool, prelude::Queryable};
use std::path::PathBuf;
use tempfile::TempDir;

const INODE_SELECTOR: &str = "MOUNT_RS_REMOTE_SATURATION_INODE_UPDATES";
const COMPACT_SELECTOR: &str = "MOUNT_RS_REMOTE_SATURATION_COMPACT_INODE_UPDATES";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InodeMode {
    Legacy,
    Inode,
    Compact,
}

impl InodeMode {
    fn from_environment() -> Result<Self, String> {
        fn selected(name: &str) -> Result<bool, String> {
            match std::env::var(name) {
                Err(std::env::VarError::NotPresent) => Ok(false),
                Ok(value) if value == "0" => Ok(false),
                Ok(value) if value == "1" => Ok(true),
                Ok(_) | Err(std::env::VarError::NotUnicode(_)) => {
                    Err(format!("{name} requires 0 or 1"))
                }
            }
        }
        match (selected(INODE_SELECTOR)?, selected(COMPACT_SELECTOR)?) {
            (false, false) => Ok(Self::Legacy),
            (true, false) => Ok(Self::Inode),
            (false, true) => Ok(Self::Compact),
            (true, true) => Err("contradictory saturation inode selectors".into()),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::Inode => "MRC4",
            Self::Compact => "MRC5",
        }
    }

    pub fn inode_updates(self) -> bool {
        matches!(self, Self::Inode | Self::Compact)
    }

    pub fn compact_inode_updates(self) -> bool {
        self == Self::Compact
    }
}

pub struct Backend {
    pub name: String,
    pub identity: String,
    pub version: String,
    pub topology: Option<String>,
    pub inode_mode: InodeMode,
    store: StoreConfig,
    // Keep disposable local state until the fresh coordinator has verified it.
    _directory: Option<TempDir>,
}

impl Backend {
    pub fn child(&self, index: usize) -> Result<Self, String> {
        let (store, directory) = match &self.store {
            StoreConfig::Tidb {
                connection,
                volume_key,
                durable,
            } => (
                StoreConfig::Tidb {
                    connection: connection.clone(),
                    volume_key: format!("{volume_key}-sandbox-{index}"),
                    durable: *durable,
                },
                None,
            ),
            StoreConfig::Sqlite { .. } => {
                let directory =
                    tempfile::tempdir().map_err(|_| "SQLite child temporary directory failed")?;
                let store = StoreConfig::Sqlite {
                    path: directory.path().join("drive.sqlite"),
                };
                (store, Some(directory))
            }
            _ => return Err("separate-drive fixture requires TiDB or SQLite".into()),
        };
        Ok(Self {
            name: self.name.clone(),
            identity: self.identity.clone(),
            version: self.version.clone(),
            topology: self.topology.clone(),
            inode_mode: self.inode_mode,
            store,
            _directory: directory,
        })
    }

    /// Configure a provisioned, disposable backing before opening measured replicas.
    /// Metadata and blocks share this file; missing or unowned files are never created.
    pub fn configure_owned_sqlite_journal(&self, mode: &str) -> Result<serde_json::Value, String> {
        if !matches!(mode, "DELETE" | "WAL") {
            return Err("owned SQLite journal requires DELETE or WAL".into());
        }
        let StoreConfig::Sqlite { path } = &self.store else {
            return Err("owned SQLite journal requires the SQLite backend".into());
        };
        let owner = self
            ._directory
            .as_ref()
            .ok_or("owned SQLite journal requires a retained temporary directory")?;
        if path != &owner.path().join("drive.sqlite") {
            return Err("owned SQLite journal path is outside its owner".into());
        }
        #[cfg(not(unix))]
        {
            Err("owned SQLite journal file identity requires Unix".into())
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;

            fn stamp(path: &std::path::Path) -> Result<(u64, u64), String> {
                let metadata = std::fs::symlink_metadata(path)
                    .map_err(|_| "owned SQLite journal backing must already exist")?;
                if !metadata.file_type().is_file() || metadata.nlink() != 1 {
                    return Err(
                        "owned SQLite journal requires a regular single-link backing".into(),
                    );
                }
                Ok((metadata.dev(), metadata.ino()))
            }

            let original = stamp(path)?;
            let canonical_owner = std::fs::canonicalize(owner.path())
                .map_err(|_| "owned SQLite journal owner resolution failed")?;
            let canonical = std::fs::canonicalize(path)
                .map_err(|_| "owned SQLite journal backing resolution failed")?;
            if canonical != canonical_owner.join("drive.sqlite") || stamp(&canonical)? != original {
                return Err("owned SQLite journal backing identity mismatch".into());
            }
            let connection = rusqlite::Connection::open_with_flags(
                &canonical,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
            )
            .map_err(|_| "owned SQLite journal existing backing open failed")?;
            let observed: Result<(String, u64), String> = (|| {
                if stamp(path)? != original || stamp(&canonical)? != original {
                    return Err("owned SQLite journal backing changed while opening".into());
                }
                let (marker, backing): (String, String) = connection
                    .query_row(
                        "SELECT write_mode,backing_id FROM mount_rs_metadata WHERE id=1",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .map_err(|_| "owned SQLite journal requires a provisioned backing")?;
                let expected = match self.inode_mode {
                    InodeMode::Legacy => "MRC2",
                    InodeMode::Inode => "MRC4",
                    InodeMode::Compact => "MRC5",
                };
                if marker != expected {
                    return Err("owned SQLite journal provisioned mode mismatch".into());
                }
                mount_rs_core::storage::ConcurrentBackingId::from_hex(&backing)
                    .map_err(|_| "owned SQLite journal provisioned backing ID is invalid")?;
                connection
                    .execute_batch("PRAGMA main.synchronous=FULL;")
                    .map_err(|_| "owned SQLite journal FULL durability configuration failed")?;
                let journal_mode: String = match mode {
                    "DELETE" => {
                        connection
                            .query_row("PRAGMA main.journal_mode=DELETE", [], |row| row.get(0))
                    }
                    "WAL" => {
                        connection.query_row("PRAGMA main.journal_mode=WAL", [], |row| row.get(0))
                    }
                    _ => unreachable!("journal selector checked before opening"),
                }
                .map_err(|_| "owned SQLite journal mode configuration failed")?;
                if journal_mode != mode.to_ascii_lowercase() {
                    return Err("owned SQLite journal requested mode was not applied".into());
                }
                let synchronous: u64 = connection
                    .query_row("PRAGMA main.synchronous", [], |row| row.get(0))
                    .map_err(|_| "owned SQLite journal durability observation failed")?;
                if synchronous != 2 {
                    return Err("owned SQLite journal FULL durability was not retained".into());
                }
                Ok((journal_mode, synchronous))
            })();
            let final_stamp = stamp(path).and_then(|requested| {
                let canonical_stamp = stamp(&canonical)?;
                if requested != original || canonical_stamp != original {
                    return Err("owned SQLite journal backing changed during observation".into());
                }
                Ok(())
            });
            let closed = connection
                .close()
                .map_err(|_| "owned SQLite journal observer close failed");
            let (journal_mode, synchronous) = observed?;
            final_stamp?;
            closed?;
            Ok(serde_json::json!({
                "requested": mode,
                "journal_mode": journal_mode,
                "synchronous": synchronous,
                "owned_file_verified": true,
                "identity_unchanged": true,
                "scope": "one owned SQLite file shared by metadata and block provider roles",
            }))
        }
    }

    /// Stat local allocation gauges without opening SQLite or forcing a checkpoint.
    pub fn owned_sqlite_file_bytes(&self) -> Result<serde_json::Value, String> {
        let StoreConfig::Sqlite { path } = &self.store else {
            return Err("owned SQLite file receipt requires the SQLite backend".into());
        };
        let owner = self
            ._directory
            .as_ref()
            .ok_or("owned SQLite file receipt requires a retained temporary directory")?;
        if path != &owner.path().join("drive.sqlite") {
            return Err("owned SQLite file receipt path is outside its owner".into());
        }
        #[cfg(not(unix))]
        {
            Err("owned SQLite file allocation receipt requires Unix".into())
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;

            fn regular(path: &std::path::Path) -> Result<std::fs::Metadata, String> {
                let metadata = std::fs::symlink_metadata(path)
                    .map_err(|_| "owned SQLite file receipt backing must already exist")?;
                if !metadata.file_type().is_file() || metadata.nlink() != 1 {
                    return Err(
                        "owned SQLite file receipt requires regular single-link files".into(),
                    );
                }
                Ok(metadata)
            }

            let original = regular(path)?;
            let identity = (original.dev(), original.ino());
            let canonical_owner = std::fs::canonicalize(owner.path())
                .map_err(|_| "owned SQLite file receipt owner resolution failed")?;
            let canonical = std::fs::canonicalize(path)
                .map_err(|_| "owned SQLite file receipt backing resolution failed")?;
            let resolved = regular(&canonical)?;
            if canonical != canonical_owner.join("drive.sqlite")
                || (resolved.dev(), resolved.ino()) != identity
            {
                return Err("owned SQLite file receipt backing identity mismatch".into());
            }
            let mut files = serde_json::Map::new();
            for (role, name) in [
                ("database", "drive.sqlite"),
                ("wal", "drive.sqlite-wal"),
                ("shm", "drive.sqlite-shm"),
            ] {
                let observed = match std::fs::symlink_metadata(canonical_owner.join(name)) {
                    Ok(metadata) => {
                        if !metadata.file_type().is_file() || metadata.nlink() != 1 {
                            return Err(
                                "owned SQLite file receipt requires regular single-link files"
                                    .into(),
                            );
                        }
                        if role == "database" && (metadata.dev(), metadata.ino()) != identity {
                            return Err(
                                "owned SQLite file receipt backing changed while sampling".into()
                            );
                        }
                        let allocated = metadata
                            .blocks()
                            .checked_mul(512)
                            .ok_or("owned SQLite file allocation bytes overflow")?;
                        serde_json::json!({
                            "state": "present", "logical_bytes": metadata.len(), "allocated_bytes": allocated,
                        })
                    }
                    Err(error)
                        if role != "database" && error.kind() == std::io::ErrorKind::NotFound =>
                    {
                        serde_json::json!({
                            "state": "absent", "logical_bytes": null, "allocated_bytes": null,
                        })
                    }
                    Err(_) => return Err("owned SQLite file receipt stat failed".into()),
                };
                files.insert(role.into(), observed);
            }
            for checked in [path.as_path(), canonical.as_path()] {
                let final_metadata = regular(checked)?;
                if (final_metadata.dev(), final_metadata.ino()) != identity {
                    return Err("owned SQLite file receipt backing changed during sampling".into());
                }
            }
            Ok(serde_json::json!({
                "block_unit_bytes": 512,
                "files": files,
                "scope": "sequential filesystem stat at a drained boundary; size and allocation gauges, not physical device writes or IOPS; absent sidecars have unavailable byte gauges",
            }))
        }
    }

    /// Validate every stored byte without repeating a full namespace open per file.
    pub async fn verify_stored_files(&self, expected: &[Vec<(usize, u64)>]) -> Result<(), String> {
        self.verify_stored_files_from(0, expected).await
    }

    pub async fn verify_stored_files_from(
        &self,
        first_client: usize,
        expected: &[Vec<(usize, u64)>],
    ) -> Result<(), String> {
        use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};
        use mount_rs_tidb::{TidbBlockStore, TidbMetadataStore, TidbStorageOptions};
        match &self.store {
            StoreConfig::Tidb {
                connection,
                volume_key,
                durable,
            } => {
                let options = TidbStorageOptions::new(volume_key).with_durable(*durable);
                let metadata = TidbMetadataStore::connect_with_options(connection, options.clone())
                    .await
                    .map_err(|_| "verification metadata open failed")?;
                let blocks = std::sync::Arc::new(
                    TidbBlockStore::connect_with_options(connection, options)
                        .await
                        .map_err(|_| "verification block open failed")?,
                );
                let result = verify_stored_snapshot(
                    &metadata,
                    blocks.clone(),
                    self.inode_mode,
                    first_client,
                    expected,
                )
                .await;
                let metadata_closed = metadata.close().await;
                let blocks_closed = blocks.close().await;
                result?;
                metadata_closed.map_err(|_| "verification metadata close failed")?;
                blocks_closed.map_err(|_| "verification block close failed")?;
                Ok(())
            }
            StoreConfig::Sqlite { path } => {
                let metadata = SqliteMetadataStore::open(path)
                    .map_err(|_| "verification metadata open failed")?;
                let blocks = std::sync::Arc::new(
                    SqliteBlockStore::open(path).map_err(|_| "verification block open failed")?,
                );
                let result = verify_stored_snapshot(
                    &metadata,
                    blocks.clone(),
                    self.inode_mode,
                    first_client,
                    expected,
                )
                .await;
                result?;
                Ok(())
            }
            _ => Err("snapshot verification requires TiDB or SQLite".into()),
        }
    }

    /// Offline fixture preparation, not a measurement of online file creation.
    pub async fn preseed_empty_files(&self, count: usize) -> Result<(), String> {
        if self.inode_mode == InodeMode::Compact {
            return Err("offline empty-file preseed does not support compact mode".into());
        }
        use mount_rs_core::chunking::{Chunker, FixedSizeChunker};
        use mount_rs_core::{
            FsDriver,
            storage::{
                BlockStore, DirectoryEntry, FileLayout, MetadataStore, Namespace, NodeData,
                NodeMetadata,
            },
        };
        use mount_rs_tidb::{TidbBlockStore, TidbMetadataStore, TidbStorageOptions};
        let StoreConfig::Tidb {
            connection,
            volume_key,
            durable,
        } = &self.store
        else {
            return Err("offline preseed currently requires TiDB".into());
        };
        let options = TidbStorageOptions::new(volume_key).with_durable(*durable);
        let metadata = TidbMetadataStore::connect_with_options(connection, options.clone())
            .await
            .map_err(|_| "preseed metadata open failed")?;
        let blocks = TidbBlockStore::connect_with_options(connection, options)
            .await
            .map_err(|_| "preseed block open failed")?;
        let result: Result<(), String> = async {
            let backing = blocks
                .prepare_concurrent_backing()
                .await
                .map_err(|_| "preseed block authority failed")?;
            metadata
                .prepare_bound_concurrent_mode(backing)
                .await
                .map_err(|_| "preseed metadata authority failed")?;
            let stats = mount_rs_memfs::MemoryFs::empty()
                .stat("/")
                .await
                .map_err(|_| "preseed root stat failed")?;
            let root = stats.ino;
            let chunker = FixedSizeChunker::new(4096)
                .map_err(|_| "preseed chunker failed")?
                .config();
            let mut nodes = std::collections::BTreeMap::new();
            let mut entries = Vec::with_capacity(count);
            for client in 0..count {
                let inode = root + client as u64 + 1;
                let mut file = stats.clone();
                file.ino = inode;
                file.mode = mount_rs_core::S_IFREG | 0o644;
                file.nlink = 1;
                file.size = 0;
                file.blocks = 0;
                nodes.insert(
                    inode,
                    NodeMetadata {
                        stats: file,
                        data: NodeData::File(FileLayout {
                            chunker: chunker.clone(),
                            extents: vec![],
                        }),
                    },
                );
                entries.push(DirectoryEntry {
                    name: format!("saturation-{client}"),
                    inode,
                });
            }
            nodes.insert(
                root,
                NodeMetadata {
                    stats,
                    data: NodeData::Directory { entries },
                },
            );
            let namespace = Namespace {
                format_version: 1,
                root,
                next_inode: root + count as u64 + 1,
                default_uid: 0,
                default_gid: 0,
                umask: 0o022,
                default_chunker: chunker,
                nodes,
            };
            namespace
                .validate()
                .map_err(|_| "invalid preseed namespace")?;
            metadata
                .publish_bound_if_revision(backing, 0, namespace)
                .await
                .map_err(|_| "preseed publication failed")?;
            Ok(())
        }
        .await;
        let metadata_closed = metadata.close().await;
        let blocks_closed = blocks.close().await;
        result?;
        metadata_closed.map_err(|_| "preseed metadata close failed")?;
        blocks_closed.map_err(|_| "preseed block close failed")?;
        Ok(())
    }

    pub async fn from_environment(key: &str) -> Result<Self, String> {
        let inode_mode = InodeMode::from_environment()?;
        let name =
            std::env::var("MOUNT_RS_REMOTE_SATURATION_PROVIDER").unwrap_or_else(|_| "tidb".into());
        if inode_mode == InodeMode::Compact && name != "tidb" && name != "sqlite" {
            return Err("compact saturation requires TiDB or SQLite stored-mode oracle".into());
        }
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
            inode_mode,
            store,
            _directory: directory,
        })
    }

    /// Exact owned-key logical backing counts; never physical SSD residency.
    #[cfg(all(feature = "resource-profiling", unix))]
    pub async fn owned_counts(&self) -> Result<serde_json::Value, String> {
        let StoreConfig::Tidb {
            connection,
            volume_key,
            ..
        } = &self.store
        else {
            return Err("owned backing counts require TiDB".into());
        };
        let pool = Pool::from_url(connection).map_err(|_| "owned counts pool failed")?;
        let result = async {
            let mut connection = pool.get_conn().await.map_err(|_| "owned counts connection failed")?;
            let (blocks, bytes): (u64, u64) = connection.exec_first("SELECT COUNT(*), COALESCE(SUM(OCTET_LENGTH(bytes)), 0) FROM mount_rs_tidb_blocks WHERE volume_key = ?", (volume_key.as_bytes(),)).await.map_err(|_| "owned block count failed")?.ok_or("owned block counts missing")?;
            let (anchor_rows, anchor_namespace_bytes): (u64, u64) = connection.exec_first(
                "SELECT COUNT(*), COALESCE(SUM(OCTET_LENGTH(namespace)),0) FROM mount_rs_tidb_metadata WHERE volume_key=?",
                (volume_key.as_bytes(),),
            ).await.map_err(|_| "owned anchor count failed")?.ok_or("owned anchor counts missing")?;
            let rows = match self.inode_mode {
                InodeMode::Compact => {
                    let (guards, guard_node_bytes): (u64, u64) = connection.exec_first(
                        "SELECT COUNT(*), COALESCE(SUM(OCTET_LENGTH(node)),0) FROM mount_rs_tidb_compact_guards WHERE volume_key=?",
                        (volume_key.as_bytes(),),
                    ).await.map_err(|_| "owned compact guard count failed")?.ok_or("owned compact guard counts missing")?;
                    serde_json::json!({"compact_guards":guards,"compact_guard_node_json_bytes":guard_node_bytes})
                }
                InodeMode::Inode => {
                    let (inodes, node_bytes): (u64, u64) = connection.exec_first(
                        "SELECT COUNT(*), COALESCE(SUM(OCTET_LENGTH(node)),0) FROM mount_rs_tidb_inodes WHERE volume_key=?",
                        (volume_key.as_bytes(),),
                    ).await.map_err(|_| "owned inode count failed")?.ok_or("owned inode counts missing")?;
                    serde_json::json!({"inodes":inodes,"inode_json_bytes":node_bytes})
                }
                InodeMode::Legacy => serde_json::json!({}),
            };
            Ok(serde_json::json!({"owned_volume_key":volume_key,"blocks":blocks,"logical_block_bytes":bytes,"anchor_rows":anchor_rows,"anchor_namespace_json_bytes":anchor_namespace_bytes,"mode_rows":rows,"scope":"exact owned key; SQL logical bytes, not physical SSD bytes or IOPS"}))
        }.await;
        let closed = pool
            .disconnect()
            .await
            .map_err(|_| "owned counts pool disconnect failed".to_string());
        match (result, closed) {
            (Ok(result), Ok(())) => Ok(result),
            (Err(error), _) | (_, Err(error)) => Err(error),
        }
    }

    pub async fn open(&self, index: usize) -> Result<Filesystem, String> {
        self.open_in_context(index, None).await
    }

    pub async fn open_in_context(
        &self,
        index: usize,
        context: Option<&mount_rs_sdk::StorageContext>,
    ) -> Result<Filesystem, String> {
        let mut options = SplitOptions::memory(format!("remote-saturation-{index}"), 4096)
            .with_concurrent_writes(true)
            .with_inode_updates(self.inode_mode.inode_updates())
            .with_compact_inode_updates(self.inode_mode.compact_inode_updates());
        options.metadata = self.store.clone();
        options.blocks = self.store.clone();
        let result = match context {
            Some(context) => Filesystem::split_with_context(options, context).await,
            None => Filesystem::split(options).await,
        };
        result.map_err(|error| {
            // TiDB's db_error explicitly redacts URL parsing details.
            // Other providers retain code-only diagnostics.
            if self.name == "tidb" {
                format!("benchmark TiDB provider open failed: {error}")
            } else {
                format!(
                    "benchmark provider open failed: {:?} (details redacted)",
                    error.code
                )
            }
        })
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

    /// Read the persisted SQL marker and prove its backing against the provider API.
    pub async fn persisted_mode_receipt(&self) -> Result<serde_json::Value, String> {
        use mount_rs_core::storage::ConcurrentBackingId;
        use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};
        use mount_rs_tidb::{TidbBlockStore, TidbMetadataStore, TidbStorageOptions};

        let (marker, backing_hex) = match &self.store {
            StoreConfig::Tidb {
                connection,
                volume_key,
                ..
            } => {
                let pool = Pool::from_url(connection).map_err(|_| "mode receipt pool failed")?;
                let result: Result<(String, String), String> = async {
                    let mut connection = pool.get_conn().await.map_err(|_| "mode receipt connection failed")?;
                    type ModeRow = (Option<Vec<u8>>, Option<Vec<u8>>);
                    let row: Option<ModeRow> = connection.exec_first(
                        "SELECT write_mode,backing_id FROM mount_rs_tidb_metadata WHERE volume_key=?",
                        (volume_key.as_bytes(),),
                    ).await.map_err(|_| "mode receipt query failed")?;
                    let (mode, backing) = row.ok_or("mode receipt row missing")?;
                    Ok((String::from_utf8(mode.ok_or("mode marker missing")?).map_err(|_| "invalid mode marker")?,
                        String::from_utf8(backing.ok_or("backing marker missing")?).map_err(|_| "invalid backing marker")?))
                }.await;
                pool.disconnect()
                    .await
                    .map_err(|_| "mode receipt disconnect failed")?;
                result?
            }
            StoreConfig::Sqlite { path } => {
                let connection = rusqlite::Connection::open(path)
                    .map_err(|_| "mode receipt SQLite open failed")?;
                let row: (Option<String>, Option<String>) = connection
                    .query_row(
                        "SELECT write_mode,backing_id FROM mount_rs_metadata WHERE id=1",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .map_err(|_| "mode receipt SQLite query failed")?;
                (
                    row.0.ok_or("mode marker missing")?,
                    row.1.ok_or("backing marker missing")?,
                )
            }
            _ => return Err("persisted mode receipt requires TiDB or SQLite".into()),
        };
        let expected = match self.inode_mode {
            InodeMode::Legacy => "MRC2",
            InodeMode::Inode => "MRC4",
            InodeMode::Compact => "MRC5",
        };
        if marker != expected {
            return Err(format!(
                "persisted mode mismatch: requested {expected}, found {marker}"
            ));
        }
        let backing = ConcurrentBackingId::from_hex(&backing_hex)
            .map_err(|_| "invalid persisted backing ID")?;
        match &self.store {
            StoreConfig::Tidb {
                connection,
                volume_key,
                durable,
            } => {
                let options = TidbStorageOptions::new(volume_key).with_durable(*durable);
                let metadata = TidbMetadataStore::connect_with_options(connection, options.clone())
                    .await
                    .map_err(|_| "mode receipt metadata open failed")?;
                let blocks = TidbBlockStore::connect_with_options(connection, options)
                    .await
                    .map_err(|_| "mode receipt block open failed")?;
                let verified =
                    verify_mode_backing(&metadata, &blocks, self.inode_mode, backing).await;
                let metadata_closed = metadata.close().await;
                let blocks_closed = blocks.close().await;
                verified?;
                metadata_closed.map_err(|_| "mode receipt metadata close failed")?;
                blocks_closed.map_err(|_| "mode receipt block close failed")?;
            }
            StoreConfig::Sqlite { path } => {
                let metadata = SqliteMetadataStore::open(path)
                    .map_err(|_| "mode receipt metadata open failed")?;
                let blocks =
                    SqliteBlockStore::open(path).map_err(|_| "mode receipt block open failed")?;
                verify_mode_backing(&metadata, &blocks, self.inode_mode, backing).await?;
            }
            _ => unreachable!(),
        }
        Ok(serde_json::json!({
            "requested": self.inode_mode.label(),
            "persisted_marker": marker,
            "persisted_backing_id": backing_hex,
            "provider_backing_verified": true,
        }))
    }
}

async fn verify_mode_backing<M, B>(
    metadata: &M,
    blocks: &B,
    requested: InodeMode,
    backing: mount_rs_core::storage::ConcurrentBackingId,
) -> Result<(), String>
where
    M: mount_rs_core::storage::MetadataStore,
    B: mount_rs_core::storage::BlockStore,
{
    let state = match requested {
        InodeMode::Legacy => {
            if metadata
                .inode_mode_state()
                .await
                .map_err(|_| "MRC4 mode inspection failed")?
                .is_some()
                || metadata
                    .compact_inode_mode_state()
                    .await
                    .map_err(|_| "MRC5 mode inspection failed")?
                    .is_some()
            {
                return Err("legacy mode has inode marker".into());
            }
            None
        }
        InodeMode::Inode => metadata
            .inode_mode_state()
            .await
            .map_err(|_| "MRC4 mode inspection failed")?,
        InodeMode::Compact => metadata
            .compact_inode_mode_state()
            .await
            .map_err(|_| "MRC5 mode inspection failed")?,
    };
    if requested != InodeMode::Legacy && state.map(|state| state.backing) != Some(backing) {
        return Err("provider mode and persisted backing disagree".into());
    }
    blocks
        .verify_concurrent_backing(backing)
        .await
        .map_err(|_| "persisted block backing verification failed")?;
    Ok(())
}

async fn verify_stored_snapshot<M, B>(
    metadata: &M,
    blocks: std::sync::Arc<B>,
    requested: InodeMode,
    first_client: usize,
    expected: &[Vec<(usize, u64)>],
) -> Result<(), String>
where
    M: mount_rs_core::storage::MetadataStore,
    B: mount_rs_core::storage::BlockStore + 'static,
{
    use mount_rs_core::storage::NodeData;
    let (backing, namespace) = match requested {
        InodeMode::Legacy => return Err("stored oracle requires MRC4 or MRC5".into()),
        InodeMode::Inode => {
            let mode = metadata
                .inode_mode_state()
                .await
                .map_err(|_| "verification MRC4 mode failed")?
                .ok_or("verification requires persisted MRC4 marker")?;
            let snapshot = metadata
                .load_inode_snapshot(mode.backing)
                .await
                .map_err(|_| "verification MRC4 snapshot failed")?;
            snapshot
                .validate()
                .map_err(|_| "verification invalid MRC4 snapshot")?;
            (mode.backing, snapshot.namespace)
        }
        InodeMode::Compact => {
            let mode = metadata
                .compact_inode_mode_state()
                .await
                .map_err(|_| "verification MRC5 mode failed")?
                .ok_or("verification requires persisted MRC5 marker")?;
            let snapshot = metadata
                .load_compact_snapshot(mode.backing)
                .await
                .map_err(|_| "verification MRC5 snapshot failed")?;
            let (namespace, identities, structure) = snapshot
                .into_validated_namespace()
                .map_err(|_| "verification invalid MRC5 graph or guard identity")?;
            if structure.anchor().backing != mode.backing
                || structure.anchor().generation != mode.structural_generation
                || identities.len() != namespace.nodes.len()
            {
                return Err("verification MRC5 authority mismatch".into());
            }
            (mode.backing, namespace)
        }
    };
    blocks
        .verify_concurrent_backing(backing)
        .await
        .map_err(|_| "verification backing failed")?;
    let NodeData::Directory { entries } = &namespace.nodes[&namespace.root].data else {
        return Err("verification root is not directory".into());
    };
    let names: std::collections::BTreeMap<_, _> = entries
        .iter()
        .map(|entry| (entry.name.as_str(), entry.inode))
        .collect();
    if names.len() != expected.len() || namespace.nodes.len() != expected.len() + 1 {
        return Err("verification membership mismatch".into());
    }
    let slots = std::sync::Arc::new(tokio::sync::Semaphore::new(16));
    let mut tasks = tokio::task::JoinSet::new();
    for (offset, ledger) in expected.iter().enumerate() {
        let client = first_client
            .checked_add(offset)
            .ok_or("verification client overflow")?;
        let name = format!("saturation-{client}");
        let inode = names
            .get(name.as_str())
            .ok_or("verification missing name")?;
        let node = &namespace.nodes[inode];
        let NodeData::File(layout) = &node.data else {
            return Err("verification non-file".into());
        };
        if node.stats.size != (ledger.len() * super::BYTES) as u64 {
            return Err("verification size mismatch".into());
        }
        let layout = layout.clone();
        let ledger = ledger.clone();
        let blocks = blocks.clone();
        let slots = slots.clone();
        tasks.spawn(async move {
            let _permit = slots
                .acquire_owned()
                .await
                .map_err(|_| "verification semaphore closed")?;
            let mut actual = vec![0; ledger.len() * super::BYTES];
            for extent in layout.extents {
                let bytes = blocks
                    .get(&extent.block)
                    .await
                    .map_err(|_| "verification block read failed")?;
                let source = usize::try_from(extent.block_offset)
                    .map_err(|_| "verification invalid block offset")?;
                let target = usize::try_from(extent.file_offset)
                    .map_err(|_| "verification invalid file offset")?;
                let length = usize::try_from(extent.length)
                    .map_err(|_| "verification invalid extent length")?;
                let source_end = source
                    .checked_add(length)
                    .ok_or("verification extent overflow")?;
                let target_end = target
                    .checked_add(length)
                    .ok_or("verification extent overflow")?;
                actual
                    .get_mut(target..target_end)
                    .ok_or("verification extent outside file")?
                    .copy_from_slice(
                        bytes
                            .get(source..source_end)
                            .ok_or("verification extent outside block")?,
                    );
            }
            let want: Vec<u8> = ledger
                .iter()
                .flat_map(|(lane, seq)| super::payload(client, *lane, *seq))
                .collect();
            if actual != want {
                return Err(format!("stored file mismatch client {client}"));
            }
            Ok::<(), String>(())
        });
    }
    while let Some(result) = tasks.join_next().await {
        result.map_err(|_| "verification worker failed")??;
    }
    blocks
        .verify_concurrent_backing(backing)
        .await
        .map_err(|_| "verification final backing failed")?;
    Ok(())
}

fn required(name: &str) -> Result<String, String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("{name} required"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
    use mount_rs_core::{Loopback, storage::MetadataStore};
    #[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
    use mount_rs_sqlite::SqliteMetadataStore;

    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    struct EnvRestore(Vec<(&'static str, Option<std::ffi::OsString>)>);

    impl EnvRestore {
        fn set(values: &[(&'static str, &'static str)]) -> Self {
            let old = values
                .iter()
                .map(|(name, _)| (*name, std::env::var_os(name)))
                .collect();
            for &(name, value) in values {
                // These tests serialize their environment changes with ENV_LOCK.
                unsafe { std::env::set_var(name, value) };
            }
            Self(old)
        }
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            for (name, old) in &self.0 {
                // The same lock remains held until this guard is dropped.
                match old {
                    Some(value) => unsafe { std::env::set_var(name, value) },
                    None => unsafe { std::env::remove_var(name) },
                }
            }
        }
    }

    #[cfg(unix)]
    fn owned_sqlite_fixture() -> Backend {
        let directory = tempfile::tempdir().unwrap();
        Backend {
            name: "sqlite".into(),
            identity: "owned SQLite journal control".into(),
            version: rusqlite::version().into(),
            topology: Some("local-file".into()),
            inode_mode: InodeMode::Compact,
            store: StoreConfig::Sqlite {
                path: directory.path().join("drive.sqlite"),
            },
            _directory: Some(directory),
        }
    }

    #[cfg(unix)]
    #[test]
    fn owned_journal_guards_reject_unowned_or_missing_files_without_creating_them() {
        let mut backend = owned_sqlite_fixture();
        let StoreConfig::Sqlite { path } = &backend.store else {
            unreachable!()
        };
        let path = path.clone();
        for mode in ["", "wal", "delete", "WAL ", "TRUNCATE"] {
            assert!(backend.configure_owned_sqlite_journal(mode).is_err());
            assert!(!path.exists(), "invalid selector must not create a file");
        }
        assert!(backend.configure_owned_sqlite_journal("WAL").is_err());
        assert!(!path.exists(), "missing backing must not be created");
        assert!(backend.owned_sqlite_file_bytes().is_err());
        assert!(
            !path.exists(),
            "a file receipt must not create missing backing"
        );

        let external = tempfile::tempdir().unwrap();
        let external_path = external.path().join("outside.sqlite");
        let connection = rusqlite::Connection::open(&external_path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE contents(value BLOB); INSERT INTO contents VALUES(X'112233');",
            )
            .unwrap();
        connection.close().unwrap();
        let original = std::fs::read(&external_path).unwrap();
        backend.store = StoreConfig::Sqlite {
            path: external_path.clone(),
        };
        assert!(backend.configure_owned_sqlite_journal("WAL").is_err());
        assert!(backend.owned_sqlite_file_bytes().is_err());
        assert_eq!(std::fs::read(&external_path).unwrap(), original);

        backend.store = StoreConfig::Memory;
        assert!(backend.configure_owned_sqlite_journal("DELETE").is_err());
        assert!(backend.owned_sqlite_file_bytes().is_err());
        backend.store = StoreConfig::Sqlite { path: path.clone() };
        let owner = backend._directory.take().unwrap();
        assert!(backend.configure_owned_sqlite_journal("WAL").is_err());
        assert!(backend.owned_sqlite_file_bytes().is_err());
        assert!(!path.exists());
        backend._directory = Some(owner);

        std::os::unix::fs::symlink(&external_path, &path).unwrap();
        assert!(backend.configure_owned_sqlite_journal("WAL").is_err());
        assert!(backend.owned_sqlite_file_bytes().is_err());
        assert_eq!(std::fs::read(&external_path).unwrap(), original);
        std::fs::remove_file(&path).unwrap();
        std::fs::hard_link(&external_path, &path).unwrap();
        assert!(backend.configure_owned_sqlite_journal("WAL").is_err());
        assert!(backend.owned_sqlite_file_bytes().is_err());
        assert_eq!(std::fs::read(&external_path).unwrap(), original);
        std::fs::remove_file(&path).unwrap();
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch("CREATE TABLE unprovisioned(value BLOB);")
            .unwrap();
        connection.close().unwrap();
        let unprovisioned = std::fs::read(&path).unwrap();
        assert!(backend.configure_owned_sqlite_journal("WAL").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), unprovisioned);
    }

    #[cfg(unix)]
    #[test]
    fn owned_file_byte_receipts_observe_sizes_absence_and_reject_sidecar_aliases() {
        use std::os::unix::fs::MetadataExt;

        let backend = owned_sqlite_fixture();
        let StoreConfig::Sqlite { path } = &backend.store else {
            unreachable!()
        };
        // Invalid SQLite contents prove the receipt does not open or query SQLite.
        let contents = b"stat this private fixture without interpreting its contents";
        std::fs::write(path, contents).unwrap();
        let wal = path.with_file_name("drive.sqlite-wal");
        let shm = path.with_file_name("drive.sqlite-shm");
        let absent = backend.owned_sqlite_file_bytes().unwrap();
        assert_eq!(absent["block_unit_bytes"], 512);
        assert_eq!(
            absent["files"]["database"]["logical_bytes"],
            contents.len() as u64
        );
        for role in ["wal", "shm"] {
            assert_eq!(absent["files"][role]["state"], "absent");
            assert!(absent["files"][role]["logical_bytes"].is_null());
            assert!(absent["files"][role]["allocated_bytes"].is_null());
        }
        let file = std::fs::File::create(&wal).unwrap();
        file.set_len(65_536).unwrap();
        drop(file);
        std::fs::write(&shm, [0x51; 4096]).unwrap();
        let observed = backend.owned_sqlite_file_bytes().unwrap();
        for (role, file) in [
            ("database", path.as_path()),
            ("wal", wal.as_path()),
            ("shm", shm.as_path()),
        ] {
            let metadata = std::fs::symlink_metadata(file).unwrap();
            assert_eq!(observed["files"][role]["state"], "present");
            assert_eq!(observed["files"][role]["logical_bytes"], metadata.len());
            assert_eq!(
                observed["files"][role]["allocated_bytes"],
                metadata.blocks().checked_mul(512).unwrap()
            );
        }
        let encoded = serde_json::to_vec(&observed).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&encoded).unwrap(),
            observed
        );
        assert_eq!(std::fs::read(path).unwrap(), contents);
        std::fs::remove_file(&wal).unwrap();
        std::fs::remove_file(&shm).unwrap();
        assert_eq!(backend.owned_sqlite_file_bytes().unwrap(), absent);

        let external = tempfile::tempdir().unwrap();
        let external_path = external.path().join("sidecar-target");
        std::fs::write(&external_path, b"unchanged external sidecar bytes").unwrap();
        std::os::unix::fs::symlink(&external_path, &wal).unwrap();
        assert!(backend.owned_sqlite_file_bytes().is_err());
        std::fs::remove_file(&wal).unwrap();
        std::fs::hard_link(&external_path, &shm).unwrap();
        assert!(backend.owned_sqlite_file_bytes().is_err());
        std::fs::remove_file(&shm).unwrap();
        std::fs::create_dir(&wal).unwrap();
        assert!(backend.owned_sqlite_file_bytes().is_err());
        assert_eq!(
            std::fs::read(&external_path).unwrap(),
            b"unchanged external sidecar bytes"
        );
        assert_eq!(std::fs::read(path).unwrap(), contents);
    }

    #[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
    #[tokio::test]
    #[ignore = "requires owned SQLite MRC5 selectors, MOUNT_RS_PROFILE_IO=1, and exclusive registry ownership"]
    async fn owned_journal_conversion_preserves_both_provider_roles_and_fresh_bytes() {
        use mount_rs_sqlite::sqlite_io_diagnostics;
        use std::collections::BTreeSet;

        fn configurations(snapshot: &serde_json::Value, count: usize) -> Vec<(u64, String)> {
            assert!(snapshot.get("error").is_none(), "{snapshot}");
            let connections = snapshot["connections"].as_array().unwrap();
            assert_eq!(connections.len(), count, "{snapshot}");
            let mut ids = BTreeSet::new();
            connections
                .iter()
                .map(|connection| {
                    assert!(connection.get("error").is_none(), "{connection}");
                    let id = connection["connection_id"].as_u64().unwrap();
                    assert!(ids.insert(id), "connection IDs must be distinct");
                    assert_eq!(connection["configuration"]["synchronous"], 2);
                    (
                        id,
                        connection["configuration"]["journal_mode"]
                            .as_str()
                            .unwrap()
                            .to_owned(),
                    )
                })
                .collect()
        }

        let _lock = ENV_LOCK.lock().await;
        assert_eq!(std::env::var("MOUNT_RS_PROFILE_IO").as_deref(), Ok("1"));
        assert_eq!(
            std::env::var("MOUNT_RS_REMOTE_SATURATION_PROVIDER").as_deref(),
            Ok("sqlite")
        );
        assert_eq!(std::env::var(INODE_SELECTOR).as_deref(), Ok("0"));
        assert_eq!(std::env::var(COMPACT_SELECTOR).as_deref(), Ok("1"));
        assert!(
            sqlite_io_diagnostics(false)["connections"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let parent = Backend::from_environment("owned-journal-control")
            .await
            .unwrap();
        let backend = parent.child(0).unwrap();
        assert_eq!(backend.inode_mode, InodeMode::Compact);
        let first_payload = super::super::payload(0, 0, 1);
        assert_eq!(first_payload.len(), 4096);
        let fs = backend.open(0).await.unwrap();
        let view = Loopback::from_arc(fs.driver());
        let written = view.write_file("/saturation-0", &first_payload).await;
        drop(view);
        let closed = fs.shutdown().await;
        drop(fs);
        written.unwrap();
        closed.unwrap();
        for (mode, initial_sequence, second_sequence, final_sequence) in
            [("WAL", 1, 2, 3), ("DELETE", 3, 4, 5)]
        {
            let first_payload = super::super::payload(0, 0, initial_sequence);
            let before_receipt = backend.persisted_mode_receipt().await.unwrap();
            assert!(
                sqlite_io_diagnostics(false)["connections"]
                    .as_array()
                    .unwrap()
                    .is_empty(),
                "the owned conversion boundary must have no live providers"
            );
            let configured = backend.configure_owned_sqlite_journal(mode).unwrap();

            // Each SDK replica opens both the metadata and block provider roles.
            let left = backend.open(1).await.unwrap();
            let right = backend.open(2).await.unwrap();
            let left_view = Loopback::from_arc(left.driver());
            let right_view = Loopback::from_arc(right.driver());
            let initial_configurations = sqlite_io_diagnostics(false);
            let cross_replica: Result<(), String> = async {
                for view in [&left_view, &right_view] {
                    let actual = view
                        .read_file("/saturation-0")
                        .await
                        .map_err(|_| "initial replica read failed")?;
                    if actual != first_payload {
                        return Err("initial replica payload mismatch".into());
                    }
                }
                let second_payload = super::super::payload(0, 0, second_sequence);
                left_view
                    .write_file("/saturation-0", &second_payload)
                    .await
                    .map_err(|_| "left replica write failed")?;
                if right_view
                    .read_file("/saturation-0")
                    .await
                    .map_err(|_| "right replica read failed")?
                    != second_payload
                {
                    return Err("left-to-right replica payload mismatch".into());
                }
                let third_payload = super::super::payload(0, 0, final_sequence);
                right_view
                    .write_file("/saturation-0", &third_payload)
                    .await
                    .map_err(|_| "right replica write failed")?;
                if left_view
                    .read_file("/saturation-0")
                    .await
                    .map_err(|_| "left replica read failed")?
                    != third_payload
                {
                    return Err("right-to-left replica payload mismatch".into());
                }
                Ok(())
            }
            .await;
            let live_files = backend.owned_sqlite_file_bytes();
            let final_configurations = sqlite_io_diagnostics(false);
            drop(left_view);
            drop(right_view);
            let left_closed = left.shutdown().await;
            let right_closed = right.shutdown().await;
            drop(left);
            drop(right);
            cross_replica.unwrap();
            left_closed.unwrap();
            right_closed.unwrap();
            let closed_files = backend.owned_sqlite_file_bytes();
            assert!(
                sqlite_io_diagnostics(false)["connections"]
                    .as_array()
                    .unwrap()
                    .is_empty(),
                "replicas must be dropped before fresh verification"
            );

            backend
                .verify_stored_files(&[vec![(0, final_sequence)]])
                .await
                .unwrap();
            let fresh = backend.open(3).await.unwrap();
            let fresh_view = Loopback::from_arc(fresh.driver());
            let fresh_bytes = fresh_view.read_file("/saturation-0").await;
            let fresh_configurations = sqlite_io_diagnostics(false);
            drop(fresh_view);
            let fresh_closed = fresh.shutdown().await;
            drop(fresh);
            fresh_closed.unwrap();
            let after_receipt = backend.persisted_mode_receipt().await.unwrap();
            assert_eq!(
                fresh_bytes.unwrap(),
                super::super::payload(0, 0, final_sequence)
            );
            assert_eq!(before_receipt["persisted_marker"], "MRC5");
            assert_eq!(after_receipt["persisted_marker"], "MRC5");
            assert_eq!(after_receipt["provider_backing_verified"], true);
            assert_eq!(
                before_receipt["persisted_backing_id"],
                after_receipt["persisted_backing_id"]
            );
            let initial = configurations(&initial_configurations, 4);
            let final_configs = configurations(&final_configurations, 4);
            let fresh_configs = configurations(&fresh_configurations, 2);
            assert_eq!(
                initial, final_configs,
                "every replica connection remains observed"
            );
            let replica_ids: BTreeSet<_> = initial.iter().map(|(id, _)| *id).collect();
            assert!(
                fresh_configs
                    .iter()
                    .all(|(id, _)| !replica_ids.contains(id)),
                "fresh providers must have new observed connection identities"
            );
            assert_eq!(configured["synchronous"], 2);
            assert!(
                sqlite_io_diagnostics(false)["connections"]
                    .as_array()
                    .unwrap()
                    .is_empty(),
                "all provider resources must be dropped before the mode assertion"
            );
            let live_files = live_files.unwrap();
            let closed_files = closed_files.unwrap();
            assert_eq!(live_files["files"]["database"]["state"], "present");
            assert_eq!(closed_files["files"]["database"]["state"], "present");
            for role in ["wal", "shm"] {
                assert_eq!(
                    live_files["files"][role]["state"],
                    if mode == "WAL" { "present" } else { "absent" }
                );
                assert_eq!(closed_files["files"][role]["state"], "absent");
            }
            println!(
                "owned_journal_mode_behavior_complete: {mode} cross_replica_full_payload_mrc5_fresh_bytes_closed"
            );
            let expected_mode = mode.to_ascii_lowercase();
            assert_eq!(
                configured["journal_mode"], expected_mode,
                "owned journal conversion is missing"
            );
            for (_, mode) in initial
                .into_iter()
                .chain(final_configs)
                .chain(fresh_configs)
            {
                assert_eq!(
                    mode, expected_mode,
                    "every actual SDK provider connection must use the requested journal mode"
                );
            }
        }
        println!(
            "owned_journal_behavior_complete: cross_replica_full_payload_mrc5_fresh_bytes_closed"
        );
    }

    #[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
    #[tokio::test]
    async fn compact_selector_opens_mrc5_and_stored_oracle_checks_every_byte() {
        let _lock = ENV_LOCK.lock().await;
        let _env = EnvRestore::set(&[
            ("MOUNT_RS_REMOTE_SATURATION_PROVIDER", "sqlite"),
            ("MOUNT_RS_REMOTE_SATURATION_INODE_UPDATES", "0"),
            ("MOUNT_RS_REMOTE_SATURATION_COMPACT_INODE_UPDATES", "1"),
        ]);
        let backend = Backend::from_environment("compact-selector-test")
            .await
            .unwrap();
        let StoreConfig::Sqlite { path } = &backend.store else {
            panic!("SQLite backend required")
        };
        let fs = backend.open(0).await.unwrap();
        let view = Loopback::from_arc(fs.driver());
        view.write_file("/saturation-0", &super::super::payload(0, 0, 1))
            .await
            .unwrap();
        fs.shutdown().await.unwrap();
        let metadata = SqliteMetadataStore::open(path).unwrap();
        assert!(metadata.compact_inode_mode_state().await.unwrap().is_some());
        let receipt = backend.persisted_mode_receipt().await.unwrap();
        assert_eq!(receipt["persisted_marker"], "MRC5");
        assert_eq!(receipt["provider_backing_verified"], true);
        backend.verify_stored_files(&[vec![(0, 1)]]).await.unwrap();
        assert!(backend.verify_stored_files(&[vec![(0, 2)]]).await.is_err());
    }

    #[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
    #[tokio::test]
    async fn inode_selector_preserves_mrc4_stored_oracle() {
        let _lock = ENV_LOCK.lock().await;
        let _env = EnvRestore::set(&[
            ("MOUNT_RS_REMOTE_SATURATION_PROVIDER", "sqlite"),
            ("MOUNT_RS_REMOTE_SATURATION_INODE_UPDATES", "1"),
            ("MOUNT_RS_REMOTE_SATURATION_COMPACT_INODE_UPDATES", "0"),
        ]);
        let backend = Backend::from_environment("inode-selector-test")
            .await
            .unwrap();
        let fs = backend.open(0).await.unwrap();
        Loopback::from_arc(fs.driver())
            .write_file("/saturation-0", &super::super::payload(0, 0, 1))
            .await
            .unwrap();
        fs.shutdown().await.unwrap();
        let receipt = backend.persisted_mode_receipt().await.unwrap();
        assert_eq!(receipt["persisted_marker"], "MRC4");
        backend.verify_stored_files(&[vec![(0, 1)]]).await.unwrap();
        assert!(backend.verify_stored_files(&[vec![(0, 2)]]).await.is_err());
    }

    #[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
    #[tokio::test]
    async fn separate_fresh_read_failure_still_shuts_down_opened_filesystem() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };

        let _lock = ENV_LOCK.lock().await;
        let _env = EnvRestore::set(&[
            ("MOUNT_RS_REMOTE_SATURATION_PROVIDER", "sqlite"),
            ("MOUNT_RS_REMOTE_SATURATION_INODE_UPDATES", "0"),
            ("MOUNT_RS_REMOTE_SATURATION_COMPACT_INODE_UPDATES", "1"),
            ("MOUNT_RS_REMOTE_SATURATION_SNAPSHOT_VERIFY", "0"),
        ]);
        let backend = Backend::from_environment("separate-fresh-read-close-test")
            .await
            .unwrap();
        let shutdown_observed = Arc::new(AtomicBool::new(false));
        let observed = shutdown_observed.clone();
        let error = super::super::verify_one_separate_drive_with_shutdown(
            &backend,
            0,
            0,
            &[(0, 1)],
            move |fs| async move {
                observed.store(true, Ordering::SeqCst);
                fs.shutdown()
                    .await
                    .map_err(|_| "separate fresh shutdown failed".into())
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error, "separate fresh read failed");
        assert!(shutdown_observed.load(Ordering::SeqCst));

        let error = super::super::verify_one_separate_drive_with_shutdown(
            &backend,
            0,
            0,
            &[(0, 1)],
            |fs| async move {
                fs.shutdown().await.unwrap();
                Err("separate fresh shutdown failed".into())
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error, "separate fresh shutdown failed");
    }

    #[tokio::test]
    async fn contradictory_selectors_fail_before_provider_identity_open() {
        let _lock = ENV_LOCK.lock().await;
        let _env = EnvRestore::set(&[
            ("MOUNT_RS_REMOTE_SATURATION_PROVIDER", "tidb"),
            ("MOUNT_RS_TIDB_URL", "invalid"),
            ("MOUNT_RS_REMOTE_SATURATION_INODE_UPDATES", "1"),
            ("MOUNT_RS_REMOTE_SATURATION_COMPACT_INODE_UPDATES", "1"),
        ]);
        let error = Backend::from_environment("contradictory-selector-test")
            .await
            .err()
            .expect("contradictory selectors must fail");
        assert!(error.contains("contradictory"), "{error}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn non_unicode_compact_selector_fails_before_provider_open() {
        use std::os::unix::ffi::OsStringExt;

        let _lock = ENV_LOCK.lock().await;
        let _env = EnvRestore::set(&[
            ("MOUNT_RS_REMOTE_SATURATION_PROVIDER", "sqlite"),
            ("MOUNT_RS_REMOTE_SATURATION_INODE_UPDATES", "0"),
            ("MOUNT_RS_REMOTE_SATURATION_COMPACT_INODE_UPDATES", "0"),
        ]);
        unsafe { std::env::set_var(COMPACT_SELECTOR, std::ffi::OsString::from_vec(vec![0xff])) };
        let error = Backend::from_environment("non-unicode-selector-test")
            .await
            .err()
            .expect("non-Unicode selector must fail");
        assert!(error.contains(COMPACT_SELECTOR), "{error}");
    }
}
