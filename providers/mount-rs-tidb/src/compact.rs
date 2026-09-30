//! Indexed MRC5 metadata. Complete logical proofs reconstruct actual stored rows;
//! scoped file and root-entry reads transfer only point-selected records.
use super::*;
use mount_rs_core::storage::{NodeData, compact::*};
use mysql_async::{Row, Value, consts::StatusFlags, from_value_opt, prelude::FromValue};
#[path = "compact/indexed.rs"]
mod indexed;
use indexed::*;
#[path = "compact/borrowed.rs"]
mod borrowed;
#[path = "compact/member_equality.rs"]
mod member_equality;

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
const AUTHORITY_SQL: &str = "SELECT revision,write_mode,backing_id,owner,fence,expires,namespace,delegation FROM mount_rs_tidb_metadata WHERE volume_key=?";
// Explicit bindings let TiDB plan each composite key as a point access rather
// than a dynamic index join. Every binding is the same configured volume.
// This statement selects at most one row from each primary key. Keep its
// executor batches and worker count small without changing bulk-read sessions.
const FILE_POINT_SQL: &str = "SELECT /*+ SET_VAR(tidb_max_chunk_size=32) SET_VAR(tidb_executor_concurrency=1) */ m.revision,m.write_mode,m.backing_id,m.owner,m.fence,m.expires,m.namespace,m.delegation,s.inode,g.inode,g.incarnation,g.epoch,g.revision,g.node FROM mount_rs_tidb_metadata AS m LEFT JOIN mount_rs_tidb_compact_members AS s ON s.volume_key=? AND s.inode=? LEFT JOIN mount_rs_tidb_compact_guards AS g ON g.volume_key=? AND g.inode=? WHERE m.volume_key=?";
// The name index avoids scanning every sibling when TiDB estimates one row
// for the parent range. Full binary-name equality and all matching rows remain
// required, so a hash collision or duplicate cannot become an admitted hit.
const ROOT_ENTRY_POINT_SQL: &str = "SELECT m.revision,m.write_mode,m.backing_id,m.owner,m.fence,m.expires,m.namespace,m.delegation,rm.inode,r.inode,r.incarnation,r.epoch,r.revision,r.node,d.parent,d.ordinal,d.name,d.inode,fm.inode,f.inode,f.incarnation,f.epoch,f.revision,f.node FROM mount_rs_tidb_metadata AS m LEFT JOIN mount_rs_tidb_compact_members AS rm ON rm.volume_key=? AND rm.inode=? LEFT JOIN mount_rs_tidb_compact_guards AS r ON r.volume_key=? AND r.inode=? LEFT JOIN mount_rs_tidb_compact_dentries AS d FORCE INDEX(name_lookup) ON d.volume_key=? AND d.parent=? AND d.name_hash=? AND d.name=? LEFT JOIN mount_rs_tidb_compact_members AS fm ON fm.volume_key=? AND fm.inode=? LEFT JOIN mount_rs_tidb_compact_guards AS f ON f.volume_key=? AND f.inode=? WHERE m.volume_key=?";
const FILE_AUTHORITY_SQL: &str = "SELECT m.revision,m.write_mode,m.backing_id,m.owner,m.fence,m.expires,m.namespace,m.delegation,s.inode,@@SESSION.max_allowed_packet FROM mount_rs_tidb_metadata AS m LEFT JOIN mount_rs_tidb_compact_members AS s ON s.volume_key=? AND s.inode=? WHERE m.volume_key=?";
type GuardRow = (i64, i64, i64, i64, String);
type DentryRow = (i64, i64, Vec<u8>, Vec<u8>, i64);
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
    for schema in [SCHEMA, MEMBERS_SCHEMA, DENTRIES_SCHEMA] {
        conn.query_drop_observed(StorageOperation::TidbSqlDdl, schema)
            .await
            .map_err(|e| db_error("initialize indexed TiDB compact tables", e))?;
    }
    validate_schemas(conn).await
}
async fn validate_schemas<C: Queryable>(conn: &mut C) -> Result<()> {
    validate_schema(conn, TABLE).await?;
    validate_members_schema(conn, MEMBERS_TABLE).await?;
    validate_dentries_schema(conn, DENTRIES_TABLE).await
}
async fn validate_members_schema<C: Queryable>(conn: &mut C, table: &str) -> Result<()> {
    validate_table(
        conn,
        table,
        &[("volume_key", "varbinary"), ("inode", "bigint")],
        &[("PRIMARY", 0, 1, "volume_key"), ("PRIMARY", 0, 2, "inode")],
    )
    .await
}
async fn validate_dentries_schema<C: Queryable>(conn: &mut C, table: &str) -> Result<()> {
    validate_table(
        conn,
        table,
        &[
            ("volume_key", "varbinary"),
            ("parent", "bigint"),
            ("ordinal", "bigint"),
            ("name_hash", "binary"),
            ("name", "longblob"),
            ("inode", "bigint"),
        ],
        &[
            ("PRIMARY", 0, 1, "volume_key"),
            ("PRIMARY", 0, 2, "parent"),
            ("PRIMARY", 0, 3, "ordinal"),
            ("name_lookup", 1, 1, "volume_key"),
            ("name_lookup", 1, 2, "parent"),
            ("name_lookup", 1, 3, "name_hash"),
        ],
    )
    .await
}
async fn validate_schema<C: Queryable>(conn: &mut C, table: &str) -> Result<()> {
    validate_table(
        conn,
        table,
        &[
            ("volume_key", "varbinary"),
            ("inode", "bigint"),
            ("incarnation", "bigint"),
            ("epoch", "bigint"),
            ("revision", "bigint"),
            ("node", "longtext"),
        ],
        &[("PRIMARY", 0, 1, "volume_key"), ("PRIMARY", 0, 2, "inode")],
    )
    .await
}
async fn validate_table<C: Queryable>(
    conn: &mut C,
    table: &str,
    expected: &[(&str, &str)],
    expected_indexes: &[(&str, u64, u64, &str)],
) -> Result<()> {
    let columns: Vec<Column> = conn.exec_observed(StorageOperation::TidbSqlMetadataRead, "SELECT COLUMN_NAME,DATA_TYPE,COLUMN_TYPE,IS_NULLABLE,EXTRA FROM INFORMATION_SCHEMA.COLUMNS WHERE TABLE_SCHEMA=DATABASE() AND TABLE_NAME=? ORDER BY ORDINAL_POSITION", (table,)).await.map_err(|e| db_error("validate indexed TiDB compact columns", e))?;
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
                    || (*n == "volume_key" && full != "varbinary(1020)")
                    || (*n == "name_hash" && full != "binary(32)")
            })
    {
        return Err(backend_error("incompatible indexed TiDB compact columns"));
    }
    let indexes: Vec<(String, u64, u64, String, Option<u64>)> = conn.exec_observed(StorageOperation::TidbSqlMetadataRead, "SELECT INDEX_NAME,NON_UNIQUE,SEQ_IN_INDEX,COLUMN_NAME,SUB_PART FROM INFORMATION_SCHEMA.STATISTICS WHERE TABLE_SCHEMA=DATABASE() AND TABLE_NAME=? ORDER BY INDEX_NAME,SEQ_IN_INDEX", (table,)).await.map_err(|e| db_error("validate indexed TiDB compact indexes", e))?;
    // Compare sets because INFORMATION_SCHEMA's index-name collation can order
    // PRIMARY before or after name_lookup. Exact full-width keys are required.
    let actual: std::collections::BTreeSet<_> = indexes.into_iter().collect();
    let expected: std::collections::BTreeSet<_> = expected_indexes
        .iter()
        .map(|(name, unique, seq, column)| {
            (
                (*name).to_owned(),
                *unique,
                *seq,
                (*column).to_owned(),
                None,
            )
        })
        .collect();
    if actual != expected {
        return Err(backend_error("incompatible indexed TiDB compact indexes"));
    }
    Ok(())
}

