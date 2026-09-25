//! Explicit MRC5 transactions. Every input uses ordinary reads at one read version.
use super::*;
use mount_rs_core::diagnostics::profile::{self, Event};
use mount_rs_core::storage::compact::*;

const GUARDS: &[u8] = b"meta/compact-guard/";
const HEADER: usize = 28;

fn guard_key(keys: &Keyspace, inode: u64) -> Vec<u8> {
    let mut key = keys.key(GUARDS);
    key.extend(inode.to_be_bytes());
    key
}
fn decode_guard(raw: &[u8]) -> Result<CompactGuard> {
    if raw.len() <= HEADER || raw.len() > FOUNDATIONDB_MAX_VALUE_BYTES || &raw[..4] != b"MRG5" {
        return Err(backend_error("invalid MRG5 guard encoding"));
    }
    let node: NodeMetadata = serde_json::from_slice(&raw[HEADER..]).map_err(backend_error)?;
    // Persist one canonical shape. In particular reject unknown/duplicate JSON
    // fields, trailing JSON, or noncanonical bodies silently accepted by serde.
    let canonical = serde_json::to_vec(&node).map_err(backend_error)?;
    // Count validation serialization as well as mutation serialization so the
    // diagnostics expose all body encoding work on selected/structural paths.
    profile::add(Event::InodeSerialized, canonical.len() as u64);
    if canonical != raw[HEADER..] {
        return Err(backend_error("noncanonical MRG5 node body"));
    }
    profile::add(Event::InodeReturned, (raw.len() - HEADER) as u64);
    Ok(CompactGuard {
        identity: PhysicalInodeIdentity {
            incarnation: u64::from_be_bytes(raw[4..12].try_into().unwrap()),
            epoch: u64::from_be_bytes(raw[12..20].try_into().unwrap()),
            revision: u64::from_be_bytes(raw[20..28].try_into().unwrap()),
        },
        node,
    })
}
fn encode_guard(anchor: &CompactAnchor, inode: u64, guard: &CompactGuard) -> Result<Vec<u8>> {
    LoadedCompactInode::from_guard(anchor, inode, guard.clone())?;
    let mut bytes = b"MRG5".to_vec();
    bytes.extend(guard.identity.incarnation.to_be_bytes());
    bytes.extend(guard.identity.epoch.to_be_bytes());
    bytes.extend(guard.identity.revision.to_be_bytes());
    let body = serde_json::to_vec(&guard.node).map_err(backend_error)?;
    profile::add(Event::InodeSerialized, body.len() as u64);
    bytes.extend(body);
    Ok(bytes)
}

/// Conservative accounting includes returned bytes (FDB need not charge them),
/// key and range conflict endpoints, mutations and a per-operation allowance.
/// No set/clear is issued until all reads, encodings and this plan are checked.
#[derive(Default)]
struct Budget(usize);
impl Budget {
    fn add(&mut self, bytes: usize) -> TxnResult<()> {
        add_affected_bytes(&mut self.0, bytes).map_err(TxnError::Fs)?;
        if self.0 > FOUNDATIONDB_MAX_TRANSACTION_BYTES {
            return Err(TxnError::Fs(metadata_transaction_too_large(self.0)));
        }
        Ok(())
    }
    fn key(key: &[u8]) -> TxnResult<()> {
        if key.len() > FOUNDATIONDB_MAX_KEY_BYTES {
            return Err(TxnError::Fs(FsError::new(ErrorCode::Efbig)));
        }
        Ok(())
    }
    fn point(&mut self, key: &[u8], bytes: usize) -> TxnResult<()> {
        Self::key(key)?;
        let cost = key
            .len()
            .checked_mul(4)
            .and_then(|v| v.checked_add(bytes))
            .and_then(|v| v.checked_add(64))
            .ok_or_else(|| TxnError::Fs(FsError::new(ErrorCode::Eoverflow)))?;
        self.add(cost)
    }
    fn range(&mut self, start: &[u8], end: &[u8]) -> TxnResult<()> {
        Self::key(start)?;
        Self::key(end)?;
        self.add(4 * (start.len() + end.len()) + 64)
    }
}

