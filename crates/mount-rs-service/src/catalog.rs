//! Versioned service metadata, separate from filesystem namespace metadata.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

const MAX_DOCUMENT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug)]
pub enum CatalogError {
    Storage(rusqlite::Error),
    Task(tokio::task::JoinError),
    Invalid(&'static str),
    Conflict,
}

impl fmt::Display for CatalogError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(_) | Self::Task(_) => output.write_str("catalog storage unavailable"),
            Self::Invalid(reason) => write!(output, "invalid catalog: {reason}"),
            Self::Conflict => output.write_str("catalog revision conflict"),
        }
    }
}

impl std::error::Error for CatalogError {}

impl From<rusqlite::Error> for CatalogError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Storage(error)
    }
}

impl From<tokio::task::JoinError> for CatalogError {
    fn from(error: tokio::task::JoinError) -> Self {
        Self::Task(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    Read,
    Write,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DriveKey {
    pub partition_id: String,
    pub drive_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DriveDefinition {
    pub driver: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartitionDefinition {
    pub drives: BTreeMap<String, DriveDefinition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantDefinition {
    pub partition_id: String,
    pub policy_id: String,
    pub drives: BTreeMap<String, Permission>,
    pub claim_conditions: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogSnapshot {
    pub revision: u64,
    pub partitions: BTreeMap<String, PartitionDefinition>,
    pub issuer_policies: BTreeMap<String, serde_json::Value>,
    pub grants: BTreeMap<String, GrantDefinition>,
}

impl CatalogSnapshot {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            revision: 0,
            partitions: BTreeMap::new(),
            issuer_policies: BTreeMap::new(),
            grants: BTreeMap::new(),
        }
    }

    pub fn validate(&self) -> Result<(), CatalogError> {
        for (partition_id, partition) in &self.partitions {
            validate_id(partition_id)?;
            for (drive_id, drive) in &partition.drives {
                validate_id(drive_id)?;
                if !drive.driver.is_object() {
                    return Err(CatalogError::Invalid("Drive driver must be an object"));
                }
            }
        }
        for (policy_id, policy) in &self.issuer_policies {
            validate_id(policy_id)?;
            if !policy.is_object() {
                return Err(CatalogError::Invalid("issuer policy must be an object"));
            }
        }
        for (grant_id, grant) in &self.grants {
            validate_id(grant_id)?;
            let partition = self
                .partitions
                .get(&grant.partition_id)
                .ok_or(CatalogError::Invalid("grant Partition does not exist"))?;
            if !self.issuer_policies.contains_key(&grant.policy_id) {
                return Err(CatalogError::Invalid("grant issuer policy does not exist"));
            }
            if grant.drives.is_empty() {
                return Err(CatalogError::Invalid("grant must name a Drive"));
            }
            for drive_id in grant.drives.keys() {
                if !partition.drives.contains_key(drive_id) {
                    return Err(CatalogError::Invalid("grant Drive does not exist"));
                }
            }
        }
        Ok(())
    }
}

fn validate_id(value: &str) -> Result<(), CatalogError> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(CatalogError::Invalid(
            "invalid Partition, Drive, or policy ID",
        ));
    }
    Ok(())
}

#[async_trait]
pub trait CatalogStore: Send + Sync {
    async fn load_current(&self) -> Result<CatalogSnapshot, CatalogError>;
    async fn compare_and_swap(
        &self,
        expected_revision: u64,
        next: CatalogSnapshot,
    ) -> Result<u64, CatalogError>;
}

#[derive(Debug, Clone)]
pub struct SqliteCatalog {
    path: PathBuf,
}

impl SqliteCatalog {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, CatalogError> {
        let path = path.as_ref().to_path_buf();
        let setup_path = path.clone();
        tokio::task::spawn_blocking(move || -> Result<(), CatalogError> {
            let connection = connect(&setup_path)?;
            connection.execute_batch(
                "CREATE TABLE IF NOT EXISTS service_catalog (\
                   singleton INTEGER PRIMARY KEY CHECK (singleton = 1), \
                   revision INTEGER NOT NULL CHECK (revision >= 0), \
                   document BLOB NOT NULL);",
            )?;
            let empty = serde_json::to_vec(&CatalogSnapshot::empty())
                .map_err(|_| CatalogError::Invalid("cannot encode empty catalog"))?;
            connection.execute(
                "INSERT OR IGNORE INTO service_catalog (singleton, revision, document) VALUES (1, 0, ?1)",
                params![empty],
            )?;
            Ok(())
        })
        .await??;
        Ok(Self { path })
    }

    pub async fn load_current(&self) -> Result<CatalogSnapshot, CatalogError> {
        <Self as CatalogStore>::load_current(self).await
    }

    pub async fn compare_and_swap(
        &self,
        expected_revision: u64,
        next: CatalogSnapshot,
    ) -> Result<u64, CatalogError> {
        <Self as CatalogStore>::compare_and_swap(self, expected_revision, next).await
    }
}

#[async_trait]
impl CatalogStore for SqliteCatalog {
    async fn load_current(&self) -> Result<CatalogSnapshot, CatalogError> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let connection = connect(&path)?;
            let row: Option<(i64, Vec<u8>)> = connection
                .query_row(
                    "SELECT revision, document FROM service_catalog WHERE singleton = 1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let (revision, document) = row.ok_or(CatalogError::Invalid("catalog row missing"))?;
            decode_snapshot(revision, &document)
        })
        .await?
    }

    async fn compare_and_swap(
        &self,
        expected_revision: u64,
        mut next: CatalogSnapshot,
    ) -> Result<u64, CatalogError> {
        next.validate()?;
        let revision = expected_revision
            .checked_add(1)
            .ok_or(CatalogError::Invalid("revision overflow"))?;
        next.revision = revision;
        let document = serde_json::to_vec(&next)
            .map_err(|_| CatalogError::Invalid("cannot encode catalog"))?;
        if document.len() > MAX_DOCUMENT_BYTES {
            return Err(CatalogError::Invalid("catalog document too large"));
        }
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let mut connection = connect(&path)?;
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let previous: i64 = transaction.query_row(
                "SELECT revision FROM service_catalog WHERE singleton = 1",
                [],
                |row| row.get(0),
            )?;
            if u64::try_from(previous).ok() != Some(expected_revision) {
                return Err(CatalogError::Conflict);
            }
            let revision_sql =
                i64::try_from(revision).map_err(|_| CatalogError::Invalid("revision overflow"))?;
            transaction.execute(
                "UPDATE service_catalog SET revision = ?1, document = ?2 WHERE singleton = 1",
                params![revision_sql, document],
            )?;
            transaction.commit()?;
            Ok(revision)
        })
        .await?
    }
}

fn connect(path: &Path) -> Result<Connection, CatalogError> {
    let connection = Connection::open(path)?;
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "FULL")?;
    Ok(connection)
}

fn decode_snapshot(revision: i64, document: &[u8]) -> Result<CatalogSnapshot, CatalogError> {
    if document.len() > MAX_DOCUMENT_BYTES {
        return Err(CatalogError::Invalid("catalog document too large"));
    }
    let snapshot: CatalogSnapshot = serde_json::from_slice(document)
        .map_err(|_| CatalogError::Invalid("malformed catalog document"))?;
    if u64::try_from(revision).ok() != Some(snapshot.revision) {
        return Err(CatalogError::Invalid("catalog revision mismatch"));
    }
    snapshot.validate()?;
    Ok(snapshot)
}