fn decode_authority_row(row: AnchorRow, backing: ConcurrentBackingId) -> Result<CompactAuthority> {
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
    let authority = decode_authority(json.as_bytes())?;
    if nonnegative(row.0, "compact generation")? != authority.generation
        || authority.backing != backing
    {
        return Err(backend_error("indexed TiDB compact authority mismatch"));
    }
    Ok(authority)
}
async fn authority<C: Queryable>(
    conn: &mut C,
    volume: &str,
    backing: ConcurrentBackingId,
) -> Result<CompactAuthority> {
    let row = conn
        .exec_first_observed(
            StorageOperation::TidbSqlMetadataRead,
            AUTHORITY_SQL,
            (volume,),
        )
        .await
        .map_err(|e| db_error("read indexed TiDB compact authority", e))?
        .ok_or_else(stale)?;
    decode_authority_row(row, backing)
}
async fn members<C: Queryable>(conn: &mut C, volume: &str) -> Result<Vec<u64>> {
    let rows: Vec<i64> = conn
        .exec_observed(
            StorageOperation::TidbSqlInodeRead,
            "SELECT inode FROM mount_rs_tidb_compact_members WHERE volume_key=? ORDER BY inode",
            (volume,),
        )
        .await
        .map_err(|e| db_error("read indexed TiDB compact members", e))?;
    rows.into_iter()
        .map(|v| nonnegative(v, "compact member"))
        .collect()
}
async fn file_authority<C: Queryable>(
    conn: &mut C,
    volume: &str,
    backing: ConcurrentBackingId,
    inode: u64,
) -> Result<(CompactAuthority, Option<u64>, Value)> {
    let row: Row = conn
        .exec_first_observed(
            StorageOperation::TidbSqlMetadataRead,
            FILE_AUTHORITY_SQL,
            (volume, signed(inode, "compact member")?, volume),
        )
        .await
        .map_err(|e| db_error("read indexed TiDB compact file authority", e))?
        .ok_or_else(stale)?;
    let mut values = row.unwrap_raw();
    // Authority refusals precede selected membership conversion. The validated
    // primary keys give one authority and at most one member in this read view.
    let authority = joined_authority(&mut values, backing)?;
    if values.len() != 10 {
        return Err(backend_error("invalid indexed TiDB file authority shape"));
    }
    let selected_member = optional_member(&mut values, 8)?;
    // Retain the raw cap until the original packet-preflight phase. Authority,
    // membership and update-validation refusals keep their existing precedence.
    let session_packet = values[9].take().ok_or_else(stale)?;
    Ok((authority, selected_member, session_packet))
}
async fn anchor<C: Queryable>(
    conn: &mut C,
    volume: &str,
    backing: ConcurrentBackingId,
) -> Result<CompactAnchor> {
    authority(conn, volume, backing)
        .await?
        .into_anchor(members(conn, volume).await?)
}
struct StoredGuard {
    identity: PhysicalInodeIdentity,
    body: StoredBody,
}
fn decode_guard_row(row: GuardRow) -> Result<(u64, StoredGuard)> {
    let (inode, incarnation, epoch, revision, json) = row;
    let inode = nonnegative(inode, "compact inode")?;
    profile::add(Event::InodeReturned, json.len() as u64);
    let identity = PhysicalInodeIdentity {
        incarnation: nonnegative(incarnation, "compact incarnation")?,
        epoch: nonnegative(epoch, "compact epoch")?,
        revision: nonnegative(revision, "compact revision")?,
    };
    Ok((
        inode,
        StoredGuard {
            identity,
            body: decode_body(inode, &json)?,
        },
    ))
}
async fn stored_guards<C: Queryable>(
    conn: &mut C,
    volume: &str,
    selected: Option<u64>,
    locking: bool,
) -> Result<BTreeMap<u64, StoredGuard>> {
    let mut sql = "SELECT inode,incarnation,epoch,revision,node FROM mount_rs_tidb_compact_guards WHERE volume_key=?".to_owned();
    let params = if let Some(inode) = selected {
        sql.push_str(" AND inode=?");
        Params::from((volume, signed(inode, "compact inode")?))
    } else {
        Params::from((volume,))
    };
    sql.push_str(" ORDER BY inode");
    if locking {
        sql.push_str(" FOR UPDATE");
    }
    let rows: Vec<GuardRow> = conn
        .exec_observed(StorageOperation::TidbSqlInodeRead, sql, params)
        .await
        .map_err(|e| db_error("read indexed TiDB compact guards", e))?;
    let mut result = BTreeMap::new();
    for row in rows {
        let (inode, guard) = decode_guard_row(row)?;
        if result.insert(inode, guard).is_some() {
            return Err(backend_error("duplicate indexed TiDB compact guard"));
        }
    }
    Ok(result)
}
async fn locked_root_file_guards(
    tx: &mut Transaction<'_>,
    volume: &str,
    expected: &BTreeMap<u64, PhysicalInodeIdentity>,
) -> Result<BTreeMap<u64, StoredGuard>> {
    let ids: Vec<_> = expected.keys().copied().collect();
    let [root, file] = ids.as_slice() else {
        return Err(backend_error(
            "compact root-file publication requires two guards",
        ));
    };
    let rows: Vec<GuardRow> = tx.exec_observed(StorageOperation::TidbSqlInodeRead, "SELECT inode,incarnation,epoch,revision,node FROM mount_rs_tidb_compact_guards WHERE volume_key=? AND inode IN (?,?) ORDER BY inode FOR UPDATE", (volume, signed(*root, "compact root")?, signed(*file, "compact file")?)).await.map_err(|e| db_error("lock indexed TiDB root-file guards", e))?;
    let mut result = BTreeMap::new();
    for row in rows {
        let (inode, guard) = decode_guard_row(row)?;
        if result.insert(inode, guard).is_some() {
            return Err(backend_error("duplicate compact root-file guard"));
        }
    }
    if result.keys().copied().ne(ids) {
        return Err(stale());
    }
    Ok(result)
}
async fn entries<C: Queryable>(
    conn: &mut C,
    volume: &str,
    parent: Option<u64>,
    locking: bool,
) -> Result<BTreeMap<u64, Vec<StoredEntry>>> {
    let mut sql = "SELECT parent,ordinal,name_hash,name,inode FROM mount_rs_tidb_compact_dentries WHERE volume_key=?".to_owned();
    let params = if let Some(parent) = parent {
        sql.push_str(" AND parent=?");
        Params::from((volume, signed(parent, "compact directory parent")?))
    } else {
        Params::from((volume,))
    };
    sql.push_str(" ORDER BY parent,ordinal");
    if locking {
        sql.push_str(" FOR UPDATE");
    }
    let rows: Vec<DentryRow> = conn
        .exec_observed(StorageOperation::TidbSqlInodeRead, sql, params)
        .await
        .map_err(|e| db_error("read indexed TiDB directory entries", e))?;
    let mut result: BTreeMap<u64, Vec<StoredEntry>> = BTreeMap::new();
    for (parent, ordinal, hash, name, inode) in rows {
        let name = String::from_utf8(name).map_err(backend_error)?;
        validate_name(&name)?;
        if hash.as_slice() != name_hash(&name) {
            return Err(backend_error("indexed TiDB directory name digest mismatch"));
        }
        let parent = nonnegative(parent, "compact directory parent")?;
        let entry = StoredEntry {
            ordinal: nonnegative(ordinal, "compact directory ordinal")?,
            name,
            inode: nonnegative(inode, "compact directory child")?,
        };
        let group = result.entry(parent).or_default();
        if group
            .last()
            .is_some_and(|last| last.ordinal >= entry.ordinal)
        {
            return Err(backend_error("duplicate indexed TiDB directory ordinal"));
        }
        group.push(entry);
    }
    Ok(result)
}
fn materialize_guards(
    stored: BTreeMap<u64, StoredGuard>,
    mut entries: BTreeMap<u64, Vec<StoredEntry>>,
    complete: bool,
) -> Result<BTreeMap<u64, CompactGuard>> {
    if complete
        && entries.keys().any(|parent| {
            !matches!(
                stored.get(parent).map(|g| &g.body),
                Some(StoredBody::Directory(_))
            )
        })
    {
        return Err(backend_error(
            "indexed TiDB entries without directory guard",
        ));
    }
    stored
        .into_iter()
        .map(|(inode, guard)| {
            let node = match guard.body {
                StoredBody::Node(node) => {
                    if entries.get(&inode).is_some_and(|rows| !rows.is_empty()) {
                        return Err(backend_error("indexed TiDB entries under non-directory"));
                    }
                    node
                }
                StoredBody::Directory(header) => materialize_directory(
                    inode,
                    &header,
                    entries.remove(&inode).unwrap_or_default(),
                )?,
            };
            Ok((
                inode,
                CompactGuard {
                    identity: guard.identity,
                    node,
                },
            ))
        })
        .collect()
}
async fn selected_complete<C: Queryable>(
    conn: &mut C,
    volume: &str,
    inode: u64,
    locking: bool,
) -> Result<Option<CompactGuard>> {
    let stored = stored_guards(conn, volume, Some(inode), locking).await?;
    let rows = entries(conn, volume, Some(inode), locking).await?;
    Ok(materialize_guards(stored, rows, true)?.remove(&inode))
}
fn joined_field<T: FromValue>(values: &mut [Option<Value>], index: usize) -> Result<T> {
    let value = values
        .get_mut(index)
        .and_then(Option::take)
        .ok_or_else(|| backend_error("incomplete indexed TiDB joined row"))?;
    from_value_opt(value).map_err(|_| backend_error("invalid indexed TiDB joined field"))
}
fn joined_authority(
    values: &mut [Option<Value>],
    backing: ConcurrentBackingId,
) -> Result<CompactAuthority> {
    decode_authority_row(
        (
            joined_field(values, 0)?,
            joined_field(values, 1)?,
            joined_field(values, 2)?,
            joined_field(values, 3)?,
            joined_field(values, 4)?,
            joined_field(values, 5)?,
            joined_field(values, 6)?,
            joined_field(values, 7)?,
        ),
        backing,
    )
}
fn optional_member(values: &mut [Option<Value>], index: usize) -> Result<Option<u64>> {
    let value: Option<i64> = joined_field(values, index)?;
    value
        .map(|v| nonnegative(v, "compact point member"))
        .transpose()
}
fn optional_guard(
    values: &mut [Option<Value>],
    start: usize,
    requested: u64,
) -> Result<Option<StoredGuard>> {
    if values[start..start + 5]
        .iter()
        .all(|v| matches!(v, Some(Value::NULL)))
    {
        return Ok(None);
    }
    let (inode, guard) = decode_guard_row((
        joined_field(values, start)?,
        joined_field(values, start + 1)?,
        joined_field(values, start + 2)?,
        joined_field(values, start + 3)?,
        joined_field(values, start + 4)?,
    ))?;
    if inode != requested {
        return Err(stale());
    }
    Ok(Some(guard))
}
fn complete_file(guard: Option<StoredGuard>) -> Result<Option<CompactGuard>> {
    guard
        .map(|guard| match guard.body {
            StoredBody::Node(node) => Ok(CompactGuard {
                identity: guard.identity,
                node,
            }),
            StoredBody::Directory(_) => Err(backend_error("selected compact file is a directory")),
        })
        .transpose()
}
fn point_rows(rows: Vec<Row>, width: usize) -> Result<(Vec<Option<Value>>, bool)> {
    let mut rows = rows.into_iter();
    let row = rows.next().ok_or_else(stale)?;
    // Decode fresh authority before rejecting selected cardinality/shape. In
    // particular duplicate exact dentries must not hide a changed generation.
    if row.len() < 8 {
        return Err(backend_error("incomplete indexed TiDB point authority"));
    }
    let shape = rows.next().is_none() && row.len() == width;
    Ok((row.unwrap_raw(), shape))
}
fn decode_file_point(
    rows: Vec<Row>,
    backing: ConcurrentBackingId,
    inode: u64,
) -> Result<CompactFileRead> {
    let (mut values, shape) = point_rows(rows, 14)?;
    let authority = joined_authority(&mut values, backing)?;
    if !shape {
        return CompactFileRead::from_guard(
            authority,
            backing,
            inode,
            None,
            Err(backend_error(
                "invalid indexed TiDB file point cardinality or shape",
            )),
        );
    }
    let selected = optional_member(&mut values, 8);
    let guard = signed(inode, "compact inode")
        .and_then(|_| optional_guard(&mut values, 9, inode))
        .and_then(complete_file);
    // Preserve the fresh generation even when selected rows are malformed.
    match selected {
        Ok(member) => CompactFileRead::from_guard(authority, backing, inode, member, guard),
        Err(error) => CompactFileRead::from_guard(authority, backing, inode, None, Err(error)),
    }
}
fn check_file_point(
    rows: &[Row],
    backing: ConcurrentBackingId,
    inode: u64,
    expected: CompactFileExpectation<'_>,
) -> Option<CompactFileRead> {
    let [row] = rows else {
        return None;
    };
    if row.len() != 14 {
        return None;
    }
    let integer = |i| match row.as_ref(i)? {
        Value::Int(v) => Some(*v),
        _ => None,
    };
    let bytes = |i| match row.as_ref(i)? {
        Value::Bytes(v) => Some(v.as_slice()),
        _ => None,
    };
    if bytes(1)? != b"MRC5"
        || ConcurrentBackingId::from_hex(std::str::from_utf8(bytes(2)?).ok()?).ok()? != backing
        || !matches!(row.as_ref(3), Some(Value::NULL))
        || integer(4)? != CONCURRENT_FENCE_SENTINEL
        || integer(5)? != 0
        || !matches!(row.as_ref(7), Some(Value::NULL))
        || u64::try_from(integer(0)?).ok()? != expected.generation()
        || u64::try_from(integer(9)?).ok()? != inode
    {
        return None;
    }
    let member = u64::try_from(integer(8)?).ok()?;
    let identity = PhysicalInodeIdentity {
        incarnation: u64::try_from(integer(10)?).ok()?,
        epoch: u64::try_from(integer(11)?).ok()?,
        revision: u64::try_from(integer(12)?).ok()?,
    };
    let authority = bytes(6)?;
    let node = bytes(13)?;
    let read = check_compact_file_unchanged(
        authority_inner(authority)?,
        backing,
        inode,
        Some(member),
        identity,
        node,
        expected,
    )?;
    profile::add(Event::CompactAnchorReturned, authority.len() as u64);
    profile::add(Event::InodeReturned, node.len() as u64);
    Some(read)
}
fn decode_root_entry_point(
    rows: Vec<Row>,
    backing: ConcurrentBackingId,
    root: u64,
    file: u64,
    name: &str,
) -> Result<CompactRootEntryRead> {
    let (mut values, shape) = point_rows(rows, 24)?;
    let authority = joined_authority(&mut values, backing)?;
    let selected = (|| {
        if !shape {
            return Err(backend_error(
                "invalid indexed TiDB root-entry point cardinality or shape",
            ));
        }
        signed(root, "compact root")?;
        signed(file, "compact file")?;
        let root_member = optional_member(&mut values, 8)?;
        let root = optional_guard(&mut values, 9, root)?
            .map(|guard| match guard.body {
                StoredBody::Directory(header) => Ok((guard.identity, header)),
                StoredBody::Node(_) => {
                    Err(backend_error("compact point root is not indexed directory"))
                }
            })
            .transpose()?;
        let entry = if values[14..18]
            .iter()
            .all(|v| matches!(v, Some(Value::NULL)))
        {
            None
        } else {
            let name: Vec<u8> = joined_field(&mut values, 16)?;
            Some(CompactDirectoryEntry {
                parent: nonnegative(joined_field(&mut values, 14)?, "compact directory parent")?,
                ordinal: nonnegative(joined_field(&mut values, 15)?, "compact directory ordinal")?,
                name: String::from_utf8(name).map_err(backend_error)?,
                inode: nonnegative(joined_field(&mut values, 17)?, "compact directory child")?,
            })
        };
        let file_member = optional_member(&mut values, 18)?;
        let file = complete_file(optional_guard(&mut values, 19, file)?)?;
        Ok(CompactRootEntryRows {
            root_member,
            root,
            entry,
            file_member,
            file,
        })
    })();
    CompactRootEntryRead::from_rows(authority, backing, root, file, name, selected)
}
fn encode_anchor(anchor: &CompactAnchor) -> Result<Vec<u8>> {
    let bytes = encode_authority(anchor)?;
    profile::add(Event::CompactAnchorSerialized, bytes.len() as u64);
    Ok(bytes)
}
fn encode_guard(inode: u64, guard: &CompactGuard, next_ordinal: u64) -> Result<String> {
    signed(inode, "compact inode")?;
    signed(guard.identity.incarnation, "compact incarnation")?;
    signed(guard.identity.epoch, "compact epoch")?;
    signed(guard.identity.revision, "compact revision")?;
    let json = encode_body(&guard.node, next_ordinal)?;
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
        "write indexed TiDB compact guard",
    )
    .await?;
    if changed != 1 {
        return Err(stale());
    }
    Ok(())
}
fn selected_session_ready(flags: Option<StatusFlags>) -> bool {
    flags.is_some_and(|f| {
        f.contains(StatusFlags::SERVER_STATUS_AUTOCOMMIT)
            && !f.intersects(
                StatusFlags::SERVER_STATUS_IN_TRANS | StatusFlags::SERVER_STATUS_IN_TRANS_READONLY,
            )
    })
}
async fn point_connection(store: &TidbMetadataStore) -> Result<Conn> {
    let conn = store
        .0
        .pool
        .get_conn_observed()
        .await
        .map_err(|e| db_error("read indexed TiDB compact point", e))?;
    if !selected_session_ready(conn.last_ok_packet().map(|p| p.status_flags())) {
        let _ = conn.disconnect().await;
        return Err(backend_error(
            "TiDB compact point read requires verified autocommit outside a transaction",
        ));
    }
    Ok(conn)
}
async fn read_transaction(conn: &mut Conn) -> Result<Transaction<'_>> {
    let mut opts = TxOpts::default();
    opts.with_isolation_level(IsolationLevel::RepeatableRead);
    observe_result_future(
        StorageOperation::TidbBeginCompactRead,
        conn.start_transaction(opts),
        0,
    )
    .await
    .map_err(|e| db_error("begin indexed TiDB compact snapshot", e))
}
#[derive(serde::Deserialize)]
struct NamespaceFormatMarker {
    #[serde(default, deserialize_with = "present_namespace_layout")]
    layout: bool,
}

