//! Additive versioned-metadata capability.
//!
//! [`MetadataStore`] remains the current-head compatibility contract. This
//! module adds the durable history, snapshot publication, and read-lease
//! contract without changing that trait or requiring every existing provider
//! to implement versioning immediately. A version record stores a complete
//! validated namespace manifest; immutable file bytes remain in the selected
//! [`BlockStore`](crate::storage::BlockStore).

use crate::Result;
use crate::storage::{BlockId, MetadataStore, Namespace, WriterLease};
use async_trait::async_trait;
use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;
use std::time::Duration;

/// Stable identity of one logical filesystem timeline.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct VolumeId(pub String);

impl VolumeId {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.is_empty() || value.contains(':') || value.contains('\0') {
            return Err(crate::FsError::new(crate::ErrorCode::Einval)
                .with_message("volume id must be non-empty and colon-free"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for VolumeId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for VolumeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Stable, ordered identity of a committed version in one volume.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct VersionId {
    pub volume: VolumeId,
    pub sequence: u64,
}

impl VersionId {
    pub fn new(volume: VolumeId, sequence: u64) -> Result<Self> {
        if sequence == 0 {
            return Err(crate::FsError::new(crate::ErrorCode::Einval)
                .with_message("version sequence must be positive"));
        }
        Ok(Self { volume, sequence })
    }

    /// Stable database/wire representation. Volume IDs are colon-free, so the
    /// representation is unambiguous and does not depend on provider tokens.
    pub fn encode(&self) -> String {
        format!("{}:{}", self.volume.0, self.sequence)
    }

    pub fn decode(value: &str) -> Result<Self> {
        let (volume, sequence_text) = value.rsplit_once(':').ok_or_else(|| {
            crate::FsError::new(crate::ErrorCode::Einval)
                .with_message("invalid version id; expected volume:sequence")
        })?;
        let volume = VolumeId::new(volume.to_owned())?;
        let sequence: u64 = sequence_text.parse().map_err(|_| {
            crate::FsError::new(crate::ErrorCode::Einval).with_message("invalid version sequence")
        })?;
        if sequence.to_string() != sequence_text {
            return Err(crate::FsError::new(crate::ErrorCode::Einval)
                .with_message("version sequence must use canonical decimal text"));
        }
        Self::new(volume, sequence)
    }
}

impl<'de> Deserialize<'de> for VersionId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Fields {
            volume: VolumeId,
            sequence: u64,
        }
        let Fields { volume, sequence } = Fields::deserialize(deserializer)?;
        Self::new(volume, sequence).map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for VersionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.encode())
    }
}

/// Identity of the logical block namespace and its garbage-collection
/// authority. A block ID is shareable across volumes only when this identity
/// is equal; a backend URL alone is not sufficient when prefixes or GC domains
/// differ.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BlockStoreId(pub String);

impl BlockStoreId {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.is_empty() || value.contains('\0') {
            return Err(crate::FsError::new(crate::ErrorCode::Einval)
                .with_message("block store id must be non-empty"));
        }
        Ok(Self(value))
    }
}

/// Qualified immutable block reference used by future delta/layer manifests.
/// The current namespace format stores `BlockId` in each extent, so the
/// manifest-level `block_store_id` on [`VersionInfo`] qualifies those IDs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockRef {
    pub store: BlockStoreId,
    pub block: BlockId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VersionKind {
    Initial,
    Snapshot,
    /// Caller-supplied namespace published through the versioned coordinator.
    NamespacePublication,
    Restore,
    Fork,
}

/// An immutable committed namespace manifest and its timeline metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionInfo {
    pub id: VersionId,
    /// Immediate predecessor in this volume's timeline. A restore points to
    /// the current head here, not to `restored_from`.
    pub parent: Option<VersionId>,
    /// Historical source whose contents were selected for a restore.
    pub restored_from: Option<VersionId>,
    /// Source version for a fork's first target version.
    pub forked_from: Option<VersionId>,
    pub kind: VersionKind,
    pub namespace: Namespace,
    pub block_store_id: BlockStoreId,
    pub created_at_ms: i64,
    pub durable: bool,
}

impl VersionInfo {
    pub fn validate(&self) -> Result<()> {
        if self.id.sequence == 0 {
            return Err(crate::FsError::new(crate::ErrorCode::Einval)
                .with_message("version sequence must be positive"));
        }
        if self
            .parent
            .as_ref()
            .is_some_and(|parent| parent.volume != self.id.volume)
        {
            return Err(crate::FsError::new(crate::ErrorCode::Einval)
                .with_message("version parent belongs to another volume"));
        }
        if self
            .parent
            .as_ref()
            .is_some_and(|parent| parent.sequence >= self.id.sequence)
        {
            return Err(crate::FsError::new(crate::ErrorCode::Einval)
                .with_message("version parent must precede its child"));
        }
        if self.restored_from.as_ref().is_some_and(|source| {
            source.volume == self.id.volume && source.sequence >= self.id.sequence
        }) {
            return Err(crate::FsError::new(crate::ErrorCode::Einval)
                .with_message("restore source must precede the restored version"));
        }
        self.namespace.validate()
    }

