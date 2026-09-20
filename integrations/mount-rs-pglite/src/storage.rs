//! Independent PGlite metadata and immutable block providers.
//!
//! These providers use separate tables and separate PostgreSQL-wire
//! connections, so metadata and blocks may be selected independently by the
//! filesystem composition layer. Durability is an explicit caller assertion:
//! the default is volatile because a PGlite server may be in-memory, and a
//! successful SQL acknowledgement is not proof of host-disk persistence.
//! This provider never makes a host-safe-mount claim about the server's data
//! directory.

use async_trait::async_trait;
use mount_rs_core::storage::{
    BlockId, BlockStore, LoadedMetadata, MetadataStore, Namespace, WriterLease,
};
use mount_rs_core::{ErrorCode, FsError, Result, backend_error};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, MutexGuard};
use tokio::task::JoinHandle;
use tokio_postgres::types::Type;
use tokio_postgres::{Client, NoTls};

use super::postgres_error;

// The clock is evaluated by PostgreSQL/PGlite, never supplied by the client.
// clock_timestamp() deliberately uses the current provider clock even inside
// a transaction, which matters when a transaction waits for a row lock.
const NOW: &str = "CAST(EXTRACT(EPOCH FROM clock_timestamp()) * 1000 AS BIGINT)";

const METADATA_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS mount_rs_metadata (
 volume_key TEXT PRIMARY KEY NOT NULL,
 revision BIGINT NOT NULL CHECK(revision>=0),
 namespace TEXT,
 owner TEXT,
 fence BIGINT NOT NULL CHECK(fence>=0),
 expires BIGINT NOT NULL);
";

const BLOCK_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS mount_rs_blocks (
 volume_key TEXT NOT NULL, id TEXT NOT NULL, bytes BYTEA NOT NULL,
 PRIMARY KEY (volume_key, id));";

/// Connection-scoped selection for one independent metadata/block volume.
///
/// `durable` is deliberately not inferred from the URL or from successful
/// network acknowledgements. Set it only when the caller has configured the
/// PGlite server with a persistence policy suitable for its application.
#[derive(Debug, Clone)]
pub struct PgliteStorageOptions {
    pub volume_key: String,
    pub durable: bool,
}

impl PgliteStorageOptions {
    pub fn new(volume_key: impl Into<String>) -> Self {
        Self {
            volume_key: volume_key.into(),
            durable: false,
        }
    }

    pub fn with_durable(mut self, durable: bool) -> Self {
        self.durable = durable;
        self
    }
}

impl Default for PgliteStorageOptions {
    fn default() -> Self {
        Self::new("mount-rs")
    }
}

#[derive(Clone)]
struct Database {
    client: Arc<Mutex<Option<Client>>>,
    connection: Arc<Mutex<Option<JoinHandle<()>>>>,
    volume_key: String,
    durable: bool,
}

impl Database {
    async fn connect(
        connection_string: &str,
        schema: &str,
        options: PgliteStorageOptions,
    ) -> Result<Self> {
        if options.volume_key.is_empty() {
            return Err(
                FsError::new(ErrorCode::Einval).with_message("PGlite volume key must not be empty")
            );
        }
        let (client, connection) = tokio_postgres::connect(connection_string, NoTls)
            .await
            .map_err(postgres_error)?;
        let connection = tokio::spawn(async move {
            let _ = connection.await;
        });
        let database = Self {
            client: Arc::new(Mutex::new(Some(client))),
            connection: Arc::new(Mutex::new(Some(connection))),
            volume_key: options.volume_key,
            durable: options.durable,
        };
        database
            .client
            .lock()
            .await
            .as_ref()
            .ok_or_else(connection_closed)?
            .batch_execute(schema)
            .await
            .map_err(postgres_error)?;
        Ok(database)
    }

