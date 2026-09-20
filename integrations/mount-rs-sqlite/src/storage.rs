//! Independently selectable SQLite metadata and immutable block providers.
//! Each database contains one namespace. Metadata and blocks may reside in
//! different databases, or either provider may be paired with another backend.

use async_trait::async_trait;
use mount_rs_core::storage::{
    BlockId, BlockStore, LoadedMetadata, MetadataStore, Namespace, WriterLease,
};
use mount_rs_core::{ErrorCode, FsError, Result, backend_error};
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

// SQLite supplies the clock inside the same statement as lease validation.
const NOW: &str = "CAST(unixepoch('subsec') * 1000 AS INTEGER)";

#[derive(Clone)]
struct Database {
    connection: Arc<Mutex<Connection>>,
    durable: bool,
}

impl Database {
    fn open(path: Option<&Path>, schema: &str) -> Result<Self> {
        let connection = match path {
            Some(path) => Connection::open(path),
            None => Connection::open_in_memory(),
        }
        .map_err(backend_error)?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(backend_error)?;
        connection
            .execute_batch("PRAGMA synchronous=FULL;")
            .map_err(backend_error)?;
        connection.execute_batch(schema).map_err(backend_error)?;
        // Empty paths and :memory: are not durable even when passed to open().
        let durable = connection.path().is_some_and(|path| !path.is_empty());
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            durable,
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| backend_error("SQLite storage lock poisoned"))
    }

    fn flush(&self) -> Result<()> {
        // Every modifying statement is an autocommit transaction with FULL
        // synchronous durability. There are no deferred writes to flush.
        let connection = self.lock()?;
        if !connection.is_autocommit() {
            return Err(backend_error(
                "SQLite storage has an unfinished transaction",
            ));
        }
        Ok(())
    }
}

const METADATA_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS mount_rs_metadata (
 id INTEGER PRIMARY KEY CHECK(id=1),
 revision INTEGER NOT NULL CHECK(revision>=0), namespace TEXT,
 owner TEXT, fence INTEGER NOT NULL CHECK(fence>=0), expires INTEGER NOT NULL);
 INSERT OR IGNORE INTO mount_rs_metadata VALUES(1,0,NULL,NULL,0,0);";
const BLOCK_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS mount_rs_blocks (
 id TEXT PRIMARY KEY NOT NULL, bytes BLOB NOT NULL);";

/// Fenced single-writer metadata transactions; no file bytes are stored here.
#[derive(Clone)]
pub struct SqliteMetadataStore(Database);

impl SqliteMetadataStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self(Database::open(Some(path.as_ref()), METADATA_SCHEMA)?))
    }
    pub fn in_memory() -> Result<Self> {
        Ok(Self(Database::open(None, METADATA_SCHEMA)?))
    }
}

/// Immutable blocks, independently configurable from the metadata database.
#[derive(Clone)]
pub struct SqliteBlockStore(Database);

impl SqliteBlockStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self(Database::open(Some(path.as_ref()), BLOCK_SCHEMA)?))
    }
    pub fn in_memory() -> Result<Self> {
        Ok(Self(Database::open(None, BLOCK_SCHEMA)?))
    }
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

#[async_trait]
impl MetadataStore for SqliteMetadataStore {
    fn durable(&self) -> bool {
        self.0.durable
    }

