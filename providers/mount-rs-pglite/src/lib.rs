//! PGlite integration through its PostgreSQL-compatible socket server.

use std::sync::Arc;

use async_trait::async_trait;
use mount_rs_core::{
    ErrorCode, FsError, LoadedSnapshot, Result, StateStore, backend_error, snapshot_conflict,
};
use mount_rs_persist::PersistedFs;
use tokio::sync::{Mutex, MutexGuard, watch};
use tokio::task::JoinHandle;
use tokio_postgres::types::Type;
use tokio_postgres::{Client, NoTls};

mod storage;

pub use storage::{PgliteBlockStore, PgliteMetadataStore, PgliteStorageOptions};

fn postgres_error(error: tokio_postgres::Error) -> mount_rs_core::FsError {
    if let Some(database) = error.as_db_error() {
        backend_error(format!(
            "PGlite {}: {}",
            database.code().code(),
            database.message()
        ))
    } else {
        backend_error(error)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CloseState {
    Open,
    Closing,
    Closed,
}

#[derive(Clone)]
pub(crate) struct CloseGate {
    state: watch::Sender<CloseState>,
}

impl CloseGate {
    fn new() -> Self {
        let (state, _receiver) = watch::channel(CloseState::Open);
        Self { state }
    }

    fn start(&self) -> bool {
        let mut started = false;
        self.state.send_if_modified(|state| {
            if *state != CloseState::Open {
                return false;
            }
            *state = CloseState::Closing;
            started = true;
            true
        });
        started
    }

    fn is_open(&self) -> bool {
        *self.state.borrow() == CloseState::Open
    }

    #[cfg(test)]
    fn is_closing(&self) -> bool {
        *self.state.borrow() == CloseState::Closing
    }

    fn complete(&self) {
        self.state.send_modify(|state| *state = CloseState::Closed);
    }

    async fn wait(&self) {
        let mut receiver = self.state.subscribe();
        loop {
            if *receiver.borrow() == CloseState::Closed {
                return;
            }
            if receiver.changed().await.is_err() {
                // The sender is owned by the store and by the spawned
                // teardown task, so this is unreachable during normal use.
                // Never report close completion if that invariant is broken.
                std::future::pending::<()>().await;
            }
        }
    }
}

#[derive(Clone)]
pub struct PgliteStore {
    client: Arc<Mutex<Option<Client>>>,
    connection: Arc<Mutex<Option<JoinHandle<()>>>>,
    close_gate: CloseGate,
    state_key: String,
}

impl PgliteStore {
    pub async fn connect(connection_string: &str) -> Result<Self> {
        Self::connect_with_key(connection_string, "mount-rs").await
    }

    pub async fn connect_with_key(
        connection_string: &str,
        state_key: impl Into<String>,
    ) -> Result<Self> {
        let (client, connection) = tokio_postgres::connect(connection_string, NoTls)
            .await
            .map_err(postgres_error)?;
        let connection = tokio::spawn(async move {
            let _ = connection.await;
        });
        let store = Self {
            client: Arc::new(Mutex::new(Some(client))),
            connection: Arc::new(Mutex::new(Some(connection))),
            close_gate: CloseGate::new(),
            state_key: state_key.into(),
        };
        store.init().await?;
        Ok(store)
    }

    async fn lock_client(&self) -> Result<MutexGuard<'_, Option<Client>>> {
        if !self.close_gate.is_open() {
            return Err(connection_closed());
        }
        let client = self.client.lock().await;
        if client.is_none() || !self.close_gate.is_open() {
            return Err(connection_closed());
        }
        Ok(client)
    }

    /// Close the PostgreSQL-wire client and wait for its connection task to
    /// finish. This is idempotent for explicit N-API filesystem shutdown.
    pub async fn close(&self) -> Result<()> {
        if self.close_gate.start() {
            let store = self.clone();
            // The teardown owner is detached from this caller's future. A
            // caller may be canceled after start() without abandoning the
            // client or its connection task.
            tokio::spawn(async move {
                store.finish_close().await;
            });
        }
        self.close_gate.wait().await;
        Ok(())
    }

    async fn finish_close(&self) {
        let client = self.client.lock().await.take();
        drop(client);
        let connection = self.connection.lock().await.take();
        if let Some(connection) = connection {
            let _ = connection.await;
        }
        self.close_gate.complete();
    }

    async fn init(&self) -> Result<()> {
        let client = self.lock_client().await?;
        client
            .as_ref()
            .ok_or_else(connection_closed)?
            .batch_execute(
                "CREATE TABLE IF NOT EXISTS mount_rs_state (
                id TEXT PRIMARY KEY,
                snapshot BYTEA NOT NULL,
                revision BIGINT NOT NULL DEFAULT 0
            );
            ALTER TABLE mount_rs_state
                ADD COLUMN IF NOT EXISTS revision BIGINT NOT NULL DEFAULT 0",
            )
            .await
            .map_err(postgres_error)
    }
}

