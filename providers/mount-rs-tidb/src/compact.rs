//! MRC5 consistent reads and explicit publication transactions; MRC4 authority stays distinct.
use super::*;
use mount_rs_core::storage::{NodeData, compact::*};
use mysql_async::{Row, Value, consts::StatusFlags, from_value_opt, prelude::FromValue};

const TABLE: &str = "mount_rs_tidb_compact_guards";
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS mount_rs_tidb_compact_guards (
    volume_key VARBINARY(1020) NOT NULL,
    inode BIGINT NOT NULL,
    incarnation BIGINT NOT NULL,
    epoch BIGINT NOT NULL,
    revision BIGINT NOT NULL,
    node LONGTEXT NOT NULL,
    PRIMARY KEY(volume_key,inode)
)";

const SELECTED_JOINED_SQL: &str = "SELECT m.revision,m.write_mode,m.backing_id,m.owner,m.fence,m.expires,m.namespace,m.delegation,g.inode,g.incarnation,g.epoch,g.revision,g.node FROM mount_rs_tidb_metadata AS m LEFT JOIN mount_rs_tidb_compact_guards AS g ON g.volume_key=m.volume_key AND g.inode=? WHERE m.volume_key=?";

type GuardRow = (i64, i64, i64, i64, String);
type AnchorRow = (
    i64,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    i64,
    i64,
    Option<String>,
    Option<String>,
);
type Column = (String, String, String, String, String);

pub(super) async fn initialize(conn: &mut Conn) -> Result<()> {
    conn.query_drop_observed(StorageOperation::TidbSqlDdl, SCHEMA)
        .await
        .map_err(|e| db_error("initialize TiDB compact guards", e))?;
    validate_schema(conn, TABLE).await
}

async fn validate_schema<C: Queryable>(conn: &mut C, table: &str) -> Result<()> {
    let columns: Vec<Column> = conn.exec_observed(StorageOperation::TidbSqlMetadataRead,
        "SELECT COLUMN_NAME,DATA_TYPE,COLUMN_TYPE,IS_NULLABLE,EXTRA FROM INFORMATION_SCHEMA.COLUMNS WHERE TABLE_SCHEMA=DATABASE() AND TABLE_NAME=? ORDER BY ORDINAL_POSITION", (table,)
    ).await.map_err(|e| db_error("validate TiDB compact columns", e))?;
    let expected = [
        ("volume_key", "varbinary"),
        ("inode", "bigint"),
        ("incarnation", "bigint"),
        ("epoch", "bigint"),
        ("revision", "bigint"),
        ("node", "longtext"),
    ];
    if columns.len() != expected.len()
        || columns
            .iter()
            .zip(expected)
            .any(|((name, kind, full, nullable, extra), (n, t))| {
                name != n
                    || kind != t
                    || nullable != "NO"
                    || !extra.is_empty()
                    || full.contains("unsigned")
                    || (n == "volume_key" && full != "varbinary(1020)")
            })
    {
        return Err(backend_error("incompatible TiDB compact guard columns"));
    }
    let indexes: Vec<(String, u64, String, Option<u64>)> = conn.exec_observed(StorageOperation::TidbSqlMetadataRead,
        "SELECT INDEX_NAME,SEQ_IN_INDEX,COLUMN_NAME,SUB_PART FROM INFORMATION_SCHEMA.STATISTICS WHERE TABLE_SCHEMA=DATABASE() AND TABLE_NAME=? ORDER BY INDEX_NAME,SEQ_IN_INDEX", (table,)
    ).await.map_err(|e| db_error("validate TiDB compact primary key", e))?;
    if indexes
        != vec![
            ("PRIMARY".into(), 1, "volume_key".into(), None),
            ("PRIMARY".into(), 2, "inode".into(), None),
        ]
    {
        return Err(backend_error("incompatible TiDB compact guard primary key"));
    }
    Ok(())
}

async fn anchor<C: Queryable>(
    conn: &mut C,
    volume: &str,
    backing: ConcurrentBackingId,
) -> Result<CompactAnchor> {
    let row: AnchorRow = conn.exec_first_observed(StorageOperation::TidbSqlMetadataRead,
        "SELECT revision,write_mode,backing_id,owner,fence,expires,namespace,delegation FROM mount_rs_tidb_metadata WHERE volume_key=?", (volume,)
    ).await.map_err(|e| db_error("read TiDB compact anchor", e))?.ok_or_else(stale)?;
    decode_anchor_row(row, backing)
}

fn decode_anchor_row(row: AnchorRow, backing: ConcurrentBackingId) -> Result<CompactAnchor> {
    if row.1.as_deref() != Some(b"MRC5")
        || row.2.as_deref() != Some(backing.to_hex().as_bytes())
        || row.3.is_some()
        || row.4 != CONCURRENT_FENCE_SENTINEL
        || row.5 != 0
        || row.7.is_some()
    {
        return Err(stale());
    }
    let json = row.6.ok_or_else(stale)?;
    profile::add(Event::CompactAnchorReturned, json.len() as u64);
    let anchor = decode_compact_anchor(json.as_bytes())?;
    if nonnegative(row.0, "compact generation")? != anchor.generation || anchor.backing != backing {
        return Err(backend_error("TiDB compact anchor authority mismatch"));
    }
    Ok(anchor)
}