struct View<'a> {
    trx: &'a Transaction,
    inner: &'a Inner,
    keys: Keyspace,
    budget: Budget,
}
impl<'a> View<'a> {
    fn new(trx: &'a Transaction, inner: &'a Inner) -> TxnResult<Self> {
        configure_transaction(trx, inner.limits)?;
        Ok(Self {
            trx,
            inner,
            keys: Keyspace::new(&inner.prefix),
            budget: Budget::default(),
        })
    }
    async fn get(&mut self, key: Vec<u8>) -> TxnResult<Option<Vec<u8>>> {
        Budget::key(&key)?;
        let value = get_owned(self.trx, &key).await?;
        self.budget
            .point(&key, value.as_ref().map_or(0, Vec::len))?;
        #[cfg(test)]
        self.inner.compact_trace.lock().unwrap().push((
            "get",
            key,
            value.as_ref().map_or(0, Vec::len),
        ));
        Ok(value)
    }
    async fn range(&mut self, prefix: Vec<u8>) -> TxnResult<Vec<(Vec<u8>, Vec<u8>)>> {
        let end = range_end(&prefix).map_err(TxnError::Fs)?;
        self.budget.range(&prefix, &end)?;
        #[cfg(test)]
        self.inner
            .compact_trace
            .lock()
            .unwrap()
            .push(("range", prefix.clone(), 0));
        let mut stream = self
            .trx
            .get_ranges_keyvalues((prefix.as_slice(), end.as_slice()).into(), false);
        let mut rows = Vec::new();
        while let Some(kv) = stream.try_next().await? {
            self.budget.point(kv.key(), kv.value().len())?;
            rows.push((kv.key().to_vec(), kv.value().to_vec()));
        }
        Ok(rows)
    }
    async fn authority(
        &mut self,
        backing: ConcurrentBackingId,
        prepare: bool,
    ) -> TxnResult<(Manifest, Vec<u8>)> {
        if self.inner.block_authority_policy != FoundationDbBlockAuthorityPolicy::SameKeyspace {
            return Err(TxnError::Fs(stale_backing()));
        }
        let mode = self.get(self.keys.write_mode()).await?;
        let bound = self.get(self.keys.metadata_backing()).await?;
        let authority = self.get(self.keys.block_authority()).await?;
        let policy = self.get(self.keys.block_authority_policy()).await?;
        let lease = self.get(self.keys.lease()).await?;
        let fence = self.get(self.keys.fence()).await?;
        let delegation = self.get(self.keys.key(b"meta/delegation")).await?;
        if mode.as_deref() != Some(if prepare { b"MRC2" } else { b"MRC5" })
            || bound.as_deref().and_then(parse_backing_bytes) != Some(backing)
            || authority.as_deref().and_then(parse_backing_bytes) != Some(backing)
            || policy.as_deref() != Some(FoundationDbBlockAuthorityPolicy::SameKeyspace.encode())
            || lease.is_some()
            || delegation.is_some()
            || fence.as_deref() != Some(CONCURRENT_FENCE_SENTINEL)
        {
            return Err(TxnError::Fs(stale_backing()));
        }
        let raw = self
            .get(self.keys.manifest())
            .await?
            .ok_or_else(|| TxnError::Fs(stale_backing()))?;
        let manifest = decode_manifest(&raw).map_err(TxnError::Fs)?;
        let length = usize::try_from(manifest.payload_len)
            .map_err(|_| TxnError::Fs(FsError::new(ErrorCode::Eoverflow)))?;
        if length > self.inner.limits.max_metadata_bytes
            || metadata_chunk_count(length, self.inner.limits.metadata_chunk_bytes)
                .map_err(TxnError::Fs)?
                != manifest.chunk_count as usize
        {
            return Err(TxnError::Fs(backend_error(
                "invalid compact manifest bounds",
            )));
        }
        let prefix = self.keys.chunks();
        let rows = self.range(prefix.clone()).await?;
        if rows.len() != manifest.chunk_count as usize {
            return Err(TxnError::Fs(backend_error(
                "compact chunk membership mismatch",
            )));
        }
        let mut payload = Vec::with_capacity(length);
        for (i, (key, value)) in rows.into_iter().enumerate() {
            let expected = (length - payload.len()).min(self.inner.limits.metadata_chunk_bytes);
            if key != metadata_chunk_key(&prefix, i as u32) || value.len() != expected {
                return Err(TxnError::Fs(backend_error(
                    "invalid compact chunk key/length",
                )));
            }
            payload.extend(value);
        }
        Ok((manifest, payload))
    }
    async fn anchor(
        &mut self,
        backing: ConcurrentBackingId,
    ) -> TxnResult<(Manifest, CompactAnchor)> {
        let (manifest, bytes) = self.authority(backing, false).await?;
        let anchor = decode_compact_anchor(&bytes).map_err(TxnError::Fs)?;
        profile::add(Event::CompactAnchorReturned, bytes.len() as u64);
        if anchor.generation != manifest.revision || anchor.backing != backing {
            return Err(TxnError::Fs(backend_error(
                "compact anchor authority mismatch",
            )));
        }
        #[cfg(test)]
        super::compact_tests::pause(self.inner, "anchor").await?;
        Ok((manifest, anchor))
    }
    async fn guard(&mut self, inode: u64) -> TxnResult<CompactGuard> {
        let raw = self
            .get(guard_key(&self.keys, inode))
            .await?
            .ok_or_else(|| TxnError::Fs(stale_backing()))?;
        if raw.len() > self.inner.limits.max_metadata_bytes {
            return Err(TxnError::Fs(FsError::new(ErrorCode::Efbig)));
        }
        decode_guard(&raw).map_err(TxnError::Fs)
    }
    async fn guards(&mut self) -> TxnResult<BTreeMap<u64, CompactGuard>> {
        let prefix = self.keys.key(GUARDS);
        let mut guards = BTreeMap::new();
        for (key, raw) in self.range(prefix.clone()).await? {
            if key.len() != prefix.len() + 8 || raw.len() > self.inner.limits.max_metadata_bytes {
                return Err(TxnError::Fs(backend_error(
                    "invalid compact guard key/value bounds",
                )));
            }
            let inode = u64::from_be_bytes(key[prefix.len()..].try_into().unwrap());
            guards.insert(inode, decode_guard(&raw).map_err(TxnError::Fs)?);
        }
        Ok(guards)
    }
}

