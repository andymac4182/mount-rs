//! Opt-in MRC5 layout. Private byte caps are implementation guardrails, not
//! PostgreSQL limits. MRC4 data and transactions retain their existing contract.
use super::*;
use futures_util::StreamExt;
use mount_rs_core::storage::{NodeData, compact::*};
use tokio_postgres::{GenericClient, IsolationLevel, Transaction};

const TABLE: &str = "mount_rs_compact_guards";
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS mount_rs_compact_guards (
 volume_key TEXT NOT NULL, inode BIGINT NOT NULL,
 incarnation BIGINT NOT NULL, epoch BIGINT NOT NULL,
 revision BIGINT NOT NULL, node TEXT NOT NULL,
 PRIMARY KEY(volume_key,inode))";
const MAX_KEY: usize = 1020;
const MAX_BODY: usize = 16 * 1024 * 1024;
const MAX_TOTAL: usize = 64 * 1024 * 1024;

#[derive(Default)]
struct Budget(usize);
impl Budget {
    fn add(&mut self, bytes: usize) -> Result<()> {
        self.0 = self
            .0
            .checked_add(bytes)
            .filter(|&n| n <= MAX_TOTAL)
            .ok_or_else(|| FsError::new(ErrorCode::Efbig))?;
        Ok(())
    }
}
fn key(volume: &str) -> Result<()> {
    if volume.is_empty() || volume.len() > MAX_KEY {
        return Err(FsError::new(ErrorCode::Efbig));
    }
    Ok(())
}
fn wire_bytes(volume: &str, body: usize) -> Result<usize> {
    key(volume)?;
    // Each compact statement has <=7 parameters; 4096 covers SQL text, all
    // Parse/Bind/Describe/Execute/Sync framing, integer values and lengths.
    let bytes = body
        .checked_add(volume.len())
        .and_then(|n| n.checked_add(4096));
    if body > MAX_BODY || bytes.is_none_or(|n| n > i32::MAX as usize) {
        return Err(FsError::new(ErrorCode::Efbig));
    }
    Ok(bytes.expect("checked wire size"))
}

pub(super) async fn initialize(client: &Client) -> Result<()> {
    client.batch_execute(SCHEMA).await.map_err(postgres_error)?;
    validate_schema(client).await
}
async fn validate_schema<C: GenericClient>(client: &C) -> Result<()> {
    let rows=client.query_typed("SELECT a.attname,a.atttypid::bigint,a.attnotnull,a.attgenerated::text,a.attidentity::text,a.atthasdef,a.attnum::int,c.relkind::text,c.relpersistence::text FROM pg_catalog.pg_attribute a JOIN pg_catalog.pg_class c ON c.oid=a.attrelid WHERE a.attrelid=to_regclass($1) AND a.attnum>0 ORDER BY a.attnum", &[(&TABLE, Type::TEXT)]).await.map_err(postgres_error)?;
    let expected = [
        ("volume_key", 25_i64),
        ("inode", 20),
        ("incarnation", 20),
        ("epoch", 20),
        ("revision", 20),
        ("node", 25),
    ];
    if rows.len() != expected.len() {
        return Err(backend_error("incompatible compact PostgreSQL columns"));
    }
    for (i, (r, (name, oid))) in rows.iter().zip(expected).enumerate() {
        if r.try_get::<_, String>(0).map_err(postgres_error)? != name
            || r.try_get::<_, i64>(1).map_err(postgres_error)? != oid
            || !r.try_get::<_, bool>(2).map_err(postgres_error)?
            || !r
                .try_get::<_, String>(3)
                .map_err(postgres_error)?
                .is_empty()
            || !r
                .try_get::<_, String>(4)
                .map_err(postgres_error)?
                .is_empty()
            || r.try_get::<_, bool>(5).map_err(postgres_error)?
            || r.try_get::<_, i32>(6).map_err(postgres_error)? != i as i32 + 1
            || r.try_get::<_, String>(7).map_err(postgres_error)? != "r"
            || r.try_get::<_, String>(8).map_err(postgres_error)? != "p"
        {
            return Err(backend_error("incompatible compact PostgreSQL columns"));
        }
    }
    let rows=client.query_typed("SELECT indisprimary,indisunique,indisvalid,indisready,indnkeyatts::int,indnatts::int,indkey::text,indexprs IS NULL,indpred IS NULL FROM pg_catalog.pg_index WHERE indrelid=to_regclass($1)",&[(&TABLE, Type::TEXT)]).await.map_err(postgres_error)?;
    if rows.len() != 1 {
        return Err(backend_error(
            "compact PostgreSQL requires sole primary key",
        ));
    }
    let r = &rows[0];
    if !r.try_get::<_, bool>(0).map_err(postgres_error)?
        || !r.try_get::<_, bool>(1).map_err(postgres_error)?
        || !r.try_get::<_, bool>(2).map_err(postgres_error)?
        || !r.try_get::<_, bool>(3).map_err(postgres_error)?
        || r.try_get::<_, i32>(4).map_err(postgres_error)? != 2
        || r.try_get::<_, i32>(5).map_err(postgres_error)? != 2
        || r.try_get::<_, String>(6).map_err(postgres_error)? != "1 2"
        || !r.try_get::<_, bool>(7).map_err(postgres_error)?
        || !r.try_get::<_, bool>(8).map_err(postgres_error)?
    {
        return Err(backend_error("incompatible compact PostgreSQL primary key"));
    }
    Ok(())
}