fn connection_closed() -> FsError {
    FsError::new(ErrorCode::Ebadf).with_syscall("PGlite connection")
}

#[async_trait]
impl StateStore for PgliteStore {
    // PGlite socket clients share a PostgreSQL backend. Named prepared
    // statements can survive another client's disconnect and collide with
    // tokio-postgres's process-local statement counter. Typed queries use the
    // unnamed statement and keep every value bound as a protocol parameter.
    async fn load(&self) -> Result<Option<Vec<u8>>> {
        Ok(self.load_versioned().await?.snapshot)
    }

    async fn load_versioned(&self) -> Result<LoadedSnapshot> {
        let client = self.lock_client().await?;
        let row = client
            .as_ref()
            .ok_or_else(connection_closed)?
            .query_typed_opt(
                "SELECT revision, snapshot FROM mount_rs_state WHERE id = $1",
                &[(&self.state_key, Type::TEXT)],
            )
            .await
            .map_err(postgres_error)?;
        Ok(match row {
            Some(row) => LoadedSnapshot {
                version: row.get::<_, i64>(0).to_string(),
                snapshot: Some(row.get::<_, Vec<u8>>(1)),
            },
            None => LoadedSnapshot {
                snapshot: None,
                version: "0".to_owned(),
            },
        })
    }

    async fn save(&self, snapshot: Vec<u8>) -> Result<()> {
        let client = self.lock_client().await?;
        client
            .as_ref()
            .ok_or_else(connection_closed)?
            .execute_typed(
                "INSERT INTO mount_rs_state (id, revision, snapshot) VALUES ($1, 1, $2)
             ON CONFLICT(id) DO UPDATE SET
                revision = mount_rs_state.revision + 1,
                snapshot = EXCLUDED.snapshot",
                &[(&self.state_key, Type::TEXT), (&snapshot, Type::BYTEA)],
            )
            .await
            .map_err(postgres_error)?;
        Ok(())
    }

    async fn save_versioned(&self, snapshot: Vec<u8>, expected_version: &str) -> Result<String> {
        let expected = expected_version
            .parse::<i64>()
            .map_err(|_| backend_error("invalid PGlite snapshot revision"))?;
        if expected < 0 {
            return Err(backend_error("invalid PGlite snapshot revision"));
        }
        let next = expected
            .checked_add(1)
            .ok_or_else(|| backend_error("PGlite snapshot revision overflow"))?;
        let changed = if expected == 0 {
            let client = self.lock_client().await?;
            client
                .as_ref()
                .ok_or_else(connection_closed)?
                .execute_typed(
                    "INSERT INTO mount_rs_state (id, revision, snapshot) VALUES ($1, 1, $2)
                 ON CONFLICT(id) DO UPDATE SET
                    revision = 1,
                    snapshot = EXCLUDED.snapshot
                 WHERE mount_rs_state.revision = 0",
                    &[(&self.state_key, Type::TEXT), (&snapshot, Type::BYTEA)],
                )
                .await
                .map_err(postgres_error)?
        } else {
            let client = self.lock_client().await?;
            client
                .as_ref()
                .ok_or_else(connection_closed)?
                .execute_typed(
                    "UPDATE mount_rs_state
                 SET revision = $1, snapshot = $2
                 WHERE id = $3 AND revision = $4",
                    &[
                        (&next, Type::INT8),
                        (&snapshot, Type::BYTEA),
                        (&self.state_key, Type::TEXT),
                        (&expected, Type::INT8),
                    ],
                )
                .await
                .map_err(postgres_error)?
        };
        if changed != 1 {
            return Err(snapshot_conflict("PGlite"));
        }
        Ok(next.to_string())
    }
}