    async fn lock_client(&self) -> Result<MutexGuard<'_, Option<Client>>> {
        let client = self.client.lock().await;
        if client.is_none() {
            return Err(connection_closed());
        }
        Ok(client)
    }

    /// Close the client and wait for the PostgreSQL-wire task to observe the
    /// dropped sender. Waiting here is important for PGlite's bounded server:
    /// a subsequent filesystem may connect immediately after shutdown without
    /// racing the old socket's teardown.
    async fn close(&self) -> Result<()> {
        let client = self.client.lock().await.take();
        drop(client);
        let connection = self.connection.lock().await.take();
        if let Some(connection) = connection {
            let _ = connection.await;
        }
        Ok(())
    }

    async fn ensure_metadata_row(&self) -> Result<()> {
        let client = self.lock_client().await?;
        client
            .as_ref()
            .ok_or_else(connection_closed)?
            .execute_typed(
                "INSERT INTO mount_rs_metadata
                    (volume_key, revision, namespace, owner, fence, expires)
                 VALUES ($1, 0, NULL, NULL, 0, 0)
                 ON CONFLICT (volume_key) DO NOTHING",
                &[(&self.volume_key, Type::TEXT)],
            )
            .await
            .map_err(postgres_error)?;
        Ok(())
    }

    async fn flush(&self) -> Result<()> {
        // Successful execute/commit calls already received the server's
        // acknowledgement. This round trip is the provider barrier and also
        // propagates a dead connection to fsync callers.
        let client = self.lock_client().await?;
        client
            .as_ref()
            .ok_or_else(connection_closed)?
            .batch_execute("SELECT 1")
            .await
            .map_err(postgres_error)
    }
}

/// Fenced single-writer metadata persisted in an external PGlite database.
/// Namespace JSON contains attributes and block references only; file bytes
/// are stored by [`PgliteBlockStore`].
#[derive(Clone)]
pub struct PgliteMetadataStore(Database);

impl PgliteMetadataStore {
    /// Connect to a PGlite PostgreSQL-wire endpoint and initialize the
    /// provider's metadata table. The endpoint is expected to provide the
    /// desired external durability policy.
    pub async fn connect(connection_string: &str) -> Result<Self> {
        Self::connect_with_options(connection_string, PgliteStorageOptions::default()).await
    }

    pub async fn connect_with_key(
        connection_string: &str,
        volume_key: impl Into<String>,
    ) -> Result<Self> {
        Self::connect_with_options(connection_string, PgliteStorageOptions::new(volume_key)).await
    }

    pub async fn connect_with_options(
        connection_string: &str,
        options: PgliteStorageOptions,
    ) -> Result<Self> {
        let database = Database::connect(connection_string, METADATA_SCHEMA, options).await?;
        database.ensure_metadata_row().await?;
        Ok(Self(database))
    }

    /// Close the underlying PostgreSQL-wire client. This is idempotent and is
    /// intended for owners that retain the store behind an `Arc`, such as the
    /// N-API chunked filesystem shutdown callback.
    pub async fn close(&self) -> Result<()> {
        self.0.close().await
    }
}

/// Immutable blocks persisted in a table independent from metadata.
#[derive(Clone)]
pub struct PgliteBlockStore(Database);

impl PgliteBlockStore {
    /// Connect to a PGlite PostgreSQL-wire endpoint and initialize the
    /// provider's block table.
    pub async fn connect(connection_string: &str) -> Result<Self> {
        Self::connect_with_options(connection_string, PgliteStorageOptions::default()).await
    }

    pub async fn connect_with_key(
        connection_string: &str,
        volume_key: impl Into<String>,
    ) -> Result<Self> {
        Self::connect_with_options(connection_string, PgliteStorageOptions::new(volume_key)).await
    }

    pub async fn connect_with_options(
        connection_string: &str,
        options: PgliteStorageOptions,
    ) -> Result<Self> {
        Ok(Self(
            Database::connect(connection_string, BLOCK_SCHEMA, options).await?,
        ))
    }

    /// Close the underlying PostgreSQL-wire client. This is idempotent.
    pub async fn close(&self) -> Result<()> {
        self.0.close().await
    }
}

fn connection_closed() -> FsError {
    FsError::new(ErrorCode::Ebadf).with_syscall("PGlite connection")
}

fn ttl_ms(ttl: Duration) -> Result<i64> {
    let value = i64::try_from(ttl.as_millis()).map_err(|_| FsError::new(ErrorCode::Einval))?;
    if value == 0 {
        return Err(FsError::new(ErrorCode::Einval));
    }
    Ok(value)
}

fn lease_numbers(lease: &WriterLease) -> Result<(i64, i64)> {
    Ok((
        i64::try_from(lease.fence).map_err(|_| stale())?,
        i64::try_from(lease.expires_at_ms).map_err(|_| stale())?,
    ))
}

fn stale() -> FsError {
    FsError::new(ErrorCode::Estale).with_syscall("metadata lease")
}

