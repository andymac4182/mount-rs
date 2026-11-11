//! SQLite-backed filesystem integration.

mod changes;
mod io_diagnostics;
mod owner;
mod storage;
pub use io_diagnostics::{connection_page_diagnostics, sqlite_io_diagnostics};
pub use storage::{SqliteBlockStore, SqliteMetadataStore};

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mount_rs_core::{LoadedSnapshot, Result, StateStore, backend_error, snapshot_conflict};
use mount_rs_persist::PersistedFs;
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
    /// The thread that owns the connection (see `owner`). Every operation
    /// is one job sent to it; jobs run one at a time in arrival order, and
    /// a caller waits for its durable commit without holding a runtime
    /// thread. Dropping the last clone waits for the connection to close.
    owner: Arc<owner::Owner>,
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
            owner: owner::Owner::start(connection)?,
        })
    }
}

#[async_trait]
impl StateStore for SqliteStore {
    async fn load(&self) -> Result<Option<Vec<u8>>> {
        Ok(self.load_versioned().await?.snapshot)
    }

    async fn load_versioned(&self) -> Result<LoadedSnapshot> {
        self.owner
            .run(|state| {
                let mut statement = state
                    .connection
                    .prepare_cached("SELECT revision, snapshot FROM mount_rs_state WHERE id = 1")
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
            })
            .await?
    }