    /// Validate a record against the one logical volume owned by a metadata
    /// provider. Providers in this capability are intentionally single-volume;
    /// this check prevents a persisted row, parent, or restore source from
    /// crossing that boundary. `forked_from` is provenance and may point to a
    /// source volume outside this provider.
    pub fn validate_for_volume(&self, volume: &VolumeId) -> Result<()> {
        if &self.id.volume != volume {
            return Err(crate::FsError::new(crate::ErrorCode::Eio)
                .with_message("version belongs to another provider volume"));
        }
        self.validate()?;
        for reference in [self.parent.as_ref(), self.restored_from.as_ref()]
            .into_iter()
            .flatten()
        {
            if &reference.volume != volume {
                return Err(crate::FsError::new(crate::ErrorCode::Eio)
                    .with_message("version ancestry crosses provider volumes"));
            }
        }
        Ok(())
    }
}

/// Current version head plus the current-head revision CAS token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionHead {
    pub version: VersionId,
    pub revision: u64,
}

/// Stable retry identity supplied to a provider publication transaction.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PublicationId(pub String);

impl PublicationId {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.is_empty() || value.contains('\0') {
            return Err(crate::FsError::new(crate::ErrorCode::Einval)
                .with_message("publication id must be non-empty"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Input to the provider's one authoritative version/head transaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionPublication {
    pub expected_revision: u64,
    pub expected_parent: Option<VersionId>,
    pub operation_id: PublicationId,
    pub namespace: Namespace,
    pub block_store_id: BlockStoreId,
    pub kind: VersionKind,
    pub restored_from: Option<VersionId>,
    pub forked_from: Option<VersionId>,
    pub durable: bool,
}

impl VersionPublication {
    pub fn validate(&self, volume: &VolumeId) -> Result<()> {
        if self.operation_id.0.is_empty() || self.operation_id.0.contains('\0') {
            return Err(crate::FsError::new(crate::ErrorCode::Einval)
                .with_message("publication id must be non-empty"));
        }
        if self.block_store_id.0.is_empty() || self.block_store_id.0.contains('\0') {
            return Err(crate::FsError::new(crate::ErrorCode::Einval)
                .with_message("publication block store id must be non-empty"));
        }
        self.namespace.validate()?;
        if self
            .expected_parent
            .as_ref()
            .is_some_and(|parent| &parent.volume != volume)
        {
            return Err(crate::FsError::new(crate::ErrorCode::Einval)
                .with_message("expected version parent belongs to another volume"));
        }
        if self
            .restored_from
            .as_ref()
            .is_some_and(|source| &source.volume != volume)
        {
            return Err(crate::FsError::new(crate::ErrorCode::Einval)
                .with_message("restore source belongs to another volume"));
        }
        Ok(())
    }

    /// Compare a retry with the committed publication identified by the same
    /// operation ID. The expected revision is deliberately not compared:
    /// a response can be lost and the volume may advance before the caller
    /// retries reconciliation.
    pub fn matches_committed(&self, committed: &VersionInfo) -> Result<bool> {
        let left = serde_json::to_vec(&self.namespace).map_err(crate::error::backend_error)?;
        let right =
            serde_json::to_vec(&committed.namespace).map_err(crate::error::backend_error)?;
        Ok(self.expected_parent == committed.parent
            && self.restored_from == committed.restored_from
            && self.forked_from == committed.forked_from
            && self.block_store_id == committed.block_store_id
            && self.kind == committed.kind
            && self.durable == committed.durable
            && left == right)
    }
}

/// Provider-owned expiring read/retention lease for an immutable view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadLease {
    pub volume: VolumeId,
    pub version: VersionId,
    pub view_id: String,
    pub owner: String,
    pub fence: u64,
    pub expires_at_ms: u64,
}

/// Request to open or renew a read lease. The provider supplies the clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadLeaseRequest {
    pub owner: String,
    pub ttl: Duration,
}

impl ReadLeaseRequest {
    pub fn validate(&self) -> Result<u64> {
        if self.owner.is_empty() {
            return Err(crate::FsError::new(crate::ErrorCode::Einval)
                .with_message("read lease owner must not be empty"));
        }
        let ttl_ms = u64::try_from(self.ttl.as_millis()).map_err(|_| {
            crate::FsError::new(crate::ErrorCode::Eoverflow).with_message("read lease TTL overflow")
        })?;
        if ttl_ms == 0 {
            return Err(crate::FsError::new(crate::ErrorCode::Einval)
                .with_message("read lease TTL must be positive"));
        }
        Ok(ttl_ms)
    }
}

#[cfg(kani)]
mod read_lease_verification {
    use super::ReadLeaseRequest;
    use crate::ErrorCode;
    use std::time::Duration;

