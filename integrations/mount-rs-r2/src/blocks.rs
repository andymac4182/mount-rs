//! Immutable byte blocks backed by an [\`object_store::ObjectStore\`].
//!
//! This adapter deliberately does not store namespace metadata or a snapshot
//! manifest. Each successful \`put\` is one provider-confirmed object upload;
//! metadata providers remain responsible for publishing references to it.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures_util::StreamExt;
use mount_rs_core::storage::{BlockId, BlockReconcileReport, BlockStore};
use mount_rs_core::{ErrorCode, FsError, Result, backend_error};
use object_store::path::Path as ObjectPath;
use object_store::{ObjectStore, PutMode, PutOptions, PutPayload};

const BLOCK_ID_PREFIX: char = 'b';
const BLOCK_ID_HEX_BYTES: usize = 32;
const MAX_CREATE_ATTEMPTS: usize = 16;

static NEXT_BLOCK_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Immutable blocks stored below one object-store prefix.
#[derive(Clone)]
pub struct R2BlockStore {
    store: Arc<dyn ObjectStore>,
    prefix: ObjectPath,
    durable: bool,
}

impl R2BlockStore {
    /// Create a block store over an existing object store.
    ///
    /// \`prefix\` is normalized once and all block IDs are validated before a
    /// provider call. \`durable\` is intentionally caller-declared: an
    /// in-memory object store is volatile, while a configured R2 endpoint is
    /// normally durable. The adapter never guesses from the object-store
    /// implementation type.
    pub fn new(
        store: Arc<dyn ObjectStore>,
        prefix: impl Into<String>,
        durable: bool,
    ) -> Result<Self> {
        Ok(Self {
            store,
            prefix: validate_prefix(&prefix.into())?,
            durable,
        })
    }

    /// Build a durable block store using the same signed S3-compatible client
    /// as [\`crate::R2Store\`].
    pub fn from_config(config: &crate::R2Config, prefix: impl Into<String>) -> Result<Self> {
        Self::new(config.build_store()?, prefix, true)
    }

    /// Return the configured scope for diagnostics and provider setup tests.
    pub fn prefix(&self) -> &str {
        self.prefix.as_ref()
    }

    fn object_path(&self, id: &BlockId) -> Result<ObjectPath> {
        validate_block_id(id)?;
        Ok(ObjectPath::from(format!("{}/{}", self.prefix, id.0)))
    }
}

#[async_trait]
impl BlockStore for R2BlockStore {
    fn durable(&self) -> bool {
        self.durable
    }

