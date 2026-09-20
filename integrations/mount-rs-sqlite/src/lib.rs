//! SQLite-backed filesystem integration.

mod storage;
pub use storage::{SqliteBlockStore, SqliteMetadataStore};

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use mount_rs_core::{Result, backend_error};
use mount_rs_persist::{LoadedSnapshot, PersistedFs, StateStore, snapshot_conflict};
use rusqlite::{Connection, params};

fn ensure_parent_directory(path: &Path) -> Result<()> {
    let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Ok(());
    };
    std::fs::create_dir_all(parent).map_err(|error| {
        backend_error(format!(
            "create SQLite parent directory {}: {error}",
            parent.display()
        ))
    })
}

#[derive(Clone)]
pub struct SqliteStore {
    connection: Arc<Mutex<Connection>>,
}

impl SqliteStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        ensure_parent_directory(path)?;
        let connection = Connection::open(path).map_err(backend_error)?;
        Self::from_connection(connection)
    }

    pub fn in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory().map_err(backend_error)?)
    }

    fn from_connection(connection: Connection) -> Result<Self> {
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(backend_error)?;
        connection
            .execute_batch(
                "PRAGMA synchronous = FULL;
            CREATE TABLE IF NOT EXISTS mount_rs_state (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                revision INTEGER NOT NULL DEFAULT 0,
                snapshot BLOB NOT NULL
            )",
            )
            .map_err(backend_error)?;
        let has_revision = {
            let mut statement = connection
                .prepare("PRAGMA table_info(mount_rs_state)")
                .map_err(backend_error)?;
            let mut rows = statement.query([]).map_err(backend_error)?;
            let mut found = false;
            while let Some(row) = rows.next().map_err(backend_error)? {
                let name: String = row.get(1).map_err(backend_error)?;
                if name == "revision" {
                    found = true;
                    break;
                }
            }
            found
        };
        if !has_revision {
            connection
                .execute(
                    "ALTER TABLE mount_rs_state
                     ADD COLUMN revision INTEGER NOT NULL DEFAULT 0",
                    [],
                )
                .map_err(backend_error)?;
        }
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }
}

#[async_trait]
impl StateStore for SqliteStore {
    async fn load(&self) -> Result<Option<Vec<u8>>> {
        Ok(self.load_versioned().await?.snapshot)
    }

    async fn load_versioned(&self) -> Result<LoadedSnapshot> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| backend_error("SQLite connection lock poisoned"))?;
        let mut statement = connection
            .prepare("SELECT revision, snapshot FROM mount_rs_state WHERE id = 1")
            .map_err(backend_error)?;
        let result = statement.query_row([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
        });
        match result {
            Ok((revision, snapshot)) => Ok(LoadedSnapshot {
                snapshot: Some(snapshot),
                version: revision.to_string(),
            }),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(LoadedSnapshot {
                snapshot: None,
                version: "0".to_owned(),
            }),
            Err(error) => Err(backend_error(error)),
        }
    }

    async fn save(&self, snapshot: Vec<u8>) -> Result<()> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| backend_error("SQLite connection lock poisoned"))?;
        connection
            .execute(
                "INSERT INTO mount_rs_state (id, revision, snapshot) VALUES (1, 1, ?1)
             ON CONFLICT(id) DO UPDATE SET
                revision = mount_rs_state.revision + 1,
                snapshot = excluded.snapshot",
                params![snapshot],
            )
            .map_err(backend_error)?;
        Ok(())
    }

    async fn save_versioned(&self, snapshot: Vec<u8>, expected_version: &str) -> Result<String> {
        let expected = expected_version
            .parse::<i64>()
            .map_err(|_| backend_error("invalid SQLite snapshot revision"))?;
        if expected < 0 {
            return Err(backend_error("invalid SQLite snapshot revision"));
        }
        let next = expected
            .checked_add(1)
            .ok_or_else(|| backend_error("SQLite snapshot revision overflow"))?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| backend_error("SQLite connection lock poisoned"))?;
        let changed = if expected == 0 {
            connection
                .execute(
                    "INSERT INTO mount_rs_state (id, revision, snapshot) VALUES (1, 1, ?1)
                 ON CONFLICT(id) DO UPDATE SET
                    revision = 1,
                    snapshot = excluded.snapshot
                 WHERE mount_rs_state.revision = 0",
                    params![snapshot],
                )
                .map_err(backend_error)?
        } else {
            connection
                .execute(
                    "UPDATE mount_rs_state
                     SET revision = ?1, snapshot = ?2
                     WHERE id = 1 AND revision = ?3",
                    params![next, snapshot, expected],
                )
                .map_err(backend_error)?
        };
        if changed != 1 {
            return Err(snapshot_conflict("SQLite"));
        }
        Ok(next.to_string())
    }
}

pub type SqliteFs = PersistedFs<SqliteStore>;

pub async fn open_sqlite(path: impl AsRef<Path>) -> Result<SqliteFs> {
    PersistedFs::open(SqliteStore::open(path)?).await
}

pub async fn open_sqlite_memory() -> Result<SqliteFs> {
    PersistedFs::open(SqliteStore::in_memory()?).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use mount_rs_core::FsDriver;
    use std::future::Future;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::task::{Context, Poll};
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn block_on<F: Future>(future: F) -> F::Output {
        let waker = std::task::Waker::noop();
        let mut context = Context::from_waker(waker);
        let mut future = Box::pin(future);
        loop {
            match Future::poll(future.as_mut(), &mut context) {
                Poll::Ready(value) => return value,
                Poll::Pending => thread::yield_now(),
            }
        }
    }

    pub(super) fn unique_database_path() -> std::path::PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "mount-rs-sqlite-{}-{}-{}.db",
            std::process::id(),
            timestamp,
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    struct TemporaryDatabase(std::path::PathBuf);

    impl Drop for TemporaryDatabase {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn file_database_reopens_bytes_links_and_metadata() {
        let path = TemporaryDatabase(unique_database_path());
        let filesystem = block_on(open_sqlite(&path.0)).unwrap();
        let file = block_on(filesystem.open("/file", "w", 0o640)).unwrap();
        block_on(file.write(&[0, 1, 254, 255], Some(0))).unwrap();
        block_on(file.close()).unwrap();
        drop(file);
        block_on(filesystem.link("/file", "/hard-link")).unwrap();
        block_on(filesystem.symlink("file", "/link")).unwrap();
        drop(filesystem);

        let reopened = block_on(open_sqlite(&path.0)).unwrap();
        let stat = block_on(reopened.stat("/file")).unwrap();
        assert_eq!(stat.mode & 0o777, 0o640);
        assert_eq!(stat.nlink, 2);
        assert!(
            block_on(reopened.lstat("/link"))
                .unwrap()
                .is_symbolic_link()
        );
        assert_eq!(block_on(reopened.readlink("/link")).unwrap(), "file");
        let file = block_on(reopened.open("/file", "r", 0)).unwrap();
        let mut bytes = [0_u8; 4];
        assert_eq!(block_on(file.read(&mut bytes, Some(0))).unwrap(), 4);
        assert_eq!(bytes, [0, 1, 254, 255]);
    }

    #[test]
    fn file_database_creates_missing_parent_directories() {
        let root = unique_database_path();
        let path = root.join("state").join("mount-rs.sqlite");
        assert!(!root.exists());

        let filesystem = block_on(open_sqlite(&path)).unwrap();
        assert!(path.is_file());
        drop(filesystem);

        std::fs::remove_dir_all(root).unwrap();
    }
}