async fn guards<C: Queryable>(
    conn: &mut C,
    volume: &str,
    selected: Option<u64>,
    locking: bool,
) -> Result<BTreeMap<u64, CompactGuard>> {
    let suffix = if locking { " FOR UPDATE" } else { "" };
    let mut sql = "SELECT inode,incarnation,epoch,revision,node FROM mount_rs_tidb_compact_guards WHERE volume_key=?".to_owned();
    let params = if let Some(inode) = selected {
        sql.push_str(" AND inode=?");
        Params::from((volume, signed(inode, "compact inode")?))
    } else {
        Params::from((volume,))
    };
    sql.push_str(" ORDER BY inode");
    sql.push_str(suffix);
    let rows: Vec<GuardRow> = conn
        .exec_observed(StorageOperation::TidbSqlInodeRead, sql, params)
        .await
        .map_err(|e| db_error("read TiDB compact guards", e))?;
    let mut guards = BTreeMap::new();
    for row in rows {
        let (inode, guard) = decode_guard_row(row)?;
        if guards.insert(inode, guard).is_some() {
            return Err(backend_error("duplicate TiDB compact guard"));
        }
    }
    Ok(guards)
}

fn decode_guard_row(row: GuardRow) -> Result<(u64, CompactGuard)> {
    let (inode, incarnation, epoch, revision, json) = row;
    profile::add(Event::InodeReturned, json.len() as u64);
    let inode = nonnegative(inode, "compact inode")?;
    let guard = CompactGuard {
        identity: PhysicalInodeIdentity {
            incarnation: nonnegative(incarnation, "compact incarnation")?,
            epoch: nonnegative(epoch, "compact epoch")?,
            revision: nonnegative(revision, "compact revision")?,
        },
        node: serde_json::from_str(&json).map_err(backend_error)?,
    };
    Ok((inode, guard))
}

fn joined_field<T: FromValue>(values: &mut [Option<Value>], index: usize) -> Result<T> {
    let value = values
        .get_mut(index)
        .and_then(Option::take)
        .ok_or_else(|| backend_error("incomplete TiDB compact joined row"))?;
    from_value_opt(value).map_err(|_| backend_error("invalid TiDB compact joined field"))
}

fn decode_joined_rows(
    rows: impl IntoIterator<Item = Vec<Option<Value>>>,
    backing: ConcurrentBackingId,
    inode: u64,
) -> Result<LoadedCompactInode> {
    let mut rows = rows.into_iter();
    let mut values = rows.next().ok_or_else(stale)?;
    if rows.next().is_some() {
        return Err(backend_error("duplicate TiDB compact joined row"));
    }
    if values.len() != 13 {
        return Err(backend_error("invalid TiDB compact joined row shape"));
    }
    // mysql_common's typed tuples stop at 12 columns. Decode all 13 fields
    // without cloning bodies or allowing conversion failures to panic.
    // Authority precedes missing, malformed or out-of-range guard handling.
    let anchor = decode_anchor_row(
        (
            joined_field(&mut values, 0)?,
            joined_field(&mut values, 1)?,
            joined_field(&mut values, 2)?,
            joined_field(&mut values, 3)?,
            joined_field(&mut values, 4)?,
            joined_field(&mut values, 5)?,
            joined_field(&mut values, 6)?,
            joined_field(&mut values, 7)?,
        ),
        backing,
    )?;
    signed(inode, "compact inode")?;
    // A LEFT JOIN without a guard produces five NULLs. Partial NULL rows are
    // corrupt and must reach checked conversion rather than look absent.
    if values[8..]
        .iter()
        .all(|value| matches!(value, Some(Value::NULL)))
    {
        return Err(stale());
    }
    let (selected, guard) = decode_guard_row((
        joined_field(&mut values, 8)?,
        joined_field(&mut values, 9)?,
        joined_field(&mut values, 10)?,
        joined_field(&mut values, 11)?,
        joined_field(&mut values, 12)?,
    ))?;
    if selected != inode {
        return Err(stale());
    }
    LoadedCompactInode::from_guard(&anchor, inode, guard)
}