    async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
        for _ in 0..MAX_CREATE_ATTEMPTS {
            let id = BlockId(next_block_id());
            let path = self.object_path(&id)?;
            let result = self
                .store
                .put_opts(
                    &path,
                    PutPayload::from(bytes.to_vec()),
                    PutOptions {
                        mode: PutMode::Create,
                        ..Default::default()
                    },
                )
                .await;
            match result {
                Ok(_) => return Ok(id),
                // A process-local sequence is only a collision avoidance
                // hint. The provider's conditional create is the authority
                // when multiple writers generate the same candidate ID.
                Err(object_store::Error::AlreadyExists { .. })
                | Err(object_store::Error::Precondition { .. }) => continue,
                Err(error) => return Err(backend_error(format!("put R2 block: {error}"))),
            }
        }
        Err(FsError::new(ErrorCode::Eagain)
            .with_syscall("put block")
            .with_message("exhausted immutable block ID collision retries"))
    }

    async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
        let path = self.object_path(id)?;
        let result = self.store.get(&path).await.map_err(map_get_error)?;
        result
            .bytes()
            .await
            .map(|bytes| bytes.to_vec())
            .map_err(|error| backend_error(format!("read R2 block: {error}")))
    }

    /// \`put_opts\` does not return until the object store has accepted the
    /// complete object. There are therefore no deferred uploads to drain;
    /// this is the confirmed-upload barrier required before metadata publish.
    async fn flush(&self) -> Result<()> {
        Ok(())
    }

    async fn delete(&self, id: &BlockId) -> Result<()> {
        let path = self.object_path(id)?;
        self.store.head(&path).await.map_err(map_not_found)?;
        self.store.delete(&path).await.map_err(map_not_found)
    }

    /// Reconcile only objects in this block-store prefix. Objects newer than
    /// the supplied grace period are retained because they may belong to an
    /// in-flight or ambiguous publication whose metadata result has not been
    /// reconciled.
    async fn reconcile(
        &self,
        live: &BTreeSet<BlockId>,
        grace: std::time::Duration,
    ) -> Result<BlockReconcileReport> {
        if grace.is_zero() {
            return Err(FsError::new(ErrorCode::Einval)
                .with_syscall("reconcile blocks")
                .with_message("reconciliation grace period must be positive"));
        }
        let mut listing = self.store.list(Some(&self.prefix));
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let cutoff_ms = now_ms.saturating_sub(grace.as_millis());
        let prefix = format!("{}/", self.prefix);
        let mut report = BlockReconcileReport::default();

        while let Some(object) = listing.next().await {
            let object = object.map_err(|error| {
                backend_error(format!("list R2 blocks for reconciliation: {error}"))
            })?;
            let Some(relative) = object.location.as_ref().strip_prefix(&prefix) else {
                continue;
            };
            if relative.is_empty() || relative.contains('/') {
                continue;
            }
            let id = BlockId(relative.to_owned());
            if validate_block_id(&id).is_err() {
                continue;
            }
            report.scanned = report.scanned.saturating_add(1);
            if live.contains(&id) {
                report.protected = report.protected.saturating_add(1);
                continue;
            }
            let is_recent = u128::try_from(object.last_modified.timestamp_millis())
                .is_ok_and(|timestamp_ms| timestamp_ms >= cutoff_ms);
            if is_recent {
                report.recent = report.recent.saturating_add(1);
                continue;
            }
            match self.store.delete(&object.location).await {
                Ok(()) | Err(object_store::Error::NotFound { .. }) => {
                    report.deleted = report.deleted.saturating_add(1);
                }
                Err(error) => {
                    return Err(backend_error(format!(
                        "delete unreferenced R2 block during reconciliation: {error}"
                    )));
                }
            }
        }
        Ok(report)
    }
}

fn validate_prefix(prefix: &str) -> Result<ObjectPath> {
    let normalized = prefix.trim_matches('/');
    if normalized.is_empty() || normalized.contains('\0') {
        return Err(invalid_scope("block prefix must be non-empty"));
    }
    if normalized
        .split('/')
        .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(invalid_scope(
            "block prefix contains an unsafe path component",
        ));
    }
    Ok(ObjectPath::from(normalized.to_owned()))
}

fn validate_block_id(id: &BlockId) -> Result<()> {
    let mut chars = id.0.chars();
    if chars.next() != Some(BLOCK_ID_PREFIX)
        || id.0.len() != 1 + BLOCK_ID_HEX_BYTES
        || !chars.all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
    {
        return Err(invalid_scope("invalid R2 block ID"));
    }
    Ok(())
}

fn invalid_scope(message: &'static str) -> FsError {
    FsError::new(ErrorCode::Einval)
        .with_syscall("block path")
        .with_message(message)
}

fn map_not_found(error: object_store::Error) -> FsError {
    match error {
        object_store::Error::NotFound { .. } => {
            FsError::new(ErrorCode::Enoent).with_syscall("block object")
        }
        error => backend_error(format!("R2 block object: {error}")),
    }
}

fn map_get_error(error: object_store::Error) -> FsError {
    map_not_found(error)
}

