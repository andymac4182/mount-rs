//! PGlite integration through its PostgreSQL-compatible socket server.

use std::sync::Arc;

use async_trait::async_trait;
use mount_rs_core::{Result, backend_error};
use mount_rs_persist::{LoadedSnapshot, PersistedFs, StateStore, snapshot_conflict};
use tokio::sync::Mutex;
use tokio_postgres::types::Type;
use tokio_postgres::{Client, NoTls};

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

#[derive(Clone)]
pub struct PgliteStore {
    client: Arc<Mutex<Client>>,
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
        tokio::spawn(async move {
            let _ = connection.await;
        });
        let store = Self {
            client: Arc::new(Mutex::new(client)),
            state_key: state_key.into(),
        };
        store.init().await?;
        Ok(store)
    }

    async fn init(&self) -> Result<()> {
        self.client
            .lock()
            .await
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
        let client = self.client.lock().await;
        let row = client
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
        self.client
            .lock()
            .await
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
            self.client
                .lock()
                .await
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
            self.client
                .lock()
                .await
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
    PersistedFs::open(PgliteStore::connect(connection_string).await?).await
}

pub async fn connect_pglite_with_key(
    connection_string: &str,
    state_key: impl Into<String>,
) -> Result<PgliteFs> {
    PersistedFs::open(PgliteStore::connect_with_key(connection_string, state_key).await?).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use mount_rs_core::FsDriver;

    /// This is opt-in because it talks to the caller-provided PGlite socket.
    /// It never runs against an ambient database merely because a URL exists.
    #[test]
    #[ignore = "requires PGLITE_DATABASE_URL; run explicitly with --ignored"]
    fn configured_pglite_state_survives_reconnect() {
        let connection_string = std::env::var("PGLITE_DATABASE_URL")
            .expect("PGLITE_DATABASE_URL is required when PGlite regression is enabled");
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
            let first = connect_pglite_with_key(&connection_string, state_key.clone())
                .await
                .unwrap();
            let handle = first.open("/reopen", "w", 0o640).await.unwrap();
            handle.write(b"pglite", Some(0)).await.unwrap();
            handle.close().await.unwrap();
            drop(handle);
            drop(first);

            let second = connect_pglite_with_key(&connection_string, state_key)
                .await
                .unwrap();
            let handle = second.open("/reopen", "r", 0).await.unwrap();
            let mut bytes = [0_u8; 6];
            assert_eq!(handle.read(&mut bytes, Some(0)).await.unwrap(), 6);
            assert_eq!(&bytes, b"pglite");
        });
    }
}