// Borrow public Row values while their receive buffers remain owned by the
// result. Unusual driver representations use the existing checked conversion.
fn check_joined_unchanged(
    rows: &[Row],
    backing: ConcurrentBackingId,
    inode: u64,
    expected: CompactInodeExpectation<'_>,
) -> Option<CheckedCompactInode> {
    let [row] = rows else {
        return None;
    };
    if row.len() != 13 {
        return None;
    }
    let integer = |index| match row.as_ref(index)? {
        Value::Int(value) => Some(*value),
        _ => None,
    };
    let bytes = |index| match row.as_ref(index)? {
        Value::Bytes(value) => Some(value.as_slice()),
        _ => None,
    };
    if bytes(1)? != b"MRC5"
        || ConcurrentBackingId::from_hex(std::str::from_utf8(bytes(2)?).ok()?).ok()? != backing
        || !matches!(row.as_ref(3), Some(Value::NULL))
        || integer(4)? != CONCURRENT_FENCE_SENTINEL
        || integer(5)? != 0
        || !matches!(row.as_ref(7), Some(Value::NULL))
    {
        return None;
    }
    let generation = u64::try_from(integer(0)?).ok()?;
    if signed(inode, "compact inode").is_err() || u64::try_from(integer(8)?).ok()? != inode {
        return None;
    }
    let identity = PhysicalInodeIdentity {
        incarnation: u64::try_from(integer(9)?).ok()?,
        epoch: u64::try_from(integer(10)?).ok()?,
        revision: u64::try_from(integer(11)?).ok()?,
    };
    let anchor = bytes(6)?;
    let node = bytes(12)?;
    let checked = check_compact_inode_unchanged(
        anchor, backing, generation, inode, identity, node, expected,
    )?;
    profile::add(Event::CompactAnchorReturned, anchor.len() as u64);
    profile::add(Event::InodeReturned, node.len() as u64);
    Some(checked)
}

fn encode_anchor(anchor: &CompactAnchor) -> Result<Vec<u8>> {
    signed(anchor.generation, "compact generation")?;
    signed(anchor.next_inode, "compact next inode")?;
    for &inode in &anchor.members {
        signed(inode, "compact member")?;
    }
    let bytes = encode_compact_anchor(anchor)?;
    profile::add(Event::CompactAnchorSerialized, bytes.len() as u64);
    Ok(bytes)
}
fn encode_guard(inode: u64, guard: &CompactGuard) -> Result<String> {
    signed(inode, "compact inode")?;
    signed(guard.identity.incarnation, "compact incarnation")?;
    signed(guard.identity.epoch, "compact epoch")?;
    signed(guard.identity.revision, "compact revision")?;
    let json = serde_json::to_string(&guard.node).map_err(backend_error)?;
    profile::add(Event::InodeSerialized, json.len() as u64);
    Ok(json)
}
async fn packet_budget(tx: &mut Transaction<'_>) -> Result<usize> {
    let client = tx.opts().max_allowed_packet().unwrap_or(usize::MAX);
    let session: u64 = tx
        .query_first_observed(
            StorageOperation::TidbSqlSession,
            "SELECT @@SESSION.max_allowed_packet",
        )
        .await
        .map_err(|e| db_error("read TiDB compact packet budget", e))?
        .ok_or_else(stale)?;
    Ok(client.min(usize::try_from(session).unwrap_or(usize::MAX)))
}
fn check_bytes(db: &Database, bytes: usize, packet: usize) -> Result<()> {
    // All statements carry one volume, one body, <=5 integers. This conservative
    // budget covers both SQL prepare and binary execution packets/length prefixes.
    let wire = bytes
        .checked_add(db.volume_key.len())
        .and_then(|n| n.checked_add(1024));
    if bytes > db.max_namespace_bytes || wire.is_none_or(|n| n > packet) {
        return Err(FsError::new(ErrorCode::Efbig));
    }
    Ok(())
}
async fn write_guard(
    tx: &mut Transaction<'_>,
    volume: &str,
    inode: u64,
    guard: &CompactGuard,
    json: String,
    created: bool,
) -> Result<()> {
    let sql = if created {
        "INSERT INTO mount_rs_tidb_compact_guards(incarnation,epoch,revision,node,volume_key,inode) VALUES(?,?,?,?,?,?)"
    } else {
        "UPDATE mount_rs_tidb_compact_guards SET incarnation=?,epoch=?,revision=?,node=? WHERE volume_key=? AND inode=?"
    };
    let changed = changed_query(
        StorageOperation::TidbSqlInodeWrite,
        tx,
        sql,
        (
            signed(guard.identity.incarnation, "compact incarnation")?,
            signed(guard.identity.epoch, "compact epoch")?,
            signed(guard.identity.revision, "compact revision")?,
            json,
            volume,
            signed(inode, "compact inode")?,
        ),
        "write TiDB compact guard",
    )
    .await?;
    if changed != 1 {
        return Err(stale());
    }
    Ok(())
}
fn selected_session_ready(flags: Option<StatusFlags>) -> bool {
    flags.is_some_and(|flags| {
        flags.contains(StatusFlags::SERVER_STATUS_AUTOCOMMIT)
            && !flags.intersects(
                StatusFlags::SERVER_STATUS_IN_TRANS | StatusFlags::SERVER_STATUS_IN_TRANS_READONLY,
            )
    })
}

async fn read_transaction(conn: &mut Conn) -> Result<Transaction<'_>> {
    // Nonlocking consistent reads share a single TiDB start_ts even when selected
    // writes commit between statements without advancing anchor generation.
    let mut opts = TxOpts::default();
    opts.with_isolation_level(IsolationLevel::RepeatableRead);
    observe_result_future(
        StorageOperation::TidbBeginCompactRead,
        conn.start_transaction(opts),
        0,
    )
    .await
    .map_err(|e| db_error("begin TiDB compact snapshot", e))
}