pub type PgliteFs = PersistedFs<PgliteStore>;

pub async fn connect_pglite(connection_string: &str) -> Result<PgliteFs> {
    let (filesystem, _store) = connect_pglite_with_store(connection_string, "mount-rs").await?;
    Ok(filesystem)
}

/// Open the legacy snapshot filesystem and return a shared store handle for
/// owners that need deterministic connection shutdown. The filesystem keeps
/// its own `Arc` clone, so closing the returned store closes the live client
/// even when the filesystem is retained by JavaScript.
pub async fn connect_pglite_with_store(
    connection_string: &str,
    state_key: impl Into<String>,
) -> Result<(PgliteFs, PgliteStore)> {
    let store = PgliteStore::connect_with_key(connection_string, state_key).await?;
    let filesystem = PersistedFs::open(store.clone()).await?;
    Ok((filesystem, store))
}

pub async fn connect_pglite_with_key(
    connection_string: &str,
    state_key: impl Into<String>,
) -> Result<PgliteFs> {
    let (filesystem, _store) = connect_pglite_with_store(connection_string, state_key).await?;
    Ok(filesystem)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::test_support::PgliteServer;
    use mount_rs_core::{ErrorCode, FsDriver};

    /// This is opt-in because it talks to the caller-provided PGlite socket.
    /// It never runs against an ambient database merely because a URL exists.
    #[test]
    #[ignore = "requires the isolated tests/pglite Node server and its dependencies"]
    fn configured_pglite_state_survives_reconnect() {
        let server = PgliteServer::start();
        let connection_string = server.connection_string();
        let state_key = format!(
            "mount-rs-regression/{}/{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock before epoch")
                .as_nanos()
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build Tokio test runtime");
        runtime.block_on(async {
            let first = connect_pglite_with_key(connection_string, state_key.clone())
                .await
                .unwrap();
            let handle = first.open("/reopen", "w", 0o640).await.unwrap();
            handle.write(b"pglite", Some(0)).await.unwrap();
            handle.close().await.unwrap();
            drop(handle);
            drop(first);

            let second = connect_pglite_with_key(connection_string, state_key)
                .await
                .unwrap();
            let handle = second.open("/reopen", "r", 0).await.unwrap();
            let mut bytes = [0_u8; 6];
            assert_eq!(handle.read(&mut bytes, Some(0)).await.unwrap(), 6);
            assert_eq!(&bytes, b"pglite");
        });
    }

    /// This exercises cancellation after a close caller has been scheduled,
    /// concurrent close callers on clones, repeated idempotent calls, and the
    /// bounded server's ability to accept new clients after teardown.
    #[test]
    #[ignore = "requires the isolated tests/pglite Node server and its dependencies"]
    fn pglite_store_close_is_shared_cancellation_safe_and_bounded() {
        let server = PgliteServer::start_with_max_connections(2);
        let connection_string = server.connection_string();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build Tokio test runtime");
        runtime.block_on(async {
            let store = PgliteStore::connect_with_key(connection_string, "close-lifecycle")
                .await
                .unwrap();

            let client_guard = store.client.lock().await;
            let canceled = tokio::spawn({
                let store = store.clone();
                async move { store.close().await }
            });
            tokio::task::yield_now().await;
            assert!(store.close_gate.is_closing());
            canceled.abort();
            assert!(canceled.await.unwrap_err().is_cancelled());

            let second = tokio::spawn({
                let store = store.clone();
                async move { store.close().await }
            });
            tokio::task::yield_now().await;
            assert!(!second.is_finished());
            drop(client_guard);
            second.await.unwrap().unwrap();

            for _ in 0..3 {
                store.close().await.unwrap();
            }
            assert!(
                store
                    .load_versioned()
                    .await
                    .unwrap_err()
                    .is(ErrorCode::Ebadf)
            );

            let first_reopened =
                PgliteStore::connect_with_key(connection_string, "close-lifecycle-reopened-1")
                    .await
                    .unwrap();
            let second_reopened =
                PgliteStore::connect_with_key(connection_string, "close-lifecycle-reopened-2")
                    .await
                    .unwrap();
            first_reopened.close().await.unwrap();
            second_reopened.close().await.unwrap();
        });
    }
}
