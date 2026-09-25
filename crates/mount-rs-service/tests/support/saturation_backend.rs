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
    pub fn child(&self, index: usize) -> Result<Self, String> {
        let StoreConfig::Tidb {
            connection,
            volume_key,
            durable,
        } = &self.store
        else {
            return Err("separate-drive fixture requires TiDB".into());
        };
        Ok(Self {
            name: self.name.clone(),
            identity: self.identity.clone(),
            version: self.version.clone(),
            topology: self.topology.clone(),
            store: StoreConfig::Tidb {
                connection: connection.clone(),
                volume_key: format!("{volume_key}-sandbox-{index}"),
                durable: *durable,
            },
            _directory: None,
        })
    }

    /// Validate every stored byte without repeating a full namespace open per file.
    pub async fn verify_stored_files(&self, expected: &[Vec<(usize, u64)>]) -> Result<(), String> {
        use mount_rs_core::storage::{BlockStore, MetadataStore, NodeData};
        use mount_rs_tidb::{TidbBlockStore, TidbMetadataStore, TidbStorageOptions};
        let StoreConfig::Tidb {
            connection,
            volume_key,
            durable,
        } = &self.store
        else {
            return Err("snapshot verification currently requires TiDB".into());
        };
        let options = TidbStorageOptions::new(volume_key).with_durable(*durable);
        let metadata = TidbMetadataStore::connect_with_options(connection, options.clone())
            .await
            .map_err(|_| "verification metadata open failed")?;
        let blocks = std::sync::Arc::new(
            TidbBlockStore::connect_with_options(connection, options)
                .await
                .map_err(|_| "verification block open failed")?,
        );
        let result: Result<(), String> = async {
            let mode = metadata
                .inode_mode_state()
                .await
                .map_err(|_| "verification mode failed")?
                .ok_or("verification requires inode mode")?;
            blocks
                .verify_concurrent_backing(mode.backing)
                .await
                .map_err(|_| "verification backing failed")?;
            let snapshot = metadata
                .load_inode_snapshot(mode.backing)
                .await
                .map_err(|_| "verification snapshot failed")?;
            snapshot
                .validate()
                .map_err(|_| "verification invalid snapshot")?;
            let NodeData::Directory { entries } =
                &snapshot.namespace.nodes[&snapshot.namespace.root].data
            else {
                return Err("verification root is not directory".into());
            };
            let names: std::collections::BTreeMap<_, _> = entries
                .iter()
                .map(|entry| (entry.name.as_str(), entry.inode))
                .collect();
            if names.len() != expected.len() || snapshot.namespace.nodes.len() != expected.len() + 1
            {
                return Err("verification membership mismatch".into());
            }
            let slots = std::sync::Arc::new(tokio::sync::Semaphore::new(16));
            let mut tasks = tokio::task::JoinSet::new();
            for (client, ledger) in expected.iter().enumerate() {
                let name = format!("saturation-{client}");
                let inode = names
                    .get(name.as_str())
                    .ok_or("verification missing name")?;
                let node = &snapshot.namespace.nodes[inode];
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
                .verify_concurrent_backing(mode.backing)
                .await
                .map_err(|_| "verification final backing failed")?;
            Ok(())
        }
        .await;
        let metadata_closed = metadata.close().await;
        let blocks_closed = blocks.close().await;
        result?;
        metadata_closed.map_err(|_| "verification metadata close failed")?;
        blocks_closed.map_err(|_| "verification block close failed")?;
        Ok(())
    }

    /// Offline fixture preparation, not a measurement of online file creation.
    pub async fn preseed_empty_files(&self, count: usize) -> Result<(), String> {
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
            let (inodes, node_bytes): (u64, u64) = connection.exec_first("SELECT COUNT(*), COALESCE(SUM(OCTET_LENGTH(node)), 0) FROM mount_rs_tidb_inodes WHERE volume_key = ?", (volume_key.as_bytes(),)).await.map_err(|_| "owned inode count failed")?.ok_or("owned inode counts missing")?;
            Ok(serde_json::json!({"owned_volume_key":volume_key,"blocks":blocks,"logical_block_bytes":bytes,"inodes":inodes,"inode_json_bytes":node_bytes,"scope":"exact owned key; SQL payload sums are not physical SSD bytes"}))
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
            .with_concurrent_writes(true);
        if std::env::var("MOUNT_RS_REMOTE_SATURATION_INODE_UPDATES").as_deref() == Ok("1") {
            options = options.with_inode_updates(true);
        }
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
}

fn required(name: &str) -> Result<String, String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("{name} required"))
}