async fn require_no_compact_markers<C: Queryable>(
    tx: &mut C,
    volume: &str,
    namespace: Option<&str>,
) -> Result<()> {
    if namespace.is_some_and(|body| decode_compact_anchor(body.as_bytes()).is_ok()) {
        return Err(stale());
    }
    let guard: Option<u8> = tx
        .exec_first_observed(
            StorageOperation::TidbSqlInodeRead,
            "SELECT 1 FROM mount_rs_tidb_compact_guards WHERE volume_key=? LIMIT 1",
            (volume,),
        )
        .await
        .map_err(|e| db_error("inspect TiDB compact guards", e))?;
    if guard.is_some() {
        return Err(stale());
    }
    Ok(())
}

impl TidbMetadataStore {
    pub(super) async fn compact_inspect(&self) -> Result<Option<InodeModeState>> {
        let mut conn = self
            .0
            .pool
            .get_conn_observed()
            .await
            .map_err(|e| db_error("inspect TiDB compact mode", e))?;
        let mut tx = read_transaction(&mut conn).await?;
        let row: AnchorRow = tx.exec_first_observed(StorageOperation::TidbSqlMetadataRead,
            "SELECT revision,write_mode,backing_id,owner,fence,expires,namespace,delegation FROM mount_rs_tidb_metadata WHERE volume_key=?",
            (&self.0.volume_key,),
        ).await.map_err(|e| db_error("inspect TiDB compact mode", e))?.ok_or_else(stale)?;
        let result = match (row.1.as_deref(), row.2.as_deref()) {
            (Some(b"MRC5"), Some(id)) => {
                let backing = backing_from_bytes(id)?;
                let anchor = decode_anchor_row(row, backing)?;
                Ok(Some(InodeModeState {
                    backing,
                    structural_generation: anchor.generation,
                }))
            }
            (Some(b"MRC4"), Some(_)) => {
                compact_inode_authority((row.0, row.1, row.2, row.3, row.4, row.5), None)?;
                if row.6.is_none() || row.7.is_some() {
                    return Err(stale());
                }
                require_no_compact_markers(&mut tx, &self.0.volume_key, row.6.as_deref()).await?;
                Ok(None)
            }
            (Some(b"MRC2"), Some(id)) => {
                backing_from_bytes(id)?;
                if row.3.is_some()
                    || row.4 != CONCURRENT_FENCE_SENTINEL
                    || row.5 != 0
                    || row.7.is_some()
                {
                    return Err(stale());
                }
                require_no_compact_markers(&mut tx, &self.0.volume_key, row.6.as_deref()).await?;
                Ok(None)
            }
            (None, None) | (Some(b"MRC1"), None) if row.7.is_none() => {
                if (row.1.is_some()
                    && (row.3.is_some() || row.4 != CONCURRENT_FENCE_SENTINEL || row.5 != 0))
                    || (row.1.is_none() && row.4 == CONCURRENT_FENCE_SENTINEL)
                {
                    return Err(stale());
                }
                require_no_compact_markers(&mut tx, &self.0.volume_key, row.6.as_deref()).await?;
                Ok(None)
            }
            _ => Err(stale()),
        }?;
        observe_result_future(StorageOperation::TidbRollback, tx.rollback(), 0)
            .await
            .map_err(|e| db_error("finish TiDB compact inspection", e))?;
        Ok(result)
    }
    pub(super) async fn compact_prepare(
        &self,
        backing: ConcurrentBackingId,
        expected: u64,
    ) -> Result<()> {
        let generation = expected
            .checked_add(1)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        signed(generation, "compact generation")?;
        let mut conn = self
            .0
            .pool
            .get_conn_observed()
            .await
            .map_err(|e| db_error("enroll TiDB compact", e))?;
        let mut tx = begin_inode_write(&mut conn).await?;
        validate_schema(&mut tx, TABLE).await?;
        locked_lease_row(&mut tx, &self.0.volume_key)
            .await?
            .ok_or_else(stale)?;
        let row = concurrent_row(&mut tx, &self.0.volume_key).await?;
        if row.mode_state()? != ConcurrentModeState::Mrc2(backing) {
            return Err(stale());
        }
        if row.revision != expected || expected == 0 {
            return Err(FsError::new(ErrorCode::Eagain));
        }
        let (json, delegation): (String, Option<String>) = tx
            .exec_first_observed(
                StorageOperation::TidbSqlMetadataRead,
                "SELECT namespace,delegation FROM mount_rs_tidb_metadata WHERE volume_key=?",
                (&self.0.volume_key,),
            )
            .await
            .map_err(|e| db_error("read TiDB compact enrollment", e))?
            .ok_or_else(stale)?;
        profile::add(Event::NamespaceReturned, json.len() as u64);
        let ns: Namespace = serde_json::from_str(&json).map_err(backend_error)?;
        ns.validate()?;
        let root = ns.nodes.get(&ns.root).ok_or_else(stale)?;
        if delegation.is_some()
            || ns.nodes.len() != 1
            || ns.next_inode
                != ns
                    .root
                    .checked_add(1)
                    .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?
            || !matches!(&root.data, NodeData::Directory { entries } if entries.is_empty())
        {
            return Err(FsError::new(ErrorCode::Ebusy));
        }
        let count: u64 = tx.exec_first_observed(StorageOperation::TidbSqlMetadataRead, "SELECT (SELECT count(*) FROM mount_rs_tidb_inodes WHERE volume_key=?)+(SELECT count(*) FROM mount_rs_tidb_compact_guards WHERE volume_key=?)", (&self.0.volume_key, &self.0.volume_key)).await.map_err(|e| db_error("check TiDB compact enrollment history", e))?.ok_or_else(stale)?;
        if count != 0 {
            return Err(FsError::new(ErrorCode::Ebusy));
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
        let root = CompactGuard {
            identity: PhysicalInodeIdentity {
                incarnation: generation,
                epoch: generation,
                revision: 0,
            },
            node: root.clone(),
        };
        let body = encode_anchor(&anchor)?;
        let node = encode_guard(ns.root, &root)?;
        let budget = packet_budget(&mut tx).await?;
        check_bytes(&self.0, body.len(), budget)?;
        check_bytes(&self.0, node.len(), budget)?;
        write_guard(&mut tx, &self.0.volume_key, ns.root, &root, node, true).await?;
        tx.exec_drop_observed(StorageOperation::TidbSqlMetadataWrite, "UPDATE mount_rs_tidb_metadata SET write_mode='MRC5',revision=?,namespace=? WHERE volume_key=?", (signed(generation,"compact generation")?, body, &self.0.volume_key)).await.map_err(|e| db_error("enroll TiDB compact anchor", e))?;
        commit(tx, "enroll compact mode").await
    }

    pub(super) async fn compact_snapshot(
        &self,
        backing: ConcurrentBackingId,
    ) -> Result<CompactSnapshot> {
        let mut conn = self
            .0
            .pool
            .get_conn_observed()
            .await
            .map_err(|e| db_error("load TiDB compact snapshot", e))?;
        let mut tx = read_transaction(&mut conn).await?;
        let anchor = anchor(&mut tx, &self.0.volume_key, backing).await?;
        let snapshot = CompactSnapshot {
            anchor,
            guards: guards(&mut tx, &self.0.volume_key, None, false).await?,
        };
        snapshot.namespace()?;
        observe_result_future(StorageOperation::TidbRollback, tx.rollback(), 0)
            .await
            .map_err(|e| db_error("finish TiDB compact snapshot", e))?;
        Ok(snapshot)
    }
    pub(super) async fn compact_load(
        &self,
        backing: ConcurrentBackingId,
        inode: u64,
    ) -> Result<LoadedCompactInode> {
        let rows = self.compact_selected_rows(inode).await?;
        decode_joined_rows(rows.into_iter().map(Row::unwrap_raw), backing, inode)
    }

    pub(super) async fn compact_read(
        &self,
        backing: ConcurrentBackingId,
        inode: u64,
        expected: CompactInodeExpectation<'_>,
    ) -> Result<CompactInodeRead> {
        let rows = self.compact_selected_rows(inode).await?;
        if let Some(checked) = check_joined_unchanged(&rows, backing, inode, expected) {
            return Ok(CompactInodeRead::Unchanged(checked));
        }
        decode_joined_rows(rows.into_iter().map(Row::unwrap_raw), backing, inode)
            .map(CompactInodeRead::Loaded)
    }

    async fn compact_selected_rows(&self, inode: u64) -> Result<Vec<Row>> {
        let mut conn = self
            .0
            .pool
            .get_conn_observed()
            .await
            .map_err(|e| db_error("load TiDB compact inode", e))?;
        // The private pool configures autocommit and finishes tracked dirty
        // cleanup before checkout. Require the latest server status witness;
        // never turn an unexpected active transaction into an implicit commit.
        if !selected_session_ready(conn.last_ok_packet().map(|packet| packet.status_flags())) {
            let _ = conn.disconnect().await;
            return Err(backend_error(
                "TiDB compact selected read requires verified autocommit outside a transaction",
            ));
        }
        // One nonlocking joined SELECT reads anchor and guard at one TiDB
        // statement snapshot. Full snapshots still require explicit RR.
        // Preserve authority-before-range-error precedence from the two-query
        // path. The decoder rejects an out-of-range inode before guard use.
        let selected = signed(inode, "compact inode").unwrap_or(0);
        let rows: Vec<Row> = conn
            .exec_observed(
                StorageOperation::TidbSqlInodeRead,
                SELECTED_JOINED_SQL,
                (selected, &self.0.volume_key),
            )
            .await
            .map_err(|e| db_error("read TiDB compact inode", e))?;
        Ok(rows)
    }
    pub(super) async fn compact_publish_inode(
        &self,
        backing: ConcurrentBackingId,
        inode: u64,
        generation: u64,
        expected: PhysicalInodeIdentity,
        node: NodeMetadata,
    ) -> Result<LoadedCompactInode> {
        let mut conn = self
            .0
            .pool
            .get_conn_observed()
            .await
            .map_err(|e| db_error("publish TiDB compact inode", e))?;
        let mut tx = begin_inode_write(&mut conn).await?;
        // Guard first, fresh nonlocking authority AFTER any wait. Selected writers
        // never acquire root authority, so structural root->guard has no lock cycle.
        let current = guards(&mut tx, &self.0.volume_key, Some(inode), true)
            .await?
            .remove(&inode)
            .ok_or_else(stale)?;
        let anchor = anchor(&mut tx, &self.0.volume_key, backing).await?;
        let guard = validate_selected_update(
            &anchor, backing, generation, inode, &current, expected, node,
        )?;
        let json = encode_guard(inode, &guard)?;
        let budget = packet_budget(&mut tx).await?;
        check_bytes(&self.0, json.len(), budget)?;
        write_guard(&mut tx, &self.0.volume_key, inode, &guard, json, false).await?;
        commit(tx, "publish compact inode").await?;
        Ok(LoadedCompactInode {
            generation: anchor.generation,
            guard,
        })
    }
    pub(super) async fn compact_publish_structure(
        &self,
        delta: &CompactStructuralDelta,
    ) -> Result<CompactPublication> {
        let mut conn = self
            .0
            .pool
            .get_conn_observed()
            .await
            .map_err(|e| db_error("publish TiDB compact structure", e))?;
        let mut tx = begin_inode_write(&mut conn).await?;
        // TiDB has no gap locks. Every API insert/delete/structural transition
        // holds this root authority lock; Full additionally locks every existing
        // guard and validates its exact membership against the immutable anchor.
        locked_lease_row(&mut tx, &self.0.volume_key)
            .await?
            .ok_or_else(stale)?;
        let anchor = anchor(&mut tx, &self.0.volume_key, delta.base_anchor().backing).await?;
        let current = match delta.scope() {
            StructuralScope::Full => guards(&mut tx, &self.0.volume_key, None, true).await?,
            StructuralScope::FileCreate => {
                let mut map = BTreeMap::new();
                for &inode in delta.expected().keys() {
                    map.extend(guards(&mut tx, &self.0.volume_key, Some(inode), true).await?);
                }
                map
            }
        };
        let publication = delta.validate_current(&anchor, &current)?;
        let body = encode_anchor(&publication.anchor)?;
        let budget = packet_budget(&mut tx).await?;
        check_bytes(&self.0, body.len(), budget)?;
        let mut encoded = BTreeMap::new();
        for (&inode, guard) in &publication.upserts {
            let json = encode_guard(inode, guard)?;
            check_bytes(&self.0, json.len(), budget)?;
            encoded.insert(inode, json);
        }
        for &inode in &publication.removed {
            signed(inode, "compact inode")?;
        }
        // No guard or anchor mutations occur before all SQL/byte checks above.
        for &inode in &publication.removed {
            if changed_query(
                StorageOperation::TidbSqlInodeWrite,
                &mut tx,
                "DELETE FROM mount_rs_tidb_compact_guards WHERE volume_key=? AND inode=?",
                (&self.0.volume_key, signed(inode, "compact inode")?),
                "delete TiDB compact guard",
            )
            .await?
                != 1
            {
                return Err(stale());
            }
        }
        for (&inode, guard) in &publication.upserts {
            write_guard(
                &mut tx,
                &self.0.volume_key,
                inode,
                guard,
                encoded.remove(&inode).expect("encoded upsert"),
                delta.created().contains_key(&inode),
            )
            .await?;
        }
        tx.exec_drop_observed(
            StorageOperation::TidbSqlMetadataWrite,
            "UPDATE mount_rs_tidb_metadata SET revision=?,namespace=? WHERE volume_key=?",
            (
                signed(publication.anchor.generation, "compact generation")?,
                body,
                &self.0.volume_key,
            ),
        )
        .await
        .map_err(|e| db_error("publish TiDB compact anchor", e))?;
        commit(tx, "publish compact structure").await?;
        Ok(publication)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_session_requires_autocommit_and_no_active_transaction() {
        assert!(!selected_session_ready(None));
        for flags in [
            StatusFlags::empty(),
            StatusFlags::SERVER_STATUS_IN_TRANS,
            StatusFlags::SERVER_STATUS_IN_TRANS_READONLY,
            StatusFlags::SERVER_STATUS_AUTOCOMMIT | StatusFlags::SERVER_STATUS_IN_TRANS,
            StatusFlags::SERVER_STATUS_AUTOCOMMIT | StatusFlags::SERVER_STATUS_IN_TRANS_READONLY,
            StatusFlags::SERVER_STATUS_AUTOCOMMIT
                | StatusFlags::SERVER_STATUS_IN_TRANS
                | StatusFlags::SERVER_STATUS_IN_TRANS_READONLY,
        ] {
            assert!(!selected_session_ready(Some(flags)), "{flags:?}");
        }
        assert!(selected_session_ready(Some(
            StatusFlags::SERVER_STATUS_AUTOCOMMIT
        )));
        assert!(selected_session_ready(Some(
            StatusFlags::SERVER_STATUS_AUTOCOMMIT | StatusFlags::SERVER_STATUS_NO_INDEX_USED
        )));
    }

    fn joined_fixture() -> (CompactAnchor, CompactGuard, Vec<Option<Value>>) {
        use mount_rs_core::chunking::{Chunker, FixedSizeChunker};
        use mount_rs_core::storage::FileLayout;
        use mount_rs_core::{S_IFREG, Stats};

        let chunker = FixedSizeChunker::new(4096).unwrap().config();
        let anchor = CompactAnchor {
            backing: ConcurrentBackingId::from_bytes([0x65; 16]).unwrap(),
            generation: 7,
            root: 1,
            next_inode: 3,
            default_uid: 0,
            default_gid: 0,
            umask: 0o022,
            default_chunker: chunker.clone(),
            members: vec![1, 2],
        };
        let guard = CompactGuard {
            identity: PhysicalInodeIdentity {
                incarnation: 2,
                epoch: 6,
                revision: 9,
            },
            node: NodeMetadata {
                stats: Stats {
                    dev: 0,
                    ino: 2,
                    mode: S_IFREG | 0o644,
                    nlink: 1,
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
                data: NodeData::File(FileLayout {
                    chunker,
                    extents: vec![],
                }),
            },
        };
        let row = vec![
            Some(Value::Int(7)),
            Some(Value::Bytes(b"MRC5".to_vec())),
            Some(Value::Bytes(anchor.backing.to_hex().into_bytes())),
            Some(Value::NULL),
            Some(Value::Int(CONCURRENT_FENCE_SENTINEL)),
            Some(Value::Int(0)),
            Some(Value::Bytes(encode_compact_anchor(&anchor).unwrap())),
            Some(Value::NULL),
            Some(Value::Int(2)),
            Some(Value::Int(2)),
            Some(Value::Int(6)),
            Some(Value::Int(9)),
            Some(Value::Bytes(serde_json::to_vec(&guard.node).unwrap())),
        ];
        (anchor, guard, row)
    }

    #[test]
    fn joined_selected_decodes_exact_body_and_physical_identity() {
        let (anchor, guard, row) = joined_fixture();
        let loaded = decode_joined_rows([row], anchor.backing, 2).unwrap();
        assert_eq!(
            loaded,
            LoadedCompactInode {
                generation: 7,
                guard,
            }
        );
    }

    #[test]
    fn joined_selected_missing_authority_or_guard_is_stale() {
        let (anchor, _, mut row) = joined_fixture();
        assert!(
            decode_joined_rows(Vec::new(), anchor.backing, 2)
                .unwrap_err()
                .is(ErrorCode::Estale)
        );
        row[8..].fill(Some(Value::NULL));
        assert!(
            decode_joined_rows([row], anchor.backing, 2)
                .unwrap_err()
                .is(ErrorCode::Estale)
        );
    }

    #[test]
    fn joined_selected_checks_authority_before_malformed_guard() {
        let (anchor, _, original) = joined_fixture();
        let invalid_authority = [
            (1, Value::Bytes(b"MRC4".to_vec())),
            (2, Value::Bytes(vec![b'0'; 32])),
            (3, Value::Bytes(b"owner".to_vec())),
            (4, Value::Int(0)),
            (5, Value::Int(1)),
            (6, Value::NULL),
            (7, Value::Bytes(b"{}".to_vec())),
        ];
        for (index, value) in invalid_authority {
            let mut row = original.clone();
            row[index] = Some(value);
            row[8] = Some(Value::NULL);
            row[9] = Some(Value::Bytes(b"invalid integer".to_vec()));
            row[12] = Some(Value::Bytes(vec![0xff]));
            assert!(
                decode_joined_rows([row], anchor.backing, 2)
                    .unwrap_err()
                    .is(ErrorCode::Estale),
                "authority column {index} did not fail first"
            );
        }
    }

    #[test]
    fn joined_selected_preserves_authority_before_inode_range_error() {
        let (anchor, _, row) = joined_fixture();
        assert!(
            decode_joined_rows([row.clone()], anchor.backing, u64::MAX)
                .unwrap_err()
                .is(ErrorCode::Eoverflow)
        );
        let mut stale_row = row;
        stale_row[1] = Some(Value::Bytes(b"MRC4".to_vec()));
        assert!(
            decode_joined_rows([stale_row], anchor.backing, u64::MAX)
                .unwrap_err()
                .is(ErrorCode::Estale)
        );
    }

    #[test]
    fn joined_selected_rejects_partial_null_and_unconvertible_cells() {
        let (anchor, _, row) = joined_fixture();
        for index in 8..13 {
            let mut partial = row.clone();
            partial[index] = Some(Value::NULL);
            assert!(
                decode_joined_rows([partial], anchor.backing, 2)
                    .unwrap_err()
                    .is(ErrorCode::Eio),
                "partial NULL in guard column {index} accepted"
            );
        }
        let mut invalid_number = row.clone();
        invalid_number[9] = Some(Value::Bytes(b"not an integer".to_vec()));
        let mut invalid_utf8 = row.clone();
        invalid_utf8[12] = Some(Value::Bytes(vec![0xff]));
        let mut incomplete = row.clone();
        incomplete[10] = None;
        let mut short = row.clone();
        short.pop();
        let mut extra = row;
        extra.push(Some(Value::NULL));
        for invalid in [invalid_number, invalid_utf8, incomplete, short, extra] {
            assert!(
                decode_joined_rows([invalid], anchor.backing, 2)
                    .unwrap_err()
                    .is(ErrorCode::Eio)
            );
        }
    }

    #[test]
    fn joined_selected_rejects_duplicate_rows_and_wrong_selected_inode() {
        let (anchor, _, row) = joined_fixture();
        assert!(
            decode_joined_rows([row.clone(), row.clone()], anchor.backing, 2)
                .unwrap_err()
                .is(ErrorCode::Eio)
        );
        let mut wrong = row;
        wrong[8] = Some(Value::Int(1));
        assert!(
            decode_joined_rows([wrong], anchor.backing, 2)
                .unwrap_err()
                .is(ErrorCode::Estale)
        );
    }

    #[test]
    fn joined_selected_rejects_negative_identity_and_malformed_body() {
        let (anchor, _, row) = joined_fixture();
        for index in 8..12 {
            let mut negative = row.clone();
            negative[index] = Some(Value::Int(-1));
            assert!(
                decode_joined_rows([negative], anchor.backing, 2)
                    .unwrap_err()
                    .is(ErrorCode::Eio),
                "negative guard column {index} accepted"
            );
        }
        let mut malformed = row;
        malformed[12] = Some(Value::Bytes(b"{".to_vec()));
        assert!(
            decode_joined_rows([malformed], anchor.backing, 2)
                .unwrap_err()
                .is(ErrorCode::Eio)
        );
    }

    #[test]
    fn joined_selected_keeps_membership_body_and_epoch_validation() {
        let (anchor, mut guard, row) = joined_fixture();
        let mut missing_membership = row.clone();
        let mut absent = anchor.clone();
        absent.members = vec![1];
        missing_membership[6] = Some(Value::Bytes(encode_compact_anchor(&absent).unwrap()));
        let mut future_epoch = row.clone();
        future_epoch[10] = Some(Value::Int(8));
        let mut wrong_body_inode = row.clone();
        guard.node.stats.ino = 1;
        wrong_body_inode[12] = Some(Value::Bytes(serde_json::to_vec(&guard.node).unwrap()));
        let mut generation_mismatch = row.clone();
        generation_mismatch[0] = Some(Value::Int(8));
        let mut malformed_anchor = row;
        malformed_anchor[6] = Some(Value::Bytes(b"{".to_vec()));
        for (invalid, expected_code) in [
            (missing_membership, ErrorCode::Einval),
            (future_epoch, ErrorCode::Einval),
            (wrong_body_inode, ErrorCode::Einval),
            (generation_mismatch, ErrorCode::Eio),
            (malformed_anchor, ErrorCode::Eio),
        ] {
            assert!(
                decode_joined_rows([invalid], anchor.backing, 2)
                    .unwrap_err()
                    .is(expected_code)
            );
        }
    }
    #[tokio::test]
    #[ignore = "requires actual owned TiDB and MOUNT_RS_TIDB_URL"]
    async fn actual_compact_schema_rejects_incompatible_scoped_tables() {
        let url = std::env::var("MOUNT_RS_TIDB_URL").unwrap();
        let pool = Pool::from_url(&url).unwrap();
        let mut conn = pool.get_conn().await.unwrap();
        let name = format!(
            "compact_schema_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let correct = SCHEMA.replace(TABLE, &name);
        let cases = [
            (
                "prefix primary",
                correct.replace(
                    "PRIMARY KEY(volume_key,inode)",
                    "PRIMARY KEY(volume_key(16),inode)",
                ),
            ),
            (
                "unsigned identity",
                correct.replace("incarnation BIGINT", "incarnation BIGINT UNSIGNED"),
            ),
            (
                "text revision",
                correct.replace("revision BIGINT", "revision VARCHAR(64)"),
            ),
            (
                "nullable epoch",
                correct.replace("epoch BIGINT NOT NULL", "epoch BIGINT NULL"),
            ),
            (
                "wrong primary",
                correct.replace(
                    "PRIMARY KEY(volume_key,inode)",
                    "PRIMARY KEY(volume_key,inode,incarnation)",
                ),
            ),
            (
                "short volume",
                correct.replace("VARBINARY(1020)", "VARBINARY(64)"),
            ),
        ];
        for (label, sql) in cases {
            conn.query_drop(sql).await.unwrap();
            let result = validate_schema(&mut conn, &name).await;
            conn.query_drop(format!("DROP TABLE {name}")).await.unwrap();
            assert!(
                result.is_err(),
                "incompatible scoped schema accepted: {label}"
            );
            eprintln!("compact TiDB scoped schema {label}: refused before enrollment");
        }
        conn.query_drop(correct).await.unwrap();
        assert!(validate_schema(&mut conn, &name).await.is_ok());
        conn.query_drop(format!("DROP TABLE {name}")).await.unwrap();
        drop(conn);
        pool.disconnect().await.unwrap();
    }
}