    async fn load(&self) -> Result<LoadedMetadata> {
        let connection = self.0.lock()?;
        let (revision, namespace): (u64, Option<String>) = connection
            .query_row(
                "SELECT revision, namespace FROM mount_rs_metadata WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(backend_error)?;
        Ok(LoadedMetadata {
            revision,
            namespace: namespace
                .map(|json| serde_json::from_str(&json))
                .transpose()
                .map_err(backend_error)?,
        })
    }

    async fn acquire_writer(&self, owner: &str, ttl: Duration) -> Result<WriterLease> {
        if owner.is_empty() {
            return Err(FsError::new(ErrorCode::Einval));
        }
        let ttl = ttl_ms(ttl)?;
        let sql = format!(
            "UPDATE mount_rs_metadata SET owner=?1, fence=fence+1, expires={NOW}+?2
            WHERE id=1 AND (owner IS NULL OR expires<={NOW})
              AND fence<9223372036854775807 AND ?2<=9223372036854775807-{NOW}
            RETURNING fence, expires"
        );
        let connection = self.0.lock()?;
        let result = connection
            .query_row(&sql, params![owner, ttl], |row| {
                Ok(WriterLease {
                    owner: owner.to_owned(),
                    fence: row.get(0)?,
                    expires_at_ms: row.get(1)?,
                })
            })
            .optional()
            .map_err(backend_error)?;
        result.ok_or_else(|| FsError::new(ErrorCode::Eagain).with_syscall("acquire writer"))
    }

    async fn renew_writer(&self, lease: &WriterLease, ttl: Duration) -> Result<WriterLease> {
        let ttl = ttl_ms(ttl)?;
        let (fence, expires) = lease_numbers(lease)?;
        let connection = self.0.lock()?;
        let sql = format!(
            "UPDATE mount_rs_metadata SET expires={NOW}+?4
            WHERE id=1 AND owner=?1 AND fence=?2 AND expires=?3 AND expires>{NOW}
              AND ?4<=9223372036854775807-{NOW} RETURNING expires"
        );
        let expiry = connection
            .query_row(&sql, params![lease.owner, fence, expires, ttl], |row| {
                row.get(0)
            })
            .optional()
            .map_err(backend_error)?
            .ok_or_else(stale)?;
        Ok(WriterLease {
            expires_at_ms: expiry,
            ..lease.clone()
        })
    }

    async fn release_writer(&self, lease: &WriterLease) -> Result<()> {
        let (fence, expires) = lease_numbers(lease)?;
        let connection = self.0.lock()?;
        let changed = connection
            .execute(
                &format!(
                    "UPDATE mount_rs_metadata SET owner=NULL, expires=0
            WHERE id=1 AND owner=?1 AND fence=?2 AND expires=?3 AND expires>{NOW}"
                ),
                params![lease.owner, fence, expires],
            )
            .map_err(backend_error)?;
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
        let mut connection = self.0.lock()?;
        let tx = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(backend_error)?;
        let valid: bool = tx
            .query_row(
                &format!(
                    "SELECT owner=?1 AND fence=?2 AND expires=?3 AND expires>{NOW}
            FROM mount_rs_metadata WHERE id=1"
                ),
                params![lease.owner, fence, expires],
                |row| Ok(row.get::<_, Option<bool>>(0)?.unwrap_or(false)),
            )
            .map_err(backend_error)?;
        if !valid {
            return Err(stale());
        }
        let changed = tx
            .execute(
                "UPDATE mount_rs_metadata SET revision=?1, namespace=?2 WHERE id=1 AND revision=?3",
                params![next, namespace, expected],
            )
            .map_err(backend_error)?;
        if changed != 1 {
            return Err(FsError::new(ErrorCode::Eagain).with_syscall("publish metadata"));
        }
        tx.commit().map_err(backend_error)?;
        Ok(next as u64)
    }

    async fn flush(&self) -> Result<()> {
        self.0.flush()
    }
}

#[async_trait]
impl BlockStore for SqliteBlockStore {
    fn durable(&self) -> bool {
        self.0.durable
    }

    async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
        let connection = self.0.lock()?;
        // Database-generated random identities avoid accidental aliasing when
        // namespaces use distinct block databases. Collisions fail, never overwrite.
        connection.query_row("INSERT INTO mount_rs_blocks(id,bytes) VALUES(lower(hex(randomblob(32))),?1) RETURNING id",
            params![bytes], |row| row.get(0)).map(BlockId).map_err(backend_error)
    }