fn nonnegative(value: i64, field: &str) -> Result<u64> {
    u64::try_from(value).map_err(|_| backend_error(format!("invalid PGlite {field}")))
}

#[async_trait]
impl MetadataStore for PgliteMetadataStore {
    fn durable(&self) -> bool {
        self.0.durable
    }

    async fn load(&self) -> Result<LoadedMetadata> {
        let client = self.0.lock_client().await?;
        let row = client
            .as_ref()
            .ok_or_else(connection_closed)?
            .query_typed_opt(
                "SELECT revision, namespace FROM mount_rs_metadata WHERE volume_key = $1",
                &[(&self.0.volume_key, Type::TEXT)],
            )
            .await
            .map_err(postgres_error)?
            .ok_or_else(|| backend_error("PGlite metadata row is missing"))?;
        let revision = nonnegative(row.get::<_, i64>(0), "metadata revision")?;
        let namespace = row
            .get::<_, Option<String>>(1)
            .map(|json| serde_json::from_str(&json))
            .transpose()
            .map_err(backend_error)?;
        Ok(LoadedMetadata {
            revision,
            namespace,
        })
    }

    async fn acquire_writer(&self, owner: &str, ttl: Duration) -> Result<WriterLease> {
        if owner.is_empty() {
            return Err(FsError::new(ErrorCode::Einval));
        }
        let ttl = ttl_ms(ttl)?;
        let owner = owner.to_owned();
        let sql = format!(
            "UPDATE mount_rs_metadata SET owner=$2, fence=fence+1, expires={NOW}+$3
             WHERE volume_key=$1 AND (owner IS NULL OR expires<={NOW})
               AND fence<9223372036854775807 AND $3<=9223372036854775807-{NOW}
             RETURNING fence, expires"
        );
        let client = self.0.lock_client().await?;
        let row = client
            .as_ref()
            .ok_or_else(connection_closed)?
            .query_typed_opt(
                &sql,
                &[
                    (&self.0.volume_key, Type::TEXT),
                    (&owner, Type::TEXT),
                    (&ttl, Type::INT8),
                ],
            )
            .await
            .map_err(postgres_error)?
            .ok_or_else(|| FsError::new(ErrorCode::Eagain).with_syscall("acquire writer"))?;
        Ok(WriterLease {
            owner,
            fence: nonnegative(row.get(0), "metadata fence")?,
            expires_at_ms: nonnegative(row.get(1), "metadata expiry")?,
        })
    }

    async fn renew_writer(&self, lease: &WriterLease, ttl: Duration) -> Result<WriterLease> {
        let ttl = ttl_ms(ttl)?;
        let (fence, expires) = lease_numbers(lease)?;
        let sql = format!(
            "UPDATE mount_rs_metadata SET expires={NOW}+$5
             WHERE volume_key=$1 AND owner=$2 AND fence=$3 AND expires=$4 AND expires>{NOW}
               AND $5<=9223372036854775807-{NOW}
             RETURNING expires"
        );
        let client = self.0.lock_client().await?;
        let row = client
            .as_ref()
            .ok_or_else(connection_closed)?
            .query_typed_opt(
                &sql,
                &[
                    (&self.0.volume_key, Type::TEXT),
                    (&lease.owner, Type::TEXT),
                    (&fence, Type::INT8),
                    (&expires, Type::INT8),
                    (&ttl, Type::INT8),
                ],
            )
            .await
            .map_err(postgres_error)?
            .ok_or_else(stale)?;
        Ok(WriterLease {
            expires_at_ms: nonnegative(row.get(0), "metadata expiry")?,
            ..lease.clone()
        })
    }

    async fn release_writer(&self, lease: &WriterLease) -> Result<()> {
        let (fence, expires) = lease_numbers(lease)?;
        let sql = format!(
            "UPDATE mount_rs_metadata SET owner=NULL, expires=0
             WHERE volume_key=$1 AND owner=$2 AND fence=$3 AND expires=$4 AND expires>{NOW}"
        );
        let client = self.0.lock_client().await?;
        let changed = client
            .as_ref()
            .ok_or_else(connection_closed)?
            .execute_typed(
                &sql,
                &[
                    (&self.0.volume_key, Type::TEXT),
                    (&lease.owner, Type::TEXT),
                    (&fence, Type::INT8),
                    (&expires, Type::INT8),
                ],
            )
            .await
            .map_err(postgres_error)?;
        if changed != 1 {
            return Err(stale());
        }
        Ok(())
    }