fn anchor_bounds(a: &CompactAnchor) -> Result<()> {
    inode_signed(a.generation)?;
    inode_signed(a.root)?;
    inode_signed(a.next_inode)?;
    // Check the minimum encoded membership size before serialization. Decimal
    // JSON membership costs at least two bytes/member. No arbitrary inode cap.
    if a.members.len().checked_mul(2).is_none_or(|n| n > MAX_BODY) {
        return Err(FsError::new(ErrorCode::Efbig));
    }
    for &id in &a.members {
        inode_signed(id)?;
    }
    Ok(())
}
fn identity_bounds(id: PhysicalInodeIdentity) -> Result<()> {
    inode_signed(id.incarnation)?;
    inode_signed(id.epoch)?;
    inode_signed(id.revision)?;
    Ok(())
}
fn encode_anchor(a: &CompactAnchor) -> Result<String> {
    anchor_bounds(a)?;
    a.validate()?;
    let mut out =
        LimitedWriter(br#"{"layout":"mount-rs-compact-inodes","version":1,"anchor":"#.to_vec());
    let result = serde_json::to_writer(&mut out, a)
        .and_then(|()| std::io::Write::write_all(&mut out, b"}").map_err(serde_json::Error::io));
    profile::add(Event::CompactAnchorSerialized, out.0.len() as u64);
    result.map_err(|e| {
        if e.is_io() {
            FsError::new(ErrorCode::Efbig)
        } else {
            backend_error(e)
        }
    })?;
    String::from_utf8(out.0).map_err(backend_error)
}
struct LimitedWriter(Vec<u8>);
impl std::io::Write for LimitedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self
            .0
            .len()
            .checked_add(bytes.len())
            .is_none_or(|n| n > MAX_BODY)
        {
            return Err(std::io::Error::other("compact node byte cap"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn encode_node(node: &NodeMetadata) -> Result<String> {
    let mut out = LimitedWriter(Vec::new());
    let result = serde_json::to_writer(&mut out, node);
    profile::add(Event::InodeSerialized, out.0.len() as u64);
    if let Err(error) = result {
        return Err(if error.is_io() {
            FsError::new(ErrorCode::Efbig)
        } else {
            backend_error(error)
        });
    }
    String::from_utf8(out.0).map_err(backend_error)
}
async fn anchor(
    tx: &Transaction<'_>,
    volume: &str,
    backing: ConcurrentBackingId,
    budget: &mut Budget,
) -> Result<CompactAnchor> {
    let row=tx.query_typed_opt("SELECT revision,write_mode,backing_id,owner,fence,expires,delegation,CASE WHEN octet_length(namespace)<=$2 THEN namespace ELSE NULL END FROM mount_rs_metadata WHERE volume_key=$1",&[(&volume,Type::TEXT),(&(MAX_BODY as i32),Type::INT4)]).await.map_err(postgres_error)?.ok_or_else(stale)?;
    if row
        .try_get::<_, Option<String>>(1)
        .map_err(postgres_error)?
        .as_deref()
        != Some("MRC5")
        || row
            .try_get::<_, Option<String>>(2)
            .map_err(postgres_error)?
            .as_deref()
            != Some(backing.to_hex().as_str())
        || row
            .try_get::<_, Option<String>>(3)
            .map_err(postgres_error)?
            .is_some()
        || row.try_get::<_, i64>(4).map_err(postgres_error)? != CONCURRENT_FENCE_SENTINEL
        || row.try_get::<_, i64>(5).map_err(postgres_error)? != 0
        || row
            .try_get::<_, Option<String>>(6)
            .map_err(postgres_error)?
            .is_some()
    {
        return Err(stale());
    }
    let json = row
        .try_get::<_, Option<String>>(7)
        .map_err(postgres_error)?
        .ok_or_else(|| FsError::new(ErrorCode::Efbig))?;
    profile::add(Event::CompactAnchorReturned, json.len() as u64);
    budget.add(wire_bytes(volume, json.len())?)?;
    let a = decode_compact_anchor(json.as_bytes())?;
    anchor_bounds(&a)?;
    if a.generation
        != nonnegative(
            row.try_get(0).map_err(postgres_error)?,
            "compact generation",
        )?
        || a.backing != backing
    {
        return Err(stale());
    }
    Ok(a)
}
async fn guards(
    tx: &Transaction<'_>,
    volume: &str,
    selected: Option<u64>,
    locking: bool,
    maximum: usize,
    budget: &mut Budget,
) -> Result<BTreeMap<u64, CompactGuard>> {
    // Every returned row is charged >=4096 conservative framing/work bytes.
    // Refuse an anchor whose known membership cannot fit, before streaming;
    // this is a private accounting cap, not PostgreSQL's row/parameter limit.
    if selected.is_none() && maximum.saturating_sub(1) > (MAX_TOTAL - budget.0) / 4096 {
        return Err(FsError::new(ErrorCode::Efbig));
    }
    let selected = selected.map(inode_signed).transpose()?;
    let mut sql="SELECT inode,incarnation,epoch,revision,CASE WHEN octet_length(node)<=$3 THEN node ELSE NULL END FROM mount_rs_compact_guards WHERE volume_key=$1 AND ($2::bigint IS NULL OR inode=$2) ORDER BY inode LIMIT $4".to_owned();
    if locking {
        sql.push_str(" FOR UPDATE");
    }
    let stream = tx
        .query_typed_raw(
            &sql,
            [
                (
                    &volume as &(dyn tokio_postgres::types::ToSql + Sync),
                    Type::TEXT,
                ),
                (&selected, Type::INT8),
                (&(MAX_BODY as i32), Type::INT4),
                (&(maximum as i64), Type::INT8),
            ],
        )
        .await
        .map_err(postgres_error)?;
    tokio::pin!(stream);
    let mut result = BTreeMap::new();
    while let Some(row) = stream.next().await {
        let row = row.map_err(postgres_error)?;
        let json = row
            .try_get::<_, Option<String>>(4)
            .map_err(postgres_error)?
            .ok_or_else(|| FsError::new(ErrorCode::Efbig))?;
        profile::add(Event::InodeReturned, json.len() as u64);
        budget.add(wire_bytes(volume, json.len())?)?;
        let inode = nonnegative(row.try_get(0).map_err(postgres_error)?, "compact inode")?;
        let identity = PhysicalInodeIdentity {
            incarnation: nonnegative(
                row.try_get(1).map_err(postgres_error)?,
                "compact incarnation",
            )?,
            epoch: nonnegative(row.try_get(2).map_err(postgres_error)?, "compact epoch")?,
            revision: nonnegative(row.try_get(3).map_err(postgres_error)?, "compact revision")?,
        };
        let node: NodeMetadata = serde_json::from_str(&json).map_err(backend_error)?;
        // Full body is authoritative. Refuse unknown/duplicate/noncanonical JSON
        // fields rather than silently losing them on the next update. Account
        // this validation serialization as well as subsequent mutation encoding.
        budget.add(json.len())?;
        if encode_node(&node)? != json || inode == 0 || node.stats.ino != inode {
            return Err(stale());
        }
        validate_node_kind(&node)?;
        if result
            .insert(inode, CompactGuard { identity, node })
            .is_some()
        {
            return Err(stale());
        }
    }
    Ok(result)
}
async fn lock_root(tx: &Transaction<'_>, volume: &str) -> Result<()> {
    tx.query_typed_opt(
        "SELECT volume_key FROM mount_rs_metadata WHERE volume_key=$1 FOR UPDATE",
        &[(&volume, Type::TEXT)],
    )
    .await
    .map_err(postgres_error)?
    .ok_or_else(stale)?;
    Ok(())
}
async fn write_guard(
    tx: &Transaction<'_>,
    volume: &str,
    inode: u64,
    guard: &CompactGuard,
    json: &str,
    created: bool,
) -> Result<()> {
    let sql = if created {
        "INSERT INTO mount_rs_compact_guards(volume_key,inode,incarnation,epoch,revision,node) VALUES($1,$2,$3,$4,$5,$6)"
    } else {
        "UPDATE mount_rs_compact_guards SET incarnation=$3,epoch=$4,revision=$5,node=$6 WHERE volume_key=$1 AND inode=$2"
    };
    if tx
        .execute_typed(
            sql,
            &[
                (&volume, Type::TEXT),
                (&inode_signed(inode)?, Type::INT8),
                (&inode_signed(guard.identity.incarnation)?, Type::INT8),
                (&inode_signed(guard.identity.epoch)?, Type::INT8),
                (&inode_signed(guard.identity.revision)?, Type::INT8),
                (&json, Type::TEXT),
            ],
        )
        .await
        .map_err(postgres_error)?
        != 1
    {
        return Err(stale());
    }
    Ok(())
}
async fn commit(tx: Transaction<'_>) -> Result<()> {
    #[cfg(test)]
    super::compact_tests::checkpoint("precommit").await?;
    // A transport/COMMIT failure is never classified as retryable EAGAIN.
    tx.commit().await.map_err(postgres_error)?;
    #[cfg(test)]
    super::compact_tests::checkpoint("ack").await?;
    Ok(())
}

impl PgliteMetadataStore {
    pub(super) async fn compact_prepare(
        &self,
        backing: ConcurrentBackingId,
        expected: u64,
    ) -> Result<()> {
        key(&self.0.volume_key)?;
        inode_signed(expected)?;
        let generation = expected
            .checked_add(1)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        inode_signed(generation)?;
        let mut client = self.0.lock_client().await?;
        let tx = client
            .as_mut()
            .ok_or_else(connection_closed)?
            .build_transaction()
            .isolation_level(IsolationLevel::ReadCommitted)
            .start()
            .await
            .map_err(postgres_error)?;
        validate_schema(&tx).await?;
        lock_root(&tx, &self.0.volume_key).await?;
        let volume = &self.0.volume_key;
        let row=tx.query_typed_one("SELECT write_mode,backing_id,owner,fence,expires,revision,delegation,CASE WHEN octet_length(namespace)<=$2 THEN namespace ELSE NULL END FROM mount_rs_metadata WHERE volume_key=$1",&[(&volume,Type::TEXT),(&(MAX_BODY as i32),Type::INT4)]).await.map_err(postgres_error)?;
        if row
            .try_get::<_, Option<String>>(0)
            .map_err(postgres_error)?
            .as_deref()
            != Some("MRC2")
            || row
                .try_get::<_, Option<String>>(1)
                .map_err(postgres_error)?
                .as_deref()
                != Some(backing.to_hex().as_str())
            || row
                .try_get::<_, Option<String>>(2)
                .map_err(postgres_error)?
                .is_some()
            || row.try_get::<_, i64>(3).map_err(postgres_error)? != CONCURRENT_FENCE_SENTINEL
            || row.try_get::<_, i64>(4).map_err(postgres_error)? != 0
            || row
                .try_get::<_, Option<String>>(6)
                .map_err(postgres_error)?
                .is_some()
        {
            return Err(stale());
        }
        if expected == 0
            || row.try_get::<_, i64>(5).map_err(postgres_error)? != inode_signed(expected)?
        {
            return Err(inode_conflict());
        }
        let json = row
            .try_get::<_, Option<String>>(7)
            .map_err(postgres_error)?
            .ok_or_else(|| FsError::new(ErrorCode::Efbig))?;
        profile::add(Event::NamespaceReturned, json.len() as u64);
        let ns: Namespace = serde_json::from_str(&json).map_err(backend_error)?;
        ns.validate()?;
        let root = ns.nodes.get(&ns.root).ok_or_else(stale)?;
        if ns.nodes.len() != 1
            || ns.next_inode
                != ns
                    .root
                    .checked_add(1)
                    .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?
            || !matches!(&root.data,NodeData::Directory{entries} if entries.is_empty())
        {
            return Err(FsError::new(ErrorCode::Ebusy));
        }
        let clean:bool=tx.query_typed_one("SELECT NOT EXISTS(SELECT 1 FROM mount_rs_inode_guards WHERE volume_key=$1) AND NOT EXISTS(SELECT 1 FROM mount_rs_compact_guards WHERE volume_key=$1) AND NOT EXISTS(SELECT 1 FROM mount_rs_versions WHERE volume_key=$1) AND NOT EXISTS(SELECT 1 FROM mount_rs_version_pins WHERE volume_key=$1) AND EXISTS(SELECT 1 FROM mount_rs_version_state WHERE volume_key=$1 AND head_id IS NULL AND next_sequence=1 AND next_read_fence=0) AND EXISTS(SELECT 1 FROM mount_rs_block_authority WHERE volume_key=$1 AND backing_id=$2)",&[(&volume,Type::TEXT),(&backing.to_hex(),Type::TEXT)]).await.map_err(postgres_error)?.try_get(0).map_err(postgres_error)?;
        if !clean {
            return Err(FsError::new(ErrorCode::Ebusy));
        }
        let a = CompactAnchor {
            backing,
            generation,
            root: ns.root,
            next_inode: ns.next_inode,
            default_uid: ns.default_uid,
            default_gid: ns.default_gid,
            umask: ns.umask,
            default_chunker: ns.default_chunker.clone(),
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
        let body = encode_anchor(&a)?;
        let node = encode_node(root)?;
        let mut budget = Budget::default();
        budget.add(json.len())?;
        budget.add(body.len())?;
        budget.add(node.len())?;
        budget.add(wire_bytes(volume, body.len())?)?;
        budget.add(wire_bytes(volume, node.len())?)?;
        write_guard(&tx, volume, ns.root, &guard, &node, true).await?;
        tx.execute_typed("UPDATE mount_rs_metadata SET write_mode='MRC5',revision=$2,namespace=$3 WHERE volume_key=$1",&[(&volume,Type::TEXT),(&inode_signed(generation)?,Type::INT8),(&body,Type::TEXT)]).await.map_err(postgres_error)?;
        commit(tx).await
    }
    pub(super) async fn compact_snapshot(
        &self,
        backing: ConcurrentBackingId,
    ) -> Result<CompactSnapshot> {
        key(&self.0.volume_key)?;
        let mut client = self.0.lock_client().await?;
        let tx = client
            .as_mut()
            .ok_or_else(connection_closed)?
            .build_transaction()
            .isolation_level(IsolationLevel::RepeatableRead)
            .read_only(true)
            .start()
            .await
            .map_err(postgres_error)?;
        let mut budget = Budget::default();
        let a = anchor(&tx, &self.0.volume_key, backing, &mut budget).await?;
        #[cfg(test)]
        super::compact_tests::checkpoint("snapshot_anchor").await?;
        let rows = guards(
            &tx,
            &self.0.volume_key,
            None,
            false,
            a.members.len().saturating_add(1),
            &mut budget,
        )
        .await?;
        let result = CompactSnapshot {
            anchor: a,
            guards: rows,
        };
        result.namespace()?;
        tx.rollback().await.map_err(postgres_error)?;
        Ok(result)
    }
    pub(super) async fn compact_load(
        &self,
        backing: ConcurrentBackingId,
        inode: u64,
    ) -> Result<LoadedCompactInode> {
        key(&self.0.volume_key)?;
        inode_signed(inode)?;
        let mut client = self.0.lock_client().await?;
        let tx = client
            .as_mut()
            .ok_or_else(connection_closed)?
            .build_transaction()
            .isolation_level(IsolationLevel::RepeatableRead)
            .read_only(true)
            .start()
            .await
            .map_err(postgres_error)?;
        let mut budget = Budget::default();
        let a = anchor(&tx, &self.0.volume_key, backing, &mut budget).await?;
        #[cfg(test)]
        super::compact_tests::checkpoint("selected_anchor").await?;
        let g = guards(&tx, &self.0.volume_key, Some(inode), false, 1, &mut budget)
            .await?
            .remove(&inode)
            .ok_or_else(stale)?;
        let loaded = LoadedCompactInode::from_guard(&a, inode, g)?;
        tx.rollback().await.map_err(postgres_error)?;
        Ok(loaded)
    }
    pub(super) async fn compact_publish_inode(
        &self,
        backing: ConcurrentBackingId,
        inode: u64,
        generation: u64,
        expected: PhysicalInodeIdentity,
        node: NodeMetadata,
    ) -> Result<LoadedCompactInode> {
        let volume = &self.0.volume_key;
        key(volume)?;
        inode_signed(inode)?;
        inode_signed(generation)?;
        identity_bounds(expected)?;
        // Encode and bound caller input even when it will conflict; reuse this
        // exact encoding for the mutation (the validator does not edit the body).
        let json = encode_node(&node)?;
        let mut writes = Budget::default();
        writes.add(wire_bytes(volume, json.len())?)?;
        writes.add(json.len())?;
        let mut client = self.0.lock_client().await?;
        let tx = client
            .as_mut()
            .ok_or_else(connection_closed)?
            .build_transaction()
            .isolation_level(IsolationLevel::ReadCommitted)
            .start()
            .await
            .map_err(postgres_error)?;
        let mut reads = writes;
        let current = guards(&tx, volume, Some(inode), true, 1, &mut reads)
            .await?
            .remove(&inode)
            .ok_or_else(stale)?;
        #[cfg(test)]
        super::compact_tests::checkpoint("selected_lock").await?;
        // Fresh RC statement after waiting: root is never write-locked here.
        let a = anchor(&tx, volume, backing, &mut reads).await?;
        let guard =
            validate_selected_update(&a, backing, generation, inode, &current, expected, node)?;
        identity_bounds(guard.identity)?;
        write_guard(&tx, volume, inode, &guard, &json, false).await?;
        commit(tx).await?;
        Ok(LoadedCompactInode {
            generation: a.generation,
            guard,
        })
    }
    pub(super) async fn compact_publish_structure(
        &self,
        delta: &CompactStructuralDelta,
    ) -> Result<CompactPublication> {
        let volume = &self.0.volume_key;
        key(volume)?;
        anchor_bounds(delta.base_anchor())?;
        let body = encode_anchor(delta.next_anchor())?;
        let mut writes = Budget::default();
        writes.add(wire_bytes(volume, body.len())?)?;
        writes.add(body.len())?;
        let mut encoded = BTreeMap::new();
        for (&inode, node) in delta.changed().iter().chain(delta.created()) {
            inode_signed(inode)?;
            let json = encode_node(node)?;
            writes.add(wire_bytes(volume, json.len())?)?;
            writes.add(json.len())?;
            encoded.insert(inode, json);
        }
        writes.add(
            delta
                .expected()
                .len()
                .checked_mul(40)
                .ok_or_else(|| FsError::new(ErrorCode::Efbig))?,
        )?;
        for (&inode, &id) in delta.expected() {
            inode_signed(inode)?;
            identity_bounds(id)?;
        }
        for &inode in delta.removed() {
            inode_signed(inode)?;
            writes.add(wire_bytes(volume, 0)?)?;
        }
        let mut client = self.0.lock_client().await?;
        let tx = client
            .as_mut()
            .ok_or_else(connection_closed)?
            .build_transaction()
            .isolation_level(IsolationLevel::ReadCommitted)
            .start()
            .await
            .map_err(postgres_error)?;
        // Every compact API insert/delete holds root; locking all existing
        // guards then auditing membership protects Full without claiming gap locks.
        lock_root(&tx, volume).await?;
        #[cfg(test)]
        super::compact_tests::checkpoint("structure_lock").await?;
        let mut reads = writes;
        let a = anchor(&tx, volume, delta.base_anchor().backing, &mut reads).await?;
        let current = match delta.scope() {
            StructuralScope::Full => {
                guards(
                    &tx,
                    volume,
                    None,
                    true,
                    a.members.len().saturating_add(1),
                    &mut reads,
                )
                .await?
            }
            StructuralScope::FileCreate => {
                let mut rows = BTreeMap::new();
                for &id in delta.expected().keys() {
                    rows.extend(guards(&tx, volume, Some(id), true, 1, &mut reads).await?);
                }
                rows
            }
        };
        // FileCreate checks new-key absence independently of anchor membership,
        // without returning or serializing any unrelated guard body.
        for &inode in delta.created().keys() {
            if tx
                .query_typed_opt(
                    "SELECT inode FROM mount_rs_compact_guards WHERE volume_key=$1 AND inode=$2",
                    &[(&volume, Type::TEXT), (&inode_signed(inode)?, Type::INT8)],
                )
                .await
                .map_err(postgres_error)?
                .is_some()
            {
                return Err(inode_conflict());
            }
        }
        let publication = delta.validate_current(&a, &current)?;
        for (&inode, guard) in &publication.upserts {
            inode_signed(inode)?;
            identity_bounds(guard.identity)?;
        }
        // All bytes/SQL integers checked, all expected guards held, before DML.
        for &inode in &publication.removed {
            if tx
                .execute_typed(
                    "DELETE FROM mount_rs_compact_guards WHERE volume_key=$1 AND inode=$2",
                    &[(&volume, Type::TEXT), (&inode_signed(inode)?, Type::INT8)],
                )
                .await
                .map_err(postgres_error)?
                != 1
            {
                return Err(stale());
            }
        }
        for (&inode, guard) in &publication.upserts {
            write_guard(
                &tx,
                volume,
                inode,
                guard,
                &encoded[&inode],
                delta.created().contains_key(&inode),
            )
            .await?;
        }
        tx.execute_typed(
            "UPDATE mount_rs_metadata SET revision=$2,namespace=$3 WHERE volume_key=$1",
            &[
                (&volume, Type::TEXT),
                (&inode_signed(publication.anchor.generation)?, Type::INT8),
                (&body, Type::TEXT),
            ],
        )
        .await
        .map_err(postgres_error)?;
        commit(tx).await?;
        Ok(publication)
    }
}