enum Mutation {
    Set(Vec<u8>, Vec<u8>),
    Clear(Vec<u8>),
    ClearRange(Vec<u8>, Vec<u8>),
}
#[derive(Default)]
struct Plan(Vec<Mutation>);
impl Plan {
    fn set(&mut self, view: &mut View<'_>, key: Vec<u8>, value: Vec<u8>) -> TxnResult<()> {
        if value.len() > FOUNDATIONDB_MAX_VALUE_BYTES
            || value.len() > view.inner.limits.max_metadata_bytes
        {
            return Err(TxnError::Fs(FsError::new(ErrorCode::Efbig)));
        }
        view.budget.point(&key, value.len())?;
        self.0.push(Mutation::Set(key, value));
        Ok(())
    }
    fn clear(&mut self, view: &mut View<'_>, key: Vec<u8>) -> TxnResult<()> {
        view.budget.point(&key, 0)?;
        self.0.push(Mutation::Clear(key));
        Ok(())
    }
    fn anchor(
        &mut self,
        view: &mut View<'_>,
        old: Manifest,
        anchor: &CompactAnchor,
    ) -> TxnResult<()> {
        let bytes = encode_compact_anchor(anchor).map_err(TxnError::Fs)?;
        profile::add(Event::CompactAnchorSerialized, bytes.len() as u64);
        if bytes.len() > view.inner.limits.max_metadata_bytes {
            return Err(TxnError::Fs(FsError::new(ErrorCode::Efbig)));
        }
        let chunk_size = view.inner.limits.metadata_chunk_bytes;
        let count = metadata_chunk_count(bytes.len(), chunk_size).map_err(TxnError::Fs)? as u32;
        let prefix = view.keys.chunks();
        if let Some(start) = metadata_chunk_clear_start(&prefix, Some(old), count) {
            let end = range_end(&prefix).map_err(TxnError::Fs)?;
            view.budget.range(&start, &end)?;
            self.0.push(Mutation::ClearRange(start, end));
        }
        for (i, chunk) in bytes.chunks(chunk_size).enumerate() {
            self.set(view, metadata_chunk_key(&prefix, i as u32), chunk.to_vec())?;
        }
        self.set(
            view,
            view.keys.manifest(),
            encode_manifest(Manifest {
                revision: anchor.generation,
                chunk_count: count,
                payload_len: bytes.len() as u64,
            }),
        )
    }
    fn guard(
        &mut self,
        view: &mut View<'_>,
        anchor: &CompactAnchor,
        inode: u64,
        guard: &CompactGuard,
    ) -> TxnResult<()> {
        let raw = encode_guard(anchor, inode, guard).map_err(TxnError::Fs)?;
        self.set(view, guard_key(&view.keys, inode), raw)
    }
    async fn apply(self, view: &View<'_>) -> TxnResult<()> {
        #[cfg(test)]
        super::compact_tests::pause(view.inner, "before_apply").await?;
        for mutation in self.0 {
            match mutation {
                Mutation::Set(key, value) => {
                    #[cfg(test)]
                    view.inner.compact_trace.lock().unwrap().push((
                        "set",
                        key.clone(),
                        value.len(),
                    ));
                    view.trx.set(&key, &value);
                }
                Mutation::Clear(key) => {
                    #[cfg(test)]
                    view.inner
                        .compact_trace
                        .lock()
                        .unwrap()
                        .push(("clear", key.clone(), 0));
                    view.trx.clear(&key);
                }
                Mutation::ClearRange(start, end) => {
                    #[cfg(test)]
                    view.inner.compact_trace.lock().unwrap().push((
                        "clear_range",
                        start.clone(),
                        end.len(),
                    ));
                    view.trx.clear_range(&start, &end);
                }
            }
        }
        #[cfg(test)]
        super::compact_tests::pause(view.inner, "after_apply").await?;
        Ok(())
    }
}