    async fn publish(
        &self,
        expected_revision: u64,
        lease: &WriterLease,
        namespace: Namespace,
    ) -> Result<u64> {
        let expected =
            i64::try_from(expected_revision).map_err(|_| FsError::new(ErrorCode::Eoverflow))?;
        let next = expected
            .checked_add(1)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        let (fence, expires) = lease_numbers(lease)?;
        let namespace = serde_json::to_string(&namespace).map_err(backend_error)?;

        let mut client = self.0.lock_client().await?;
        let tx = client
            .as_mut()
            .ok_or_else(connection_closed)?
            .transaction()
            .await
            .map_err(postgres_error)?;
        let state = match tx
            .query_typed_opt(
                &format!(
                    "SELECT
                         (owner IS NOT DISTINCT FROM $2 AND fence=$3 AND expires=$4
                          AND expires>{NOW}) AS lease_valid,
                         (revision=$5) AS revision_valid
                     FROM mount_rs_metadata WHERE volume_key=$1 FOR UPDATE"
                ),
                &[
                    (&self.0.volume_key, Type::TEXT),
                    (&lease.owner, Type::TEXT),
                    (&fence, Type::INT8),
                    (&expires, Type::INT8),
                    (&expected, Type::INT8),
                ],
            )
            .await
        {
            Ok(Some(row)) => (row.get::<_, bool>(0), row.get::<_, bool>(1)),
            Ok(None) => {
                let _ = tx.rollback().await;
                return Err(backend_error("PGlite metadata row is missing"));
            }
            Err(error) => {
                let _ = tx.rollback().await;
                return Err(postgres_error(error));
            }
        };
        if !state.0 {
            let _ = tx.rollback().await;
            return Err(stale());
        }
        if !state.1 {
            let _ = tx.rollback().await;
            return Err(FsError::new(ErrorCode::Eagain).with_syscall("publish metadata"));
        }

        let changed = match tx
            .execute_typed(
                &format!(
                    "UPDATE mount_rs_metadata SET revision=$2, namespace=$3
                     WHERE volume_key=$1 AND revision=$4
                       AND owner IS NOT DISTINCT FROM $5 AND fence=$6 AND expires=$7
                       AND expires>{NOW}"
                ),
                &[
                    (&self.0.volume_key, Type::TEXT),
                    (&next, Type::INT8),
                    (&namespace, Type::TEXT),
                    (&expected, Type::INT8),
                    (&lease.owner, Type::TEXT),
                    (&fence, Type::INT8),
                    (&expires, Type::INT8),
                ],
            )
            .await
        {
            Ok(changed) => changed,
            Err(error) => {
                let _ = tx.rollback().await;
                return Err(postgres_error(error));
            }
        };
        if changed != 1 {
            let _ = tx.rollback().await;
            // The row was locked and the revision was already checked. A
            // failed fenced update therefore means the lease expired while
            // the transaction was completing (or the backend violated the
            // row-lock invariant); fail closed as stale rather than allowing
            // a caller to retry a potentially fenced publication.
            return Err(stale());
        }
        tx.commit().await.map_err(postgres_error)?;
        Ok(next as u64)
    }

    async fn flush(&self) -> Result<()> {
        self.0.flush().await
    }
}

#[async_trait]
impl BlockStore for PgliteBlockStore {
    fn durable(&self) -> bool {
        self.0.durable
    }

    async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
        let bytes = bytes.to_vec();
        let client = self.0.lock_client().await?;
        // PostgreSQL's built-in md5/encode functions give the same opaque
        // identity for equal bytes without requiring a PGlite extension.
        // A collision is checked below and never aliases different content.
        let row = client
            .as_ref()
            .ok_or_else(connection_closed)?
            .query_typed_opt("SELECT md5(encode($1, 'hex'))", &[(&bytes, Type::BYTEA)])
            .await
            .map_err(postgres_error)?
            .ok_or_else(|| backend_error("PGlite did not return a block identity"))?;
        let id = row.get::<_, String>(0);
        let changed = client
            .as_ref()
            .ok_or_else(connection_closed)?
            .execute_typed(
                "INSERT INTO mount_rs_blocks (volume_key, id, bytes) VALUES ($1, $2, $3)
                 ON CONFLICT (volume_key, id) DO NOTHING",
                &[
                    (&self.0.volume_key, Type::TEXT),
                    (&id, Type::TEXT),
                    (&bytes, Type::BYTEA),
                ],
            )
            .await
            .map_err(postgres_error)?;
        if changed == 1 {
            return Ok(BlockId(id));
        }