    async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
        self.0
            .lock()?
            .query_row(
                "SELECT bytes FROM mount_rs_blocks WHERE id=?1",
                params![id.0],
                |row| row.get(0),
            )
            .optional()
            .map_err(backend_error)?
            .ok_or_else(|| FsError::new(ErrorCode::Enoent).with_syscall("get block"))
    }

    async fn flush(&self) -> Result<()> {
        self.0.flush()
    }

    async fn delete(&self, id: &BlockId) -> Result<()> {
        self.0
            .lock()?
            .execute("DELETE FROM mount_rs_blocks WHERE id=?1", params![id.0])
            .map_err(backend_error)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mount_rs_core::{
        FsDriver, MemoryFs,
        chunking::{Chunker, FixedSizeChunker},
        storage::{NodeData, NodeMetadata},
    };
    use std::{
        collections::BTreeMap,
        future::Future,
        task::{Context, Poll, Waker},
    };

    fn run<T>(future: impl Future<Output = T>) -> T {
        let mut future = Box::pin(future);
        match future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("SQLite operations must complete synchronously"),
        }
    }

    fn namespace() -> Namespace {
        let stats = run(MemoryFs::empty().stat("/")).unwrap();
        let root = stats.ino;
        Namespace {
            format_version: 1,
            root,
            next_inode: root + 1,
            default_uid: 0,
            default_gid: 0,
            umask: 0o022,
            default_chunker: FixedSizeChunker::new(4096).unwrap().config(),
            nodes: BTreeMap::from([(
                root,
                NodeMetadata {
                    stats,
                    data: NodeData::Directory { entries: vec![] },
                },
            )]),
        }
    }

    #[test]
    fn lease_fencing_revision_conflicts_and_expiry_fail_closed() {
        let store = SqliteMetadataStore::in_memory().unwrap();
        assert!(!store.durable());
        assert!(run(store.load()).unwrap().namespace.is_none());
        assert!(
            run(store.acquire_writer("", Duration::from_secs(60)))
                .unwrap_err()
                .is(ErrorCode::Einval)
        );
        assert!(
            run(store.acquire_writer("a", Duration::ZERO))
                .unwrap_err()
                .is(ErrorCode::Einval)
        );
        let first = run(store.acquire_writer("a", Duration::from_secs(60))).unwrap();
        assert!(
            run(store.acquire_writer("a", Duration::from_secs(60)))
                .unwrap_err()
                .is(ErrorCode::Eagain)
        );
        assert_eq!(run(store.publish(0, &first, namespace())).unwrap(), 1);
        assert!(
            run(store.publish(0, &first, namespace()))
                .unwrap_err()
                .is(ErrorCode::Eagain)
        );
        let renewed = run(store.renew_writer(&first, Duration::from_secs(120))).unwrap();
        assert!(
            run(store.publish(1, &first, namespace()))
                .unwrap_err()
                .is(ErrorCode::Estale)
        );
        assert_eq!(run(store.publish(1, &renewed, namespace())).unwrap(), 2);
        // Deterministic provider-clock expiration: no timing-sensitive sleeps.
        store
            .0
            .lock()
            .unwrap()
            .execute("UPDATE mount_rs_metadata SET expires=0", [])
            .unwrap();
        assert!(
            run(store.renew_writer(&renewed, Duration::from_secs(60)))
                .unwrap_err()
                .is(ErrorCode::Estale)
        );
        assert!(
            run(store.publish(2, &renewed, namespace()))
                .unwrap_err()
                .is(ErrorCode::Estale)
        );
        let second = run(store.acquire_writer("b", Duration::from_secs(60))).unwrap();
        assert!(second.fence > renewed.fence);
        assert!(
            run(store.release_writer(&renewed))
                .unwrap_err()
                .is(ErrorCode::Estale)
        );
        assert_eq!(run(store.publish(2, &second, namespace())).unwrap(), 3);
        run(store.release_writer(&second)).unwrap();
        assert!(
            run(store.publish(3, &second, namespace()))
                .unwrap_err()
                .is(ErrorCode::Estale)
        );
        let third = run(store.acquire_writer("a", Duration::from_secs(60))).unwrap();
        assert!(third.fence > second.fence);
        run(store.flush()).unwrap();
        assert_eq!(run(store.load()).unwrap().revision, 3);
    }

    #[test]
    fn block_ids_are_immutable_and_separate_from_metadata() {
        let blocks = SqliteBlockStore::in_memory().unwrap();
        assert!(!blocks.durable());
        let a = run(blocks.put(&[0, 255, 1, 2])).unwrap();
        let b = run(blocks.put(&[9])).unwrap();
        assert_ne!(a, b);
        assert_eq!(run(blocks.get(&a)).unwrap(), [0, 255, 1, 2]);
        assert_eq!(run(blocks.get(&b)).unwrap(), [9]);
        run(blocks.flush()).unwrap();
        run(blocks.delete(&a)).unwrap();
        assert!(run(blocks.get(&a)).unwrap_err().is(ErrorCode::Enoent));
        assert_eq!(run(blocks.get(&b)).unwrap(), [9]);
        let other = SqliteBlockStore::in_memory().unwrap();
        assert!(run(other.get(&b)).unwrap_err().is(ErrorCode::Enoent));
        let tables: u64 = blocks
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name='mount_rs_metadata'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0);
    }

    #[test]
    fn separate_files_reopen_and_independent_connections_enforce_one_writer() {
        let path = super::super::tests::unique_database_path();
        let block_path = path.with_extension("blocks.db");
        let metadata = SqliteMetadataStore::open(&path).unwrap();
        let competitor = SqliteMetadataStore::open(&path).unwrap();
        let blocks = SqliteBlockStore::open(&block_path).unwrap();
        assert!(metadata.durable());
        assert!(blocks.durable());
        let lease = run(metadata.acquire_writer("first", Duration::from_secs(60))).unwrap();
        assert!(
            run(competitor.acquire_writer("second", Duration::from_secs(60)))
                .unwrap_err()
                .is(ErrorCode::Eagain)
        );
        let id = run(blocks.put(&[1, 2, 3, 0, 255])).unwrap();
        run(blocks.flush()).unwrap();
        assert_eq!(run(metadata.publish(0, &lease, namespace())).unwrap(), 1);
        run(metadata.flush()).unwrap();
        run(metadata.release_writer(&lease)).unwrap();
        let new_lease = run(competitor.acquire_writer("second", Duration::from_secs(60))).unwrap();
        assert!(new_lease.fence > lease.fence);
        assert!(
            run(metadata.publish(1, &lease, namespace()))
                .unwrap_err()
                .is(ErrorCode::Estale)
        );
        drop(metadata);
        drop(competitor);
        drop(blocks);
        let reopened = SqliteMetadataStore::open(&path).unwrap();
        assert_eq!(run(reopened.load()).unwrap().revision, 1);
        let reopened_blocks = SqliteBlockStore::open(&block_path).unwrap();
        assert_eq!(run(reopened_blocks.get(&id)).unwrap(), [1, 2, 3, 0, 255]);
        assert_eq!(
            reopened
                .0
                .lock()
                .unwrap()
                .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        drop(reopened);
        drop(reopened_blocks);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_file(block_path).unwrap();
    }
}