fn present_namespace_layout<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<bool, D::Error> {
    let _: serde::de::IgnoredAny = serde::Deserialize::deserialize(deserializer)?;
    Ok(true)
}

fn require_plain_namespace_format(body: &str) -> Result<()> {
    // Plain namespaces have no outer layout tag. Any tagged envelope,
    // including an unsupported or damaged version, must retain its mode fence.
    // Ignored fields are streamed rather than allocating the complete graph.
    let marker: NamespaceFormatMarker = serde_json::from_str(body).map_err(backend_error)?;
    if marker.layout { Err(stale()) } else { Ok(()) }
}

pub(super) async fn require_no_compact_markers<C: Queryable>(
    conn: &mut C,
    volume: &str,
    namespace: Option<&str>,
) -> Result<()> {
    if let Some(body) = namespace {
        require_plain_namespace_format(body)?;
    }
    let row: Option<(u8, u8, u8)> = conn.exec_first_observed(StorageOperation::TidbSqlInodeRead, "SELECT EXISTS(SELECT 1 FROM mount_rs_tidb_compact_guards WHERE volume_key=?),EXISTS(SELECT 1 FROM mount_rs_tidb_compact_members WHERE volume_key=?),EXISTS(SELECT 1 FROM mount_rs_tidb_compact_dentries WHERE volume_key=?)", (volume, volume, volume)).await.map_err(|e| db_error("inspect indexed TiDB compact markers", e))?;
    if row.is_none_or(|r| r != (0, 0, 0)) {
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
        let row: AnchorRow = tx
            .exec_first_observed(
                StorageOperation::TidbSqlMetadataRead,
                AUTHORITY_SQL,
                (&self.0.volume_key,),
            )
            .await
            .map_err(|e| db_error("inspect TiDB compact mode", e))?
            .ok_or_else(stale)?;
        let result = match (row.1.as_deref(), row.2.as_deref()) {
            (Some(b"MRC5"), Some(id)) => {
                let backing = backing_from_bytes(id)?;
                let authority = decode_authority_row(row, backing)?;
                Some(InodeModeState {
                    backing,
                    structural_generation: authority.generation,
                })
            }
            (Some(b"MRC4"), Some(_)) => {
                compact_inode_authority((row.0, row.1, row.2, row.3, row.4, row.5), None)?;
                if row.6.is_none() || row.7.is_some() {
                    return Err(stale());
                }
                require_no_compact_markers(&mut tx, &self.0.volume_key, row.6.as_deref()).await?;
                None
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
                None
            }
            (None, None) | (Some(b"MRC1"), None) if row.7.is_none() => {
                if (row.1.is_some()
                    && (row.3.is_some() || row.4 != CONCURRENT_FENCE_SENTINEL || row.5 != 0))
                    || (row.1.is_none() && row.4 == CONCURRENT_FENCE_SENTINEL)
                {
                    return Err(stale());
                }
                require_no_compact_markers(&mut tx, &self.0.volume_key, row.6.as_deref()).await?;
                None
            }
            _ => return Err(stale()),
        };
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
            .map_err(|e| db_error("enroll indexed TiDB compact", e))?;
        let mut tx = begin_inode_write(&mut conn).await?;
        validate_schemas(&mut tx).await?;
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
        let root_node = ns.nodes.get(&ns.root).ok_or_else(stale)?;
        if delegation.is_some()
            || ns.nodes.len() != 1
            || ns.next_inode
                != ns
                    .root
                    .checked_add(1)
                    .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?
            || !matches!(&root_node.data, NodeData::Directory { entries } if entries.is_empty())
        {
            return Err(FsError::new(ErrorCode::Ebusy));
        }
        require_no_compact_markers(&mut tx, &self.0.volume_key, Some(&json)).await?;
        let count: u64 = tx
            .exec_first_observed(
                StorageOperation::TidbSqlMetadataRead,
                "SELECT count(*) FROM mount_rs_tidb_inodes WHERE volume_key=?",
                (&self.0.volume_key,),
            )
            .await
            .map_err(|e| db_error("check TiDB compact enrollment history", e))?
            .ok_or_else(stale)?;
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
            node: root_node.clone(),
        };
        let body = encode_anchor(&anchor)?;
        let node = encode_guard(ns.root, &root, 0)?;
        let budget = packet_budget(&mut tx).await?;
        check_bytes(&self.0, body.len(), budget)?;
        check_bytes(&self.0, node.len(), budget)?;
        check_bytes(&self.0, 0, budget)?;
        tx.exec_drop_observed(
            StorageOperation::TidbSqlInodeWrite,
            "INSERT INTO mount_rs_tidb_compact_members(volume_key,inode) VALUES(?,?)",
            (&self.0.volume_key, signed(ns.root, "compact member")?),
        )
        .await
        .map_err(|e| db_error("enroll TiDB compact root member", e))?;
        write_guard(&mut tx, &self.0.volume_key, ns.root, &root, node, true).await?;
        tx.exec_drop_observed(StorageOperation::TidbSqlMetadataWrite, "UPDATE mount_rs_tidb_metadata SET write_mode='MRC5',revision=?,namespace=? WHERE volume_key=?", (signed(generation, "compact generation")?, body, &self.0.volume_key)).await.map_err(|e| db_error("enroll indexed TiDB compact authority", e))?;
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
        let stored = stored_guards(&mut tx, &self.0.volume_key, None, false).await?;
        let rows = entries(&mut tx, &self.0.volume_key, None, false).await?;
        let snapshot = CompactSnapshot {
            anchor,
            guards: materialize_guards(stored, rows, true)?,
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
            .map_err(|e| db_error("load complete TiDB compact inode", e))?;
        let mut tx = read_transaction(&mut conn).await?;
        let anchor = anchor(&mut tx, &self.0.volume_key, backing).await?;
        signed(inode, "compact inode")?;
        let guard = selected_complete(&mut tx, &self.0.volume_key, inode, false)
            .await?
            .ok_or_else(stale)?;
        let loaded = LoadedCompactInode::from_guard(&anchor, inode, guard)?;
        observe_result_future(StorageOperation::TidbRollback, tx.rollback(), 0)
            .await
            .map_err(|e| db_error("finish complete TiDB compact inode", e))?;
        Ok(loaded)
    }
    pub(super) async fn compact_read(
        &self,
        backing: ConcurrentBackingId,
        inode: u64,
        expected: CompactInodeExpectation<'_>,
    ) -> Result<CompactInodeRead> {
        let mut conn = self
            .0
            .pool
            .get_conn_observed()
            .await
            .map_err(|e| db_error("read complete TiDB compact inode", e))?;
        let mut tx = read_transaction(&mut conn).await?;
        if let Some(checked) =
            borrowed::try_root(&mut tx, &self.0.volume_key, backing, inode, expected).await?
        {
            observe_result_future(StorageOperation::TidbRollback, tx.rollback(), 0)
                .await
                .map_err(|e| db_error("finish complete TiDB compact inode", e))?;
            return Ok(CompactInodeRead::Unchanged(checked));
        }
        let anchor = anchor(&mut tx, &self.0.volume_key, backing).await?;
        signed(inode, "compact inode")?;
        let guard = selected_complete(&mut tx, &self.0.volume_key, inode, false)
            .await?
            .ok_or_else(stale)?;
        // The complete proof uses freshly reconstructed SQL rows from this
        // transaction. Typed comparison avoids encoding those rows again.
        let read =
            CompactInodeRead::from_materialized_guard(&anchor, backing, inode, guard, expected)?;
        observe_result_future(StorageOperation::TidbRollback, tx.rollback(), 0)
            .await
            .map_err(|e| db_error("finish complete TiDB compact inode", e))?;
        Ok(read)
    }
    pub(super) async fn compact_root_file(
        &self,
        backing: ConcurrentBackingId,
        expected_root: u64,
        candidate_file: u64,
    ) -> Result<CompactRootFileRead> {
        let mut conn = self
            .0
            .pool
            .get_conn_observed()
            .await
            .map_err(|e| db_error("read complete TiDB compact root-file", e))?;
        let mut tx = read_transaction(&mut conn).await?;
        let anchor = anchor(&mut tx, &self.0.volume_key, backing).await?;
        signed(expected_root, "compact root")?;
        signed(candidate_file, "compact file")?;
        let root = selected_complete(&mut tx, &self.0.volume_key, expected_root, false).await?;
        let file = selected_complete(&mut tx, &self.0.volume_key, candidate_file, false).await?;
        let read =
            CompactRootFileRead::from_guards(anchor, expected_root, candidate_file, root, file)?;
        observe_result_future(StorageOperation::TidbRollback, tx.rollback(), 0)
            .await
            .map_err(|e| db_error("finish complete TiDB root-file", e))?;
        Ok(read)
    }
    pub(super) async fn compact_file(
        &self,
        backing: ConcurrentBackingId,
        inode: u64,
        expected: CompactFileExpectation<'_>,
    ) -> Result<CompactFileRead> {
        let mut conn = point_connection(self).await?;
        let selected = signed(inode, "compact inode").unwrap_or(0);
        let rows: Vec<Row> = conn
            .exec_observed(
                StorageOperation::TidbSqlInodeRead,
                FILE_POINT_SQL,
                (
                    &self.0.volume_key,
                    selected,
                    &self.0.volume_key,
                    selected,
                    &self.0.volume_key,
                ),
            )
            .await
            .map_err(|e| db_error("read TiDB compact file point", e))?;
        if let Some(read) = check_file_point(&rows, backing, inode, expected) {
            return Ok(read);
        }
        decode_file_point(rows, backing, inode)
    }
    pub(super) async fn compact_root_entry(
        &self,
        backing: ConcurrentBackingId,
        expected_root: u64,
        candidate_file: u64,
        name: &str,
        _expected: CompactFileExpectation<'_>,
    ) -> Result<CompactRootEntryRead> {
        let mut conn = point_connection(self).await?;
        let root = signed(expected_root, "compact root").unwrap_or(0);
        let file = signed(candidate_file, "compact file").unwrap_or(0);
        let rows: Vec<Row> = conn
            .exec_observed(
                StorageOperation::TidbSqlInodeRead,
                ROOT_ENTRY_POINT_SQL,
                Params::Positional(vec![
                    Value::from(&self.0.volume_key),
                    Value::from(root),
                    Value::from(&self.0.volume_key),
                    Value::from(root),
                    Value::from(&self.0.volume_key),
                    Value::from(root),
                    Value::Bytes(name_hash(name).to_vec()),
                    Value::Bytes(name.as_bytes().to_vec()),
                    Value::from(&self.0.volume_key),
                    Value::from(file),
                    Value::from(&self.0.volume_key),
                    Value::from(file),
                    Value::from(&self.0.volume_key),
                ]),
            )
            .await
            .map_err(|e| db_error("read TiDB compact root-entry point", e))?;
        decode_root_entry_point(rows, backing, expected_root, candidate_file, name)
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
        // Guard first; authority and explicit membership are fresh after a wait.
        // No structural authority lock is taken by selected file publication.
        let stored = stored_guards(&mut tx, &self.0.volume_key, Some(inode), true)
            .await?
            .remove(&inode)
            .ok_or_else(stale)?;
        let (authority, selected_member, session_packet) =
            file_authority(&mut tx, &self.0.volume_key, backing, inode).await?;
        let (guard, next_ordinal) = match stored.body {
            StoredBody::Node(current_node) if matches!(current_node.data, NodeData::File(_)) => {
                let current = CompactGuard {
                    identity: stored.identity,
                    node: current_node,
                };
                (
                    validate_compact_file_update(
                        &authority,
                        backing,
                        generation,
                        inode,
                        selected_member,
                        &current,
                        expected,
                        node,
                    )?,
                    0,
                )
            }
            body => {
                let anchor = authority
                    .clone()
                    .into_anchor(members(&mut tx, &self.0.volume_key).await?)?;
                let next_ordinal = match &body {
                    StoredBody::Directory(h) => h.next_ordinal,
                    _ => 0,
                };
                let rows = entries(&mut tx, &self.0.volume_key, Some(inode), true).await?;
                let mut current = materialize_guards(
                    BTreeMap::from([(
                        inode,
                        StoredGuard {
                            identity: stored.identity,
                            body,
                        },
                    )]),
                    rows,
                    true,
                )?;
                let current = current.remove(&inode).ok_or_else(stale)?;
                (
                    validate_selected_update(
                        &anchor, backing, generation, inode, &current, expected, node,
                    )?,
                    next_ordinal,
                )
            }
        };
        let json = encode_guard(inode, &guard, next_ordinal)?;
        // This provider-owned session stays on the same connection throughout
        // the transaction. Preserve both the explicit client and server caps.
        let session: u64 = from_value_opt(session_packet)
            .map_err(|_| backend_error("invalid TiDB compact packet budget"))?;
        let budget = tx
            .opts()
            .max_allowed_packet()
            .unwrap_or(usize::MAX)
            .min(usize::try_from(session).unwrap_or(usize::MAX));
        check_bytes(&self.0, json.len(), budget)?;
        write_guard(&mut tx, &self.0.volume_key, inode, &guard, json, false).await?;
        commit(tx, "publish compact inode").await?;
        Ok(LoadedCompactInode {
            generation: authority.generation,
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
        locked_lease_row(&mut tx, &self.0.volume_key)
            .await?
            .ok_or_else(stale)?;
        let (anchor, read_budget) =
            member_equality::structural_anchor(&mut tx, &self.0.volume_key, delta).await?;
        let stored = match delta.scope() {
            StructuralScope::Full => stored_guards(&mut tx, &self.0.volume_key, None, true).await?,
            StructuralScope::FileCreate => {
                let mut guards = BTreeMap::new();
                for &inode in delta.expected().keys() {
                    guards.extend(
                        stored_guards(&mut tx, &self.0.volume_key, Some(inode), true).await?,
                    );
                }
                guards
            }
            StructuralScope::RootFileRenameAbsent | StructuralScope::RootFileUnlinkLastLink => {
                locked_root_file_guards(&mut tx, &self.0.volume_key, delta.expected()).await?
            }
        };
        let mut directory_rows = BTreeMap::new();
        // The volume authority lock serializes every cooperative membership and
        // dentry writer. Read Committed keeps these complete reads fresh after
        // acquiring it; locking untouched siblings would add lock-only commit
        // mutations. Retain the guard locks and complete structural validation.
        if delta.scope() == StructuralScope::Full {
            directory_rows = entries(&mut tx, &self.0.volume_key, None, false).await?;
        } else {
            for (&inode, guard) in &stored {
                if matches!(guard.body, StoredBody::Directory(_)) {
                    directory_rows
                        .extend(entries(&mut tx, &self.0.volume_key, Some(inode), false).await?);
                }
            }
        }
        // Retain only the physical positions for write planning. Materializing
        // below transfers SQL name buffers into the complete logical guards.
        let old_directories: BTreeMap<_, _> = stored
            .iter()
            .filter_map(|(&inode, guard)| match &guard.body {
                StoredBody::Directory(h) => Some((
                    inode,
                    (
                        h.next_ordinal,
                        directory_rows
                            .get(&inode)
                            .map(|rows| rows.iter().map(|entry| entry.ordinal).collect::<Vec<_>>())
                            .unwrap_or_default(),
                    ),
                )),
                _ => None,
            })
            .collect();
        let current = materialize_guards(
            stored,
            directory_rows,
            delta.scope() == StructuralScope::Full,
        )?;
        let publication = delta.validate_current(&anchor, &current)?;
        let body = encode_anchor(&publication.anchor)?;
        let budget = match read_budget {
            Some(budget) => budget,
            None => packet_budget(&mut tx).await?,
        };
        check_bytes(&self.0, body.len(), budget)?;
        check_bytes(&self.0, 0, budget)?;
        let mut encoded = BTreeMap::new();
        let mut plans = BTreeMap::new();
        for (&inode, guard) in &publication.upserts {
            let ordinal = if let NodeData::Directory { entries } = &guard.node.data {
                let (next_ordinal, old_positions) = old_directories
                    .get(&inode)
                    .map(|(next, positions)| (*next, positions.as_slice()))
                    .unwrap_or((0, &[]));
                let old = current
                    .get(&inode)
                    .and_then(|guard| match &guard.node.data {
                        NodeData::Directory { entries } => Some(entries.as_slice()),
                        _ => None,
                    })
                    .unwrap_or_default();
                let plan = DirectoryPlan::new_from_materialized(
                    old_positions,
                    old,
                    next_ordinal,
                    entries,
                )?;
                for entry in &plan.upserts {
                    signed(entry.ordinal, "compact directory ordinal")?;
                    signed(entry.inode, "compact directory child")?;
                    check_bytes(
                        &self.0,
                        entry
                            .name
                            .len()
                            .checked_add(32)
                            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?,
                        budget,
                    )?;
                }
                let ordinal = plan.next_ordinal;
                plans.insert(inode, plan);
                ordinal
            } else {
                0
            };
            let json = encode_guard(inode, guard, ordinal)?;
            check_bytes(&self.0, json.len(), budget)?;
            encoded.insert(inode, json);
        }
        for &inode in &publication.removed {
            signed(inode, "compact inode")?;
        }
        let added: Vec<_> = publication
            .anchor
            .members
            .iter()
            .copied()
            .filter(|inode| anchor.members.binary_search(inode).is_err())
            .collect();
        let removed: Vec<_> = anchor
            .members
            .iter()
            .copied()
            .filter(|inode| publication.anchor.members.binary_search(inode).is_err())
            .collect();
        // Every encoding, name, integer and packet is preflighted before DML.
        for &inode in &removed {
            if changed_query(
                StorageOperation::TidbSqlInodeWrite,
                &mut tx,
                "DELETE FROM mount_rs_tidb_compact_members WHERE volume_key=? AND inode=?",
                (&self.0.volume_key, signed(inode, "compact member")?),
                "remove TiDB compact member",
            )
            .await?
                != 1
            {
                return Err(stale());
            }
        }
        for &inode in &added {
            tx.exec_drop_observed(
                StorageOperation::TidbSqlInodeWrite,
                "INSERT INTO mount_rs_tidb_compact_members(volume_key,inode) VALUES(?,?)",
                (&self.0.volume_key, signed(inode, "compact member")?),
            )
            .await
            .map_err(|e| db_error("add TiDB compact member", e))?;
        }
        for &inode in &publication.removed {
            tx.exec_drop_observed(
                StorageOperation::TidbSqlInodeWrite,
                "DELETE FROM mount_rs_tidb_compact_dentries WHERE volume_key=? AND parent=?",
                (
                    &self.0.volume_key,
                    signed(inode, "compact directory parent")?,
                ),
            )
            .await
            .map_err(|e| db_error("remove TiDB compact directory rows", e))?;
            if changed_query(
                StorageOperation::TidbSqlInodeWrite,
                &mut tx,
                "DELETE FROM mount_rs_tidb_compact_guards WHERE volume_key=? AND inode=?",
                (&self.0.volume_key, signed(inode, "compact inode")?),
                "remove TiDB compact guard",
            )
            .await?
                != 1
            {
                return Err(stale());
            }
        }
        for (&inode, plan) in &plans {
            if plan.rewrite {
                tx.exec_drop_observed(
                    StorageOperation::TidbSqlInodeWrite,
                    "DELETE FROM mount_rs_tidb_compact_dentries WHERE volume_key=? AND parent=?",
                    (
                        &self.0.volume_key,
                        signed(inode, "compact directory parent")?,
                    ),
                )
                .await
                .map_err(|e| db_error("rewrite TiDB compact directory", e))?;
            } else {
                for &ordinal in &plan.removed {
                    if changed_query(StorageOperation::TidbSqlInodeWrite, &mut tx, "DELETE FROM mount_rs_tidb_compact_dentries WHERE volume_key=? AND parent=? AND ordinal=?", (&self.0.volume_key, signed(inode, "compact directory parent")?, signed(ordinal, "compact directory ordinal")?), "remove TiDB compact directory entry").await? != 1 { return Err(stale()); }
                }
            }
            for entry in &plan.upserts {
                tx.exec_drop_observed(StorageOperation::TidbSqlInodeWrite, "INSERT INTO mount_rs_tidb_compact_dentries(volume_key,parent,ordinal,name_hash,name,inode) VALUES(?,?,?,?,?,?) ON DUPLICATE KEY UPDATE name_hash=VALUES(name_hash),name=VALUES(name),inode=VALUES(inode)", (&self.0.volume_key, signed(inode, "compact directory parent")?, signed(entry.ordinal, "compact directory ordinal")?, name_hash(&entry.name).to_vec(), entry.name.as_bytes(), signed(entry.inode, "compact directory child")?)).await.map_err(|e| db_error("write TiDB compact directory entry", e))?;
            }
        }
        for (&inode, guard) in &publication.upserts {
            write_guard(
                &mut tx,
                &self.0.volume_key,
                inode,
                guard,
                encoded.remove(&inode).expect("preflighted compact guard"),
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
        .map_err(|e| db_error("publish indexed TiDB compact authority", e))?;
        commit(tx, "publish compact structure").await?;
        Ok(publication)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_namespace_layout_fences_unknown_and_damaged_envelopes() {
        assert!(require_plain_namespace_format(r#"{"root":1,"nodes":{}}"#).is_ok());
        for body in [
            r#"{"layout":"mount-rs-tidb-indexed-compact","version":99}"#,
            r#"{"layout":"mount-rs-compact-inodes","anchor":{}}"#,
            r#"{"layout":null}"#,
            r#"{"layout":"unknown-future-format"}"#,
            r#"{"layout":"mount-rs-tidb-indexed-compact","layout":null}"#,
            r#"{"layout":"mount-rs-tidb-indexed-compact""#,
        ] {
            assert!(
                require_plain_namespace_format(body).is_err(),
                "accepted {body}"
            );
        }
    }
    #[test]
    fn selected_session_requires_autocommit_and_no_active_transaction() {
        assert!(!selected_session_ready(None));
        for flags in [
            StatusFlags::empty(),
            StatusFlags::SERVER_STATUS_IN_TRANS,
            StatusFlags::SERVER_STATUS_IN_TRANS_READONLY,
            StatusFlags::SERVER_STATUS_AUTOCOMMIT | StatusFlags::SERVER_STATUS_IN_TRANS,
            StatusFlags::SERVER_STATUS_AUTOCOMMIT | StatusFlags::SERVER_STATUS_IN_TRANS_READONLY,
        ] {
            assert!(!selected_session_ready(Some(flags)));
        }
        assert!(selected_session_ready(Some(
            StatusFlags::SERVER_STATUS_AUTOCOMMIT
        )));
    }
    #[test]
    fn point_sql_has_exact_binary_name_and_selected_membership() {
        assert!(FILE_POINT_SQL.contains("s.inode=?"));
        assert!(ROOT_ENTRY_POINT_SQL.contains("d.name_hash=? AND d.name=?"));
        assert!(!FILE_POINT_SQL.contains("ORDER BY"));
        assert!(!ROOT_ENTRY_POINT_SQL.contains("ORDER BY"));
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
        for (label, sql) in [
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
        ] {
            conn.query_drop(sql).await.unwrap();
            let result = validate_schema(&mut conn, &name).await;
            conn.query_drop(format!("DROP TABLE {name}")).await.unwrap();
            assert!(result.is_err(), "accepted {label}");
        }
        conn.query_drop(correct).await.unwrap();
        assert!(validate_schema(&mut conn, &name).await.is_ok());
        conn.query_drop(format!("DROP TABLE {name}")).await.unwrap();
        let correct = MEMBERS_SCHEMA.replace(MEMBERS_TABLE, &name);
        for (label, sql) in [
            (
                "member primary prefix",
                correct.replace(
                    "PRIMARY KEY(volume_key,inode)",
                    "PRIMARY KEY(volume_key(16),inode)",
                ),
            ),
            (
                "unsigned member",
                correct.replace("inode BIGINT", "inode BIGINT UNSIGNED"),
            ),
            (
                "member wrong primary",
                correct.replace("PRIMARY KEY(volume_key,inode)", "PRIMARY KEY(volume_key)"),
            ),
            (
                "member short volume",
                correct.replace("VARBINARY(1020)", "VARBINARY(64)"),
            ),
            (
                "nullable member without its primary",
                correct
                    .replace("inode BIGINT NOT NULL", "inode BIGINT NULL")
                    .replace("PRIMARY KEY(volume_key,inode)", "PRIMARY KEY(volume_key)"),
            ),
        ] {
            conn.query_drop(sql).await.unwrap();
            let result = validate_members_schema(&mut conn, &name).await;
            conn.query_drop(format!("DROP TABLE {name}")).await.unwrap();
            assert!(result.is_err(), "accepted {label}");
        }
        conn.query_drop(correct).await.unwrap();
        assert!(validate_members_schema(&mut conn, &name).await.is_ok());
        conn.query_drop(format!("DROP TABLE {name}")).await.unwrap();
        let correct = DENTRIES_SCHEMA.replace(DENTRIES_TABLE, &name);
        for (label, sql) in [
            (
                "dentry primary prefix",
                correct.replace(
                    "PRIMARY KEY(volume_key,parent,ordinal)",
                    "PRIMARY KEY(volume_key(16),parent,ordinal)",
                ),
            ),
            (
                "dentry wrong primary",
                correct.replace(
                    "PRIMARY KEY(volume_key,parent,ordinal)",
                    "PRIMARY KEY(volume_key,parent)",
                ),
            ),
            (
                "unsigned ordinal",
                correct.replace("ordinal BIGINT", "ordinal BIGINT UNSIGNED"),
            ),
            (
                "nullable child",
                correct.replace("inode BIGINT NOT NULL", "inode BIGINT NULL"),
            ),
            ("short digest", correct.replace("BINARY(32)", "BINARY(16)")),
            (
                "variable digest",
                correct.replace("name_hash BINARY(32)", "name_hash VARBINARY(32)"),
            ),
            (
                "text name",
                correct.replace("name LONGBLOB", "name LONGTEXT"),
            ),
            (
                "unique digest lookup",
                correct.replace("KEY name_lookup", "UNIQUE KEY name_lookup"),
            ),
            (
                "prefix digest lookup",
                correct.replace(
                    "name_lookup(volume_key,parent,name_hash)",
                    "name_lookup(volume_key,parent,name_hash(16))",
                ),
            ),
            (
                "missing digest lookup",
                correct.replace(",\n    KEY name_lookup(volume_key,parent,name_hash)", ""),
            ),
        ] {
            conn.query_drop(sql).await.unwrap();
            let result = validate_dentries_schema(&mut conn, &name).await;
            conn.query_drop(format!("DROP TABLE {name}")).await.unwrap();
            assert!(result.is_err(), "accepted {label}");
        }
        conn.query_drop(correct).await.unwrap();
        assert!(validate_dentries_schema(&mut conn, &name).await.is_ok());
        conn.query_drop(format!("DROP TABLE {name}")).await.unwrap();
        drop(conn);
        pool.disconnect().await.unwrap();
    }
}