    #[kani::proof]
    #[kani::unwind(8)]
    fn read_lease_ttl_milliseconds_are_checked() {
        let seconds: u64 = kani::any();
        let nanos: u32 = kani::any();
        kani::assume(nanos < 1_000_000_000);

        let request = ReadLeaseRequest {
            owner: "owner".to_owned(),
            ttl: Duration::new(seconds, nanos),
        };
        let exact_ms = u128::from(seconds) * 1_000 + u128::from(nanos / 1_000_000);
        let result = request.validate();
        let admitted = exact_ms > 0 && exact_ms <= u128::from(u64::MAX);
        assert_eq!(result.is_ok(), admitted);
        match result {
            Ok(ttl_ms) => assert_eq!(u128::from(ttl_ms), exact_ms),
            Err(error) if exact_ms == 0 => {
                assert!(error.is(ErrorCode::Einval));
            }
            Err(error) => {
                assert!(exact_ms > u128::from(u64::MAX));
                assert!(error.is(ErrorCode::Eoverflow));
            }
        }

        kani::cover!(seconds == 0 && nanos == 999_999 && !admitted);
        kani::cover!(seconds == 0 && nanos == 1_000_000 && admitted);
        kani::cover!(exact_ms == u128::from(u64::MAX) && admitted);
        kani::cover!(seconds == u64::MAX && !admitted);
    }
}

/// Additional metadata capability. Existing providers can continue
/// implementing only [`MetadataStore`]; versioned coordinators require this
/// trait and therefore fail explicitly at integration selection time rather
/// than silently degrading to an in-memory copy.
#[async_trait]
pub trait VersionedMetadataStore: MetadataStore {
    /// Every provider instance owns exactly one stable logical volume. Version
    /// IDs, parent/restore ancestry, and read leases must use this identity;
    /// implementations must reject cross-volume persisted state. A
    /// `forked_from` field may retain provenance for a source in another
    /// provider volume.
    fn volume_id(&self) -> VolumeId;

    /// Return the public current head. `None` is the valid empty history state.
    async fn version_head(&self) -> Result<Option<VersionHead>>;

    async fn load_version(&self, id: &VersionId) -> Result<VersionInfo>;

    async fn list_versions(&self) -> Result<Vec<VersionInfo>>;

    /// Reconcile an operation after a lost/ambiguous response. This lookup
    /// does not require a current writer lease because it only reads an
    /// immutable committed record; callers must still compare the original
    /// publication payload before accepting the result.
    async fn find_publication(&self, operation_id: &PublicationId) -> Result<Option<VersionInfo>>;

    /// Reconcile a lost/ambiguous publication response and verify that the
    /// committed record is the same logical publication. The expected
    /// revision is intentionally not part of the comparison: the head may
    /// have advanced after the original commit.
    async fn reconcile_publication(
        &self,
        publication: &VersionPublication,
    ) -> Result<Option<VersionInfo>> {
        publication.validate(&self.volume_id())?;
        let Some(committed) = self.find_publication(&publication.operation_id).await? else {
            return Ok(None);
        };
        if !publication.matches_committed(&committed)? {
            return Err(crate::FsError::new(crate::ErrorCode::Eexist)
                .with_syscall("reconcile publication")
                .with_message("publication id was reused with a different payload"));
        }
        Ok(Some(committed))
    }

    /// Atomically validate the writer lease, current metadata revision, and
    /// expected same-volume parent, then activate the version and current
    /// namespace together. Implementations must only return success after the
    /// active transaction is durable when `publication.durable` is true.
    /// Pending internal records are allowed but must not appear in the public
    /// head/history until activation.
    async fn publish_version(
        &self,
        lease: &WriterLease,
        publication: VersionPublication,
    ) -> Result<VersionInfo>;

    async fn open_view_pin(&self, id: &VersionId, request: ReadLeaseRequest) -> Result<ReadLease>;

    /// Renew only when the persisted pin matches every supplied token field,
    /// including the lease owner, and the request owner matches that same
    /// persisted owner. Checking only the request owner would allow a forged
    /// token to be renewed under the real owner's name.
    async fn renew_view_pin(
        &self,
        lease: &ReadLease,
        request: ReadLeaseRequest,
    ) -> Result<ReadLease>;

