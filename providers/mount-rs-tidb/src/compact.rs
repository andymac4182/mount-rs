//! Explicit MRC5 transactions; MRC4 authority and guards remain distinct.
use super::*;
use mount_rs_core::storage::{NodeData, compact::*};

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
    for (inode, incarnation, epoch, revision, json) in rows {
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
        if guards.insert(inode, guard).is_some() {
            return Err(backend_error("duplicate TiDB compact guard"));
        }
    }
    Ok(guards)
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
        let mut conn = self
            .0
            .pool
            .get_conn_observed()
            .await
            .map_err(|e| db_error("load TiDB compact inode", e))?;
        let mut tx = read_transaction(&mut conn).await?;
        let anchor = anchor(&mut tx, &self.0.volume_key, backing).await?;
        let guard = guards(&mut tx, &self.0.volume_key, Some(inode), false)
            .await?
            .remove(&inode)
            .ok_or_else(stale)?;
        let loaded = LoadedCompactInode::from_guard(&anchor, inode, guard)?;
        observe_result_future(StorageOperation::TidbRollback, tx.rollback(), 0)
            .await
            .map_err(|e| db_error("finish TiDB compact inode", e))?;
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