    async fn save(&self, snapshot: Vec<u8>) -> Result<()> {
        self.owner
            .run(move |state| {
                state
                    .connection
                    .execute(
                        "INSERT INTO mount_rs_state (id, revision, snapshot) VALUES (1, 1, ?1)
             ON CONFLICT(id) DO UPDATE SET
                revision = mount_rs_state.revision + 1,
                snapshot = excluded.snapshot",
                        params![snapshot],
                    )
                    .map_err(backend_error)?;
                Ok(())
            })
            .await?
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
        // The comparison and the write are one statement in one job.
        let changed = self
            .owner
            .run(move |state| {
                if expected == 0 {
                    state.connection.execute(
                        "INSERT INTO mount_rs_state (id, revision, snapshot) VALUES (1, 1, ?1)
                 ON CONFLICT(id) DO UPDATE SET
                    revision = 1,
                    snapshot = excluded.snapshot
                 WHERE mount_rs_state.revision = 0",
                        params![snapshot],
                    )
                } else {
                    state.connection.execute(
                        "UPDATE mount_rs_state
                     SET revision = ?1, snapshot = ?2
                     WHERE id = 1 AND revision = ?3",
                        params![next, snapshot, expected],
                    )
                }
                .map_err(backend_error)
            })
            .await??;
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
    fn concurrent_saves_and_loads_run_one_at_a_time_and_keep_the_last_save() {
        let path = TemporaryDatabase(unique_database_path());
        let store = SqliteStore::open(&path.0).unwrap();
        let (savers, each) = (6_u8, 40_u8);
        let finished = std::sync::atomic::AtomicBool::new(false);
        // What each saver's last acknowledged save held, and how many
        // versioned saves were accepted.
        let (last, accepted): (Vec<Vec<u8>>, u64) = thread::scope(|scope| {
            let loaders: Vec<_> = (0..3)
                .map(|_| {
                    let (store, finished) = (store.clone(), &finished);
                    scope.spawn(move || {
                        let mut seen = 0_u64;
                        let mut newest = [0_u8; 6];
                        while !finished.load(Ordering::SeqCst) {
                            let loaded = block_on(store.load_versioned()).unwrap();
                            let revision: u64 = loaded.version.parse().unwrap();
                            assert!(revision >= seen, "revisions never go back");
                            seen = revision;
                            let Some(snapshot) = loaded.snapshot else {
                                assert_eq!(revision, 0);
                                continue;
                            };
                            // A whole snapshot from one save, never a mix.
                            assert_eq!(snapshot.len(), 4096);
                            let (saver, sequence) = (snapshot[0], snapshot[1]);
                            assert!(snapshot[2..].iter().all(|byte| *byte == saver ^ sequence));
                            // Each saver's own saves appear in its order.
                            let newest = &mut newest[usize::from(saver)];
                            assert!(sequence >= *newest);
                            *newest = sequence;
                        }
                    })
                })
                .collect();
            let writers: Vec<_> = (0..savers)
                .map(|saver| {
                    let store = store.clone();
                    scope.spawn(move || {
                        let mut accepted = 0_u64;
                        let mut last = Vec::new();
                        for sequence in 1..=each {
                            let mut snapshot = vec![saver ^ sequence; 4096];
                            (snapshot[0], snapshot[1]) = (saver, sequence);
                            if saver % 2 == 0 {
                                block_on(store.save(snapshot.clone())).unwrap();
                            } else {
                                // Compare and swap: retried until accepted,
                                // and accepted exactly once.
                                loop {
                                    let version = block_on(store.load_versioned()).unwrap().version;
                                    match block_on(store.save_versioned(snapshot.clone(), &version))
                                    {
                                        Ok(next) => {
                                            let (next, version): (u64, u64) =
                                                (next.parse().unwrap(), version.parse().unwrap());
                                            assert_eq!(next, version + 1);
                                            break;
                                        }
                                        Err(error) => {
                                            assert_eq!(
                                                error.to_string(),
                                                snapshot_conflict("SQLite").to_string()
                                            );
                                        }
                                    }
                                }
                            }
                            accepted += 1;
                            last = snapshot;
                        }
                        (last, accepted)
                    })
                })
                .collect();
            let results: Vec<_> = writers
                .into_iter()
                .map(|writer| writer.join().unwrap())
                .collect();
            finished.store(true, Ordering::SeqCst);
            for loader in loaders {
                loader.join().unwrap();
            }
            (
                results.iter().map(|(last, _)| last.clone()).collect(),
                results.iter().map(|(_, accepted)| accepted).sum(),
            )
        });
        // Every acknowledged save advanced the revision by one: none was
        // lost and none ran inside another.
        let loaded = block_on(store.load_versioned()).unwrap();
        assert_eq!(accepted, u64::from(savers) * u64::from(each));
        assert_eq!(loaded.version, accepted.to_string());
        // What is stored is the last save acknowledged, which is the last
        // save of whichever saver finished last.
        let stored = loaded.snapshot.unwrap();
        assert!(last.contains(&stored));
        // It is durable in the file once the store is dropped.
        drop(store);
        let reopened = SqliteStore::open(&path.0).unwrap();
        assert_eq!(block_on(reopened.load()).unwrap().unwrap(), stored);
        assert_eq!(
            block_on(reopened.load_versioned()).unwrap().version,
            accepted.to_string()
        );
    }

    #[test]
    fn a_save_whose_caller_gave_up_is_still_stored_and_later_saves_follow_it() {
        let store = SqliteStore::in_memory().unwrap();
        // Hold the connection so that the save stays queued.
        let (release, held) = std::sync::mpsc::channel::<()>();
        let (started, wait_started) = std::sync::mpsc::channel();
        drop(store.owner.run(move |_| {
            started.send(()).unwrap();
            let _ = held.recv();
        }));
        wait_started.recv().unwrap();
        let mut abandoned = Box::pin(store.save(vec![1]));
        let mut context = Context::from_waker(std::task::Waker::noop());
        assert!(abandoned.as_mut().poll(&mut context).is_pending());
        let later = {
            let store = store.clone();
            thread::spawn(move || block_on(store.save_versioned(vec![2], "1")))
        };
        drop(abandoned);
        release.send(()).unwrap();
        // In arrival order: the abandoned save made revision 1, which the
        // versioned save expected.
        assert_eq!(later.join().unwrap().unwrap(), "2");
        let loaded = block_on(store.load_versioned()).unwrap();
        assert_eq!(
            (loaded.snapshot.unwrap(), loaded.version),
            (vec![2], "2".to_owned())
        );
    }

    #[test]
    fn a_panic_on_the_connection_fails_later_snapshot_operations() {
        let store = SqliteStore::in_memory().unwrap();
        block_on(store.save(vec![1])).unwrap();
        assert!(
            store
                .owner
                .run_blocking(|_| panic!("injected job panic"))
                .is_err()
        );
        let poisoned = backend_error("SQLite storage lock poisoned").to_string();
        assert_eq!(block_on(store.load()).unwrap_err().to_string(), poisoned);
        assert_eq!(
            block_on(store.save(vec![2])).unwrap_err().to_string(),
            poisoned
        );
        assert_eq!(
            block_on(store.save_versioned(vec![2], "1"))
                .unwrap_err()
                .to_string(),
            poisoned
        );
        // Dropping a poisoned store still ends its thread.
        drop(store);
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