    /// Close is idempotent for a missing view ID. A present view with a stale
    /// lease token returns `ESTALE` and remains protected. Matching deletion
    /// must be conditional and atomic with respect to renewal/deletion.
    async fn close_view_pin(&self, lease: &ReadLease) -> Result<()>;

    /// Delete a non-head version when no unexpired view lease protects it.
    /// Block reclamation remains a coordinator operation over all volumes
    /// sharing a qualified block-store identity.
    async fn delete_version(&self, lease: &WriterLease, id: &VersionId) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunking::ChunkerConfig;
    use crate::storage::{NAMESPACE_FORMAT_VERSION, NodeData, NodeMetadata};
    use crate::types::{S_IFDIR, Stats};
    use std::collections::BTreeMap;

    fn root_namespace() -> Namespace {
        Namespace {
            format_version: NAMESPACE_FORMAT_VERSION,
            root: 1,
            next_inode: 2,
            default_uid: 0,
            default_gid: 0,
            umask: 0o022,
            default_chunker: ChunkerConfig {
                algorithm: "fixed-size".to_owned(),
                version: 1,
                parameters: BTreeMap::from([("chunk_size".to_owned(), 4096)]),
            },
            nodes: BTreeMap::from([(
                1,
                NodeMetadata {
                    stats: Stats {
                        dev: 0,
                        ino: 1,
                        mode: S_IFDIR | 0o755,
                        nlink: 2,
                        uid: 0,
                        gid: 0,
                        rdev: 0,
                        size: 0,
                        blksize: 4096,
                        blocks: 0,
                        atime_ms: 0,
                        mtime_ms: 0,
                        ctime_ms: 0,
                        birthtime_ms: 0,
                    },
                    data: NodeData::Directory { entries: vec![] },
                },
            )]),
        }
    }

    fn version(sequence: u64) -> VersionInfo {
        VersionInfo {
            id: VersionId::new(VolumeId::new("team").unwrap(), sequence).unwrap(),
            parent: None,
            restored_from: None,
            forked_from: None,
            kind: VersionKind::Snapshot,
            namespace: root_namespace(),
            block_store_id: BlockStoreId::new("blocks").unwrap(),
            created_at_ms: 0,
            durable: true,
        }
    }

    #[test]
    fn deserialization_rejects_invalid_version_identity() {
        assert!(serde_json::from_str::<VolumeId>(r#""team:prod""#).is_err());
        assert!(serde_json::from_str::<VolumeId>(r#""""#).is_err());
        assert!(serde_json::from_str::<VersionId>(r#"{"volume":"team","sequence":0}"#).is_err());
        assert!(
            serde_json::from_str::<VersionId>(r#"{"volume":"team:prod","sequence":1}"#).is_err()
        );
    }

    #[test]
    fn decode_rejects_noncanonical_sequence_text() {
        for text in ["team:01", "team:+1"] {
            assert_eq!(
                VersionId::decode(text).unwrap_err().code,
                crate::ErrorCode::Einval
            );
        }
    }

    #[test]
    fn read_lease_ttl_checks_submillisecond_and_u64_boundaries() {
        let request = |ttl| ReadLeaseRequest {
            owner: "owner".to_owned(),
            ttl,
        };

        assert_eq!(
            request(Duration::new(0, 999_999))
                .validate()
                .unwrap_err()
                .code,
            crate::ErrorCode::Einval
        );
        assert_eq!(request(Duration::new(0, 1_000_000)).validate().unwrap(), 1);
        assert_eq!(
            request(Duration::new(u64::MAX / 1_000, 615_000_000))
                .validate()
                .unwrap(),
            u64::MAX
        );
        assert_eq!(
            request(Duration::new(u64::MAX, 0))
                .validate()
                .unwrap_err()
                .code,
            crate::ErrorCode::Eoverflow
        );
    }

    #[test]
    fn committed_lineage_cannot_point_to_same_or_future_version() {
        let mut record = version(3);
        assert!(record.validate().is_ok());
        for sequence in [3, 4] {
            record.parent = Some(VersionId::new(record.id.volume.clone(), sequence).unwrap());
            assert_eq!(
                record.validate().unwrap_err().code,
                crate::ErrorCode::Einval
            );
        }
        record.parent = Some(VersionId::new(record.id.volume.clone(), 2).unwrap());
        assert!(record.validate().is_ok());
        record.restored_from = Some(VersionId::new(record.id.volume.clone(), 4).unwrap());
        assert_eq!(
            record.validate().unwrap_err().code,
            crate::ErrorCode::Einval
        );
    }
}