        let existing = client
            .as_ref()
            .ok_or_else(connection_closed)?
            .query_typed_opt(
                "SELECT bytes FROM mount_rs_blocks WHERE volume_key=$1 AND id=$2",
                &[(&self.0.volume_key, Type::TEXT), (&id, Type::TEXT)],
            )
            .await
            .map_err(postgres_error)?
            .ok_or_else(|| backend_error("PGlite block conflict disappeared"))?
            .get::<_, Vec<u8>>(0);
        if existing == bytes {
            Ok(BlockId(id))
        } else {
            Err(backend_error("PGlite block identity collision"))
        }
    }

    async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
        let client = self.0.lock_client().await?;
        client
            .as_ref()
            .ok_or_else(connection_closed)?
            .query_typed_opt(
                "SELECT bytes FROM mount_rs_blocks WHERE volume_key=$1 AND id=$2",
                &[(&self.0.volume_key, Type::TEXT), (&id.0, Type::TEXT)],
            )
            .await
            .map_err(postgres_error)?
            .map(|row| row.get::<_, Vec<u8>>(0))
            .ok_or_else(|| FsError::new(ErrorCode::Enoent).with_syscall("get block"))
    }

    async fn flush(&self) -> Result<()> {
        self.0.flush().await
    }

    async fn delete(&self, id: &BlockId) -> Result<()> {
        let client = self.0.lock_client().await?;
        client
            .as_ref()
            .ok_or_else(connection_closed)?
            .execute_typed(
                "DELETE FROM mount_rs_blocks WHERE volume_key=$1 AND id=$2",
                &[(&self.0.volume_key, Type::TEXT), (&id.0, Type::TEXT)],
            )
            .await
            .map_err(postgres_error)?;
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::net::{SocketAddr, TcpStream};
    use std::process::{Child, Command, Stdio};
    use std::thread;
    use std::time::Duration;

    pub(crate) struct PgliteServer {
        child: Child,
        connection_string: String,
    }

    impl PgliteServer {
        pub(crate) fn start() -> Self {
            let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/pglite/server.mjs");
            assert!(
                script.is_file(),
                "PGlite server helper is missing: {}",
                script.display()
            );
            let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let port = listener.local_addr().unwrap().port();
            drop(listener);
            let mut child = Command::new("node")
                .arg(script)
                .env("PGLITE_PORT", port.to_string())
                .env("PGLITE_MAX_CONNECTIONS", "8")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap_or_else(|error| panic!("start node PGlite helper: {error}"));
            let address = SocketAddr::from(([127, 0, 0, 1], port));
            for _ in 0..300 {
                if TcpStream::connect_timeout(&address, Duration::from_millis(50)).is_ok() {
                    return Self {
                        child,
                        connection_string: format!(
                            "postgresql://postgres:postgres@127.0.0.1:{port}/postgres?sslmode=disable"
                        ),
                    };
                }
                if let Ok(Some(status)) = child.try_wait() {
                    panic!("PGlite helper exited before becoming ready: {status}");
                }
                thread::sleep(Duration::from_millis(50));
            }
            let _ = child.kill();
            panic!("timed out waiting for PGlite helper on {address}");
        }

        pub(crate) fn connection_string(&self) -> &str {
            &self.connection_string
        }
    }

    impl Drop for PgliteServer {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::PgliteServer;
    use super::*;
    use mount_rs_core::chunking::{Chunker, FixedSizeChunker};
    use mount_rs_core::storage::{BlockExtent, FileLayout, NodeData, NodeMetadata};
    use mount_rs_core::{FsDriver, MemoryFs};
    use std::collections::BTreeMap;

    async fn namespace(block: BlockId) -> Namespace {
        let stats = MemoryFs::empty().stat("/").await.unwrap();
        let root = stats.ino;
        let root_stats = mount_rs_core::Stats { nlink: 2, ..stats };
        let file = NodeMetadata {
            stats: mount_rs_core::Stats {
                ino: root + 1,
                mode: mount_rs_core::S_IFREG | 0o644,
                nlink: 1,
                size: 3,
                blocks: 1,
                ..root_stats.clone()
            },
            data: NodeData::File(FileLayout {
                chunker: FixedSizeChunker::new(4096).unwrap().config(),
                extents: vec![BlockExtent {
                    file_offset: 0,
                    block,
                    block_offset: 0,
                    length: 3,
                }],
            }),
        };
        let namespace = Namespace {
            format_version: 1,
            root,
            next_inode: root + 2,
            default_uid: 0,
            default_gid: 0,
            umask: 0o022,
            default_chunker: FixedSizeChunker::new(4096).unwrap().config(),
            nodes: BTreeMap::from([
                (
                    root,
                    NodeMetadata {
                        stats: root_stats,
                        data: NodeData::Directory {
                            entries: vec![mount_rs_core::storage::DirectoryEntry {
                                name: "file".to_owned(),
                                inode: root + 1,
                            }],
                        },
                    },
                ),
                (root + 1, file),
            ]),
        };
        namespace.validate().unwrap();
        namespace
    }

    #[test]
    #[ignore = "requires the isolated tests/pglite Node server and its dependencies"]
    fn split_stores_enforce_durability_fencing_cas_and_immutable_blocks() {
        let server = PgliteServer::start();
        let connection_string = server.connection_string();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let options = PgliteStorageOptions::new("pglite-storage-test");
            let metadata =
                PgliteMetadataStore::connect_with_options(connection_string, options.clone())
                    .await
                    .unwrap();
            let competitor =
                PgliteMetadataStore::connect_with_options(connection_string, options.clone())
                    .await
                    .unwrap();
            let blocks = PgliteBlockStore::connect_with_options(connection_string, options)
                .await
                .unwrap();
            assert!(!metadata.durable());
            assert!(!blocks.durable());

            let other_volume = PgliteMetadataStore::connect_with_key(
                connection_string,
                "pglite-storage-other-volume",
            )
            .await
            .unwrap();
            assert_eq!(other_volume.load().await.unwrap().revision, 0);
            let other_lease = other_volume
                .acquire_writer("other-volume", Duration::from_secs(60))
                .await
                .unwrap();
            other_volume.release_writer(&other_lease).await.unwrap();

            let assured = PgliteMetadataStore::connect_with_options(
                connection_string,
                PgliteStorageOptions::new("pglite-storage-assured").with_durable(true),
            )
            .await
            .unwrap();
            assert!(assured.durable());

            let bytes = b"pglite";
            let id = blocks.put(bytes).await.unwrap();
            assert_eq!(blocks.put(bytes).await.unwrap(), id);
            assert_eq!(blocks.get(&id).await.unwrap(), bytes);
            let orphan = blocks.put(b"orphan").await.unwrap();
            blocks.flush().await.unwrap();

            let first = metadata
                .acquire_writer("pglite-test-first", Duration::from_secs(60))
                .await
                .unwrap();
            assert!(
                competitor
                    .acquire_writer("pglite-test-second", Duration::from_secs(60))
                    .await
                    .unwrap_err()
                    .is(ErrorCode::Eagain)
            );
            let current = metadata.load().await.unwrap();
            let renewed = metadata
                .renew_writer(&first, Duration::from_secs(120))
                .await
                .unwrap();
            assert!(renewed.expires_at_ms > first.expires_at_ms);
            assert!(
                metadata
                    .publish(current.revision, &first, namespace(id.clone()).await)
                    .await
                    .unwrap_err()
                    .is(ErrorCode::Estale)
            );
            let revision = metadata
                .publish(current.revision, &renewed, namespace(id.clone()).await)
                .await
                .unwrap();
            assert_eq!(revision, current.revision + 1);
            assert!(metadata.load().await.unwrap().namespace.is_some());
            assert!(
                metadata
                    .publish(current.revision, &renewed, namespace(id.clone()).await)
                    .await
                    .unwrap_err()
                    .is(ErrorCode::Eagain)
            );
            metadata.release_writer(&renewed).await.unwrap();
            let second = competitor
                .acquire_writer("pglite-test-second", Duration::from_secs(60))
                .await
                .unwrap();
            assert!(second.fence > renewed.fence);
            assert!(
                metadata
                    .publish(revision, &renewed, namespace(id.clone()).await)
                    .await
                    .unwrap_err()
                    .is(ErrorCode::Estale)
            );

            blocks.delete(&orphan).await.unwrap();
            assert!(blocks.get(&orphan).await.unwrap_err().is(ErrorCode::Enoent));
            competitor.release_writer(&second).await.unwrap();
            metadata.flush().await.unwrap();
        });
    }
}