fn next_block_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_nanos()).ok())
        .unwrap_or_default();
    let sequence = NEXT_BLOCK_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{BLOCK_ID_PREFIX}{timestamp:016x}{sequence:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use mount_rs_core::storage::BlockStore;
    use object_store::memory::InMemory;

    async fn stores() -> (R2BlockStore, R2BlockStore, Arc<InMemory>) {
        let object_store = Arc::new(InMemory::new());
        let first = R2BlockStore::new(object_store.clone(), "vol-a/blocks", false).unwrap();
        let second = R2BlockStore::new(object_store.clone(), "vol-b/blocks", false).unwrap();
        (first, second, object_store)
    }

    #[tokio::test]
    async fn puts_are_confirmed_immutable_and_scoped() {
        let (first, second, object_store) = stores().await;
        assert!(!first.durable());
        assert_eq!(first.prefix(), "vol-a/blocks");

        let first_id = first.put(b"first bytes").await.unwrap();
        let second_id = first.put(b"first bytes").await.unwrap();
        assert_ne!(first_id, second_id, "each upload receives a unique ID");
        assert_eq!(first.get(&first_id).await.unwrap(), b"first bytes");
        assert_eq!(first.get(&second_id).await.unwrap(), b"first bytes");
        first.flush().await.unwrap();

        assert!(
            second
                .get(&first_id)
                .await
                .unwrap_err()
                .is(ErrorCode::Enoent)
        );
        assert!(
            second
                .delete(&first_id)
                .await
                .unwrap_err()
                .is(ErrorCode::Enoent)
        );
        assert_eq!(first.get(&first_id).await.unwrap(), b"first bytes");

        let listing = object_store
            .list_with_delimiter(Some(&ObjectPath::from("vol-a/blocks")))
            .await
            .unwrap();
        assert_eq!(listing.objects.len(), 2);
        assert!(
            listing
                .objects
                .iter()
                .all(|object| object.location.as_ref().starts_with("vol-a/blocks/b"))
        );
    }

    #[tokio::test]
    async fn volatility_is_explicit_and_missing_or_invalid_blocks_fail_closed() {
        let object_store: Arc<dyn ObjectStore> = Arc::new(InMemory::new());
        let volatile = R2BlockStore::new(object_store.clone(), "blocks", false).unwrap();
        let declared_durable = R2BlockStore::new(object_store, "blocks", true).unwrap();
        assert!(!volatile.durable());
        assert!(declared_durable.durable());

        let missing = BlockId("b00000000000000000000000000000000".to_owned());
        assert!(
            volatile
                .get(&missing)
                .await
                .unwrap_err()
                .is(ErrorCode::Enoent)
        );
        assert!(
            volatile
                .delete(&missing)
                .await
                .unwrap_err()
                .is(ErrorCode::Enoent)
        );
        for invalid in [
            BlockId("../other".to_owned()),
            BlockId("bUPPER00000000000000000000000000".to_owned()),
            BlockId("b0000/other".to_owned()),
        ] {
            assert!(
                volatile
                    .get(&invalid)
                    .await
                    .unwrap_err()
                    .is(ErrorCode::Einval)
            );
            assert!(
                volatile
                    .delete(&invalid)
                    .await
                    .unwrap_err()
                    .is(ErrorCode::Einval)
            );
        }
        assert!(R2BlockStore::new(Arc::new(InMemory::new()), "blocks/../other", false,).is_err());
    }

    #[tokio::test]
    async fn delete_is_confirmed_and_does_not_alias_another_prefix() {
        let (first, second, _) = stores().await;
        let id = first.put(&[0, 255, 3]).await.unwrap();
        first.delete(&id).await.unwrap();
        assert!(first.get(&id).await.unwrap_err().is(ErrorCode::Enoent));
        assert!(second.get(&id).await.unwrap_err().is(ErrorCode::Enoent));
        assert!(first.delete(&id).await.unwrap_err().is(ErrorCode::Enoent));
    }

    #[tokio::test]
    async fn reconciliation_protects_live_and_recent_blocks_then_deletes_old_blocks() {
        let (first, _, _) = stores().await;
        let id = first.put(b"reconcile me").await.unwrap();
        let live = BTreeSet::from([id.clone()]);

        let protected = first
            .reconcile(&live, std::time::Duration::from_secs(60))
            .await
            .unwrap();
        assert_eq!(protected.scanned, 1);
        assert_eq!(protected.protected, 1);
        assert_eq!(protected.recent, 0);
        assert_eq!(protected.deleted, 0);

        let recent = first
            .reconcile(&BTreeSet::new(), std::time::Duration::from_secs(60))
            .await
            .unwrap();
        assert_eq!(recent.recent, 1);
        assert_eq!(recent.deleted, 0);
        assert_eq!(first.get(&id).await.unwrap(), b"reconcile me");

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let deleted = first
            .reconcile(&BTreeSet::new(), std::time::Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(deleted.deleted, 1);
        assert!(first.get(&id).await.unwrap_err().is(ErrorCode::Enoent));
    }
}