#[derive(Clone)]
pub(super) enum Command {
    Prepare(ConcurrentBackingId, u64),
    Snapshot(ConcurrentBackingId),
    Load(ConcurrentBackingId, u64),
    Publish(
        ConcurrentBackingId,
        u64,
        u64,
        PhysicalInodeIdentity,
        NodeMetadata,
    ),
    Structure(CompactStructuralDelta),
}
pub(super) enum Output {
    Prepared,
    Snapshot(CompactSnapshot),
    Inode(LoadedCompactInode),
    Structure(CompactPublication),
}
impl FoundationDbMetadataStore {
    pub(super) async fn compact_transaction(&self, command: Command) -> Result<Output> {
        let inner = Arc::clone(&self.0);
        let context = Arc::clone(&inner);
        // Even reads use this policy: no command in this bridge can accidentally
        // acquire the idempotent mutation retry behavior on a future extension.
        inner
            .transact_metadata((), move |trx, _| {
                let inner = Arc::clone(&context);
                let command = command.clone();
                Box::pin(async move { execute(trx, &inner, command).await })
            })
            .await
    }
}

async fn execute(trx: &Transaction, inner: &Inner, command: Command) -> TxnResult<Output> {
    let mut view = View::new(trx, inner)?;
    match command {
        Command::Prepare(backing, expected) => {
            let generation = expected
                .checked_add(1)
                .ok_or_else(|| TxnError::Fs(FsError::new(ErrorCode::Eoverflow)))?;
            let (manifest, bytes) = view.authority(backing, true).await?;
            if expected == 0 || manifest.revision != expected {
                return Err(TxnError::Fs(FsError::new(ErrorCode::Eagain)));
            }
            let ns: Namespace =
                serde_json::from_slice(&bytes).map_err(|e| TxnError::Fs(backend_error(e)))?;
            ns.validate().map_err(TxnError::Fs)?;
            let root = ns
                .nodes
                .get(&ns.root)
                .ok_or_else(|| TxnError::Fs(stale_backing()))?;
            if ns.nodes.len() != 1
                || ns.next_inode
                    != ns
                        .root
                        .checked_add(1)
                        .ok_or_else(|| TxnError::Fs(FsError::new(ErrorCode::Eoverflow)))?
                || !matches!(&root.data,NodeData::Directory{entries} if entries.is_empty())
            {
                return Err(TxnError::Fs(FsError::new(ErrorCode::Ebusy)));
            }
            for prefix in [GUARDS, b"meta/inode/", b"meta/inode-version/"] {
                if !view.range(view.keys.key(prefix)).await?.is_empty() {
                    return Err(TxnError::Fs(FsError::new(ErrorCode::Ebusy)));
                }
            }
            let anchor = CompactAnchor {
                backing,
                generation,
                root: ns.root,
                next_inode: ns.next_inode,
                default_uid: ns.default_uid,
                default_gid: ns.default_gid,
                umask: ns.umask,
                default_chunker: ns.default_chunker,
                members: vec![ns.root],
            };
            let guard = CompactGuard {
                identity: PhysicalInodeIdentity {
                    incarnation: generation,
                    epoch: generation,
                    revision: 0,
                },
                node: root.clone(),
            };
            let mut plan = Plan::default();
            plan.guard(&mut view, &anchor, ns.root, &guard)?;
            plan.anchor(&mut view, manifest, &anchor)?;
            let key = view.keys.write_mode();
            plan.set(&mut view, key, b"MRC5".to_vec())?;
            plan.apply(&view).await?;
            Ok(Output::Prepared)
        }
        Command::Snapshot(backing) => {
            let (_, anchor) = view.anchor(backing).await?;
            let snapshot = CompactSnapshot {
                anchor,
                guards: view.guards().await?,
            };
            snapshot.namespace().map_err(TxnError::Fs)?;
            Ok(Output::Snapshot(snapshot))
        }
        Command::Load(backing, inode) => {
            let (_, anchor) = view.anchor(backing).await?;
            let guard = view.guard(inode).await?;
            Ok(Output::Inode(
                LoadedCompactInode::from_guard(&anchor, inode, guard).map_err(TxnError::Fs)?,
            ))
        }
        Command::Publish(backing, inode, generation, expected, node) => {
            let (_, anchor) = view.anchor(backing).await?;
            if anchor.generation != generation {
                return Err(TxnError::Fs(FsError::new(ErrorCode::Eagain)));
            }
            let current = view.guard(inode).await?;
            let guard = validate_selected_update(
                &anchor, backing, generation, inode, &current, expected, node,
            )
            .map_err(TxnError::Fs)?;
            let mut plan = Plan::default();
            plan.guard(&mut view, &anchor, inode, &guard)?;
            plan.apply(&view).await?;
            Ok(Output::Inode(LoadedCompactInode { generation, guard }))
        }
        Command::Structure(delta) => {
            let (manifest, anchor) = view.anchor(delta.base_anchor().backing).await?;
            let current = match delta.scope() {
                StructuralScope::Full => view.guards().await?,
                StructuralScope::FileCreate => {
                    let mut map = BTreeMap::new();
                    for &inode in delta.expected().keys() {
                        map.insert(inode, view.guard(inode).await?);
                    }
                    map
                }
            };
            let publication = delta
                .validate_current(&anchor, &current)
                .map_err(TxnError::Fs)?;
            for &inode in delta.created().keys() {
                if view.get(guard_key(&view.keys, inode)).await?.is_some() {
                    return Err(TxnError::Fs(backend_error(
                        "new compact guard already exists",
                    )));
                }
            }
            let mut plan = Plan::default();
            for &inode in &publication.removed {
                let key = guard_key(&view.keys, inode);
                plan.clear(&mut view, key)?;
            }
            for (&inode, guard) in &publication.upserts {
                plan.guard(&mut view, &publication.anchor, inode, guard)?;
            }
            plan.anchor(&mut view, manifest, &publication.anchor)?;
            plan.apply(&view).await?;
            Ok(Output::Structure(publication))
        }
    }
}
