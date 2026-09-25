//! Explicit MRC5 reference transactions. Existing MRC4 records are never reused.
use super::*;
#[cfg(unix)]
use mount_rs_core::storage::{NodeData, compact::*};

pub(super) fn initialize_schema(connection: &Connection) -> Result<()> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS mount_rs_compact_guards (
        inode TEXT PRIMARY KEY NOT NULL,
        incarnation INTEGER NOT NULL CHECK(incarnation>0),
        epoch INTEGER NOT NULL CHECK(epoch>=incarnation),
        revision INTEGER NOT NULL CHECK(revision>=0),
        node TEXT NOT NULL)",
        )
        .map_err(backend_error)?;
    validate_schema(connection)
}

/// The compact codec binds decimal inode keys as TEXT and physical identities
/// as INTEGER. SQLite affinity coercion must not change those representations.
/// Keep this stricter contract local to the new table; legacy schemas retain
/// their existing compatibility checks.
fn validate_schema(connection: &Connection) -> Result<()> {
    let mut statement = connection
        .prepare("PRAGMA table_xinfo(mount_rs_compact_guards)")
        .map_err(backend_error)?;
    let mut rows = statement.query([]).map_err(backend_error)?;
    let mut count = 0;
    while let Some(row) = rows.next().map_err(backend_error)? {
        let name: String = row.get(1).map_err(backend_error)?;
        let declared_type: String = row.get(2).map_err(backend_error)?;
        let not_null: i64 = row.get(3).map_err(backend_error)?;
        let primary_key: i64 = row.get(5).map_err(backend_error)?;
        let hidden: i64 = row.get(6).map_err(backend_error)?;
        let (expected_type, expected_key) = match name.as_str() {
            "inode" => ("TEXT", 1),
            "incarnation" | "epoch" | "revision" => ("INTEGER", 0),
            "node" => ("TEXT", 0),
            _ => return Err(incompatible_schema("unexpected compact guard column")),
        };
        if !declared_type.eq_ignore_ascii_case(expected_type)
            || not_null != 1
            || primary_key != expected_key
            || hidden != 0
        {
            return Err(incompatible_schema(
                "compact guards require ordinary NOT NULL TEXT/INTEGER columns and sole inode primary key",
            ));
        }
        count += 1;
    }
    if count != 5 {
        return Err(incompatible_schema("compact guard columns are missing"));
    }
    Ok(())
}

#[cfg(unix)]
fn anchor(
    database: &Database,
    connection: &Connection,
    backing: ConcurrentBackingId,
) -> Result<CompactAnchor> {
    let row: MetadataPublicationRow = connection.query_row(
        "SELECT write_mode,backing_id,owner,fence,expires,revision,physical_dev,physical_ino,physical_path FROM mount_rs_metadata INDEXED BY mount_rs_inode_authority WHERE id=1",[],
        |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?))).map_err(backend_error)?;
    let (mode, stored, owner, fence, expires, generation, dev, ino, path) = row;
    if mode.as_deref() != Some(COMPACT_WRITE_MODE)
        || stored.as_deref() != Some(backing.to_hex().as_str())
    {
        return Err(stale());
    }
    require_matching_metadata_stamp(database, dev.as_deref(), ino.as_deref(), path.as_deref())?;
    if owner.is_some() || fence != CONCURRENT_FENCE_SENTINEL || expires != 0 || generation <= 0 {
        return Err(incompatible_schema(
            "invalid compact authority fence or generation",
        ));
    }
    let json: String = connection
        .query_row(
            "SELECT namespace FROM mount_rs_metadata WHERE id=1",
            [],
            |r| r.get(0),
        )
        .map_err(backend_error)?;
    profile::add(Event::CompactAnchorReturned, json.len() as u64);
    let anchor = decode_compact_anchor(json.as_bytes())?;
    if anchor.generation != generation as u64 || anchor.backing != backing {
        return Err(incompatible_schema("compact anchor authority mismatch"));
    }
    Ok(anchor)
}

#[cfg(unix)]
fn decode_guard(row: &Row<'_>) -> Result<(u64, CompactGuard)> {
    let text: String = row.get(0).map_err(backend_error)?;
    let inode = text.parse::<u64>().map_err(backend_error)?;
    if inode.to_string() != text {
        return Err(incompatible_schema("noncanonical compact inode key"));
    }
    let identity = PhysicalInodeIdentity {
        incarnation: row.get(1).map_err(backend_error)?,
        epoch: row.get(2).map_err(backend_error)?,
        revision: row.get(3).map_err(backend_error)?,
    };
    let json: String = row.get(4).map_err(backend_error)?;
    Ok((
        inode,
        CompactGuard {
            identity,
            node: decode_inode_guard(inode, &json)?,
        },
    ))
}

#[cfg(unix)]
fn guards(connection: &Connection, selected: Option<u64>) -> Result<BTreeMap<u64, CompactGuard>> {
    let sql = if selected.is_some() {
        "SELECT inode,incarnation,epoch,revision,node FROM mount_rs_compact_guards WHERE inode=?1"
    } else {
        "SELECT inode,incarnation,epoch,revision,node FROM mount_rs_compact_guards"
    };
    let mut statement = connection.prepare(sql).map_err(backend_error)?;
    let mut rows = if let Some(inode) = selected {
        statement.query([inode.to_string()])
    } else {
        statement.query([])
    }
    .map_err(backend_error)?;
    let mut result = BTreeMap::new();
    while let Some(row) = rows.next().map_err(backend_error)? {
        let (inode, guard) = decode_guard(row)?;
        if result.insert(inode, guard).is_some() {
            return Err(incompatible_schema("duplicate compact guard identity"));
        }
    }
    Ok(result)
}

#[cfg(unix)]
fn sql_integer(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| FsError::new(ErrorCode::Eoverflow))
}

#[cfg(unix)]
fn write_guard(
    connection: &Connection,
    inode: u64,
    guard: &CompactGuard,
    created: bool,
) -> Result<()> {
    let identity = guard.identity;
    let incarnation = sql_integer(identity.incarnation)?;
    let epoch = sql_integer(identity.epoch)?;
    let revision = sql_integer(identity.revision)?;
    let json = serde_json::to_string(&guard.node).map_err(backend_error)?;
    profile::add(Event::InodeSerialized, json.len() as u64);
    let sql = if created {
        "INSERT INTO mount_rs_compact_guards(inode,incarnation,epoch,revision,node) VALUES(?1,?2,?3,?4,?5)"
    } else {
        "UPDATE mount_rs_compact_guards SET incarnation=?2,epoch=?3,revision=?4,node=?5 WHERE inode=?1"
    };
    if connection
        .execute(
            sql,
            params![inode.to_string(), incarnation, epoch, revision, json],
        )
        .map_err(backend_error)?
        != 1
    {
        return Err(stale());
    }
    Ok(())
}

#[cfg(unix)]
fn encode_anchor(anchor: &CompactAnchor) -> Result<String> {
    sql_integer(anchor.generation)?;
    let json = String::from_utf8(encode_compact_anchor(anchor)?).map_err(backend_error)?;
    profile::add(Event::CompactAnchorSerialized, json.len() as u64);
    Ok(json)
}

#[cfg(unix)]
impl SqliteMetadataStore {
    fn compact_transaction<T>(&self, action: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        if self.compact_inode_capability() != CompactInodeCapability::V1 {
            return Err(FsError::new(ErrorCode::Enotsup));
        }
        self.0.with_concurrent_publish_timeout(|connection| {
            let was_autocommit = connection.is_autocommit();
            let tx = match connection.transaction_with_behavior(TransactionBehavior::Immediate) {
                Ok(tx) => tx,
                Err(error) => {
                    return Err(sqlite_busy_known_noncommit(
                        &error,
                        was_autocommit,
                        "publish compact SQLite metadata",
                    )
                    .unwrap_or_else(|| backend_error(error)));
                }
            };
            let result = action(&tx)?;
            self.0.current_file_stamp()?;
            if let Err(error) = tx.commit() {
                return Err(sqlite_busy_known_noncommit(
                    &error,
                    connection.is_autocommit(),
                    "publish compact SQLite metadata",
                )
                .unwrap_or_else(|| backend_error(error)));
            }
            self.0.current_file_stamp()?;
            Ok(result)
        })
    }

    pub(super) fn compact_prepare(
        &self,
        backing: ConcurrentBackingId,
        expected_revision: u64,
    ) -> Result<()> {
        self.compact_transaction(|tx| {
            // Recheck inside the enrollment transaction: a handle may have been
            // opened before another connection replaced the empty guard table.
            validate_schema(tx)?;
            let (mode,stored,owner,fence,expires,revision,dev,ino,path):MetadataPublicationRow=tx.query_row(
                "SELECT write_mode,backing_id,owner,fence,expires,revision,physical_dev,physical_ino,physical_path FROM mount_rs_metadata WHERE id=1",[],
                |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?))).map_err(backend_error)?;
            if mode.as_deref()!=Some(BOUND_WRITE_MODE) || stored.as_deref()!=Some(backing.to_hex().as_str()) {return Err(stale());}
            require_matching_metadata_stamp(&self.0,dev.as_deref(),ino.as_deref(),path.as_deref())?;
            if owner.is_some() || fence!=CONCURRENT_FENCE_SENTINEL || expires!=0 {return Err(incompatible_schema("invalid compact enrollment fence"));}
            if revision<=0 || u64::try_from(revision).ok()!=Some(expected_revision) {return Err(inode_conflict());}
            let (json,delegation):(String,Option<String>)=tx.query_row("SELECT namespace,delegation_state FROM mount_rs_metadata WHERE id=1",[],|r|Ok((r.get(0)?,r.get(1)?))).map_err(backend_error)?;
            profile::add(Event::NamespaceReturned,json.len() as u64);
            let ns:Namespace=serde_json::from_str(&json).map_err(backend_error)?;ns.validate()?;
            let root=ns.nodes.get(&ns.root).ok_or_else(stale)?;
            if delegation.is_some() || ns.nodes.len()!=1 || ns.next_inode!=ns.root.checked_add(1).ok_or_else(||FsError::new(ErrorCode::Eoverflow))? || !matches!(&root.data,NodeData::Directory { entries } if entries.is_empty()) {
                return Err(FsError::new(ErrorCode::Ebusy).with_message("compact enrollment requires fresh root-only metadata"));
            }
            let history:i64=tx.query_row("SELECT (SELECT count(*) FROM mount_rs_inode_guards)+(SELECT count(*) FROM mount_rs_compact_guards)+(SELECT count(*) FROM mount_rs_versions)+(SELECT count(*) FROM mount_rs_version_pins)+(SELECT count(*) FROM mount_rs_version_state WHERE head_id IS NOT NULL)",[],|r|r.get(0)).map_err(backend_error)?;
            if history!=0 {return Err(FsError::new(ErrorCode::Ebusy));}
            let generation=checked_sqlite_next(expected_revision)? as u64;
            let anchor=CompactAnchor { backing,generation,root:ns.root,next_inode:ns.next_inode,default_uid:ns.default_uid,default_gid:ns.default_gid,umask:ns.umask,default_chunker:ns.default_chunker.clone(),members:vec![ns.root] };
            let json=encode_anchor(&anchor)?;
            write_guard(tx,ns.root,&CompactGuard { identity:PhysicalInodeIdentity { incarnation:generation,epoch:generation,revision:0 },node:root.clone() },true)?;
            if tx.execute("UPDATE mount_rs_metadata SET write_mode='MRC5',revision=?1,namespace=?2 WHERE id=1 AND write_mode='MRC2' AND revision=?3",params![generation,json,expected_revision]).map_err(backend_error)?!=1 {return Err(stale());}
            Ok(())
        })
    }

    pub(super) fn compact_snapshot(&self, backing: ConcurrentBackingId) -> Result<CompactSnapshot> {
        let mut connection = self.0.lock()?;
        let tx = connection.transaction().map_err(backend_error)?;
        let anchor = anchor(&self.0, &tx, backing)?;
        let snapshot = CompactSnapshot {
            anchor,
            guards: guards(&tx, None)?,
        };
        snapshot.namespace()?;
        self.0.current_file_stamp()?;
        Ok(snapshot)
    }

    pub(super) fn compact_load(
        &self,
        backing: ConcurrentBackingId,
        inode: u64,
    ) -> Result<LoadedCompactInode> {
        let mut connection = self.0.lock()?;
        let tx = connection.transaction().map_err(backend_error)?;
        let anchor = anchor(&self.0, &tx, backing)?;
        let guard = guards(&tx, Some(inode))?.remove(&inode).ok_or_else(stale)?;
        let loaded = LoadedCompactInode::from_guard(&anchor, inode, guard)?;
        self.0.current_file_stamp()?;
        Ok(loaded)
    }

    pub(super) fn compact_publish_inode(
        &self,
        backing: ConcurrentBackingId,
        inode: u64,
        generation: u64,
        expected: PhysicalInodeIdentity,
        node: NodeMetadata,
    ) -> Result<LoadedCompactInode> {
        self.compact_transaction(|tx| {
            let anchor = anchor(&self.0, tx, backing)?;
            let current = guards(tx, Some(inode))?.remove(&inode).ok_or_else(stale)?;
            let guard = validate_selected_update(
                &anchor, backing, generation, inode, &current, expected, node,
            )?;
            write_guard(tx, inode, &guard, false)?;
            Ok(LoadedCompactInode {
                generation: anchor.generation,
                guard,
            })
        })
    }

    pub(super) fn compact_publish_structure(
        &self,
        delta: &CompactStructuralDelta,
    ) -> Result<CompactPublication> {
        self.compact_transaction(|tx| {
            let anchor=anchor(&self.0,tx,delta.base_anchor().backing)?;
            let current=match delta.scope() {
                StructuralScope::Full=>guards(tx,None)?,
                StructuralScope::FileCreate=> {
                    let mut current=BTreeMap::new();
                    for &inode in delta.expected().keys() { current.extend(guards(tx,Some(inode))?); }
                    current
                }
            };
            let publication=delta.validate_current(&anchor,&current)?;
            let json=encode_anchor(&publication.anchor)?;
            for &inode in &publication.removed {
                if tx.execute("DELETE FROM mount_rs_compact_guards WHERE inode=?1",[inode.to_string()]).map_err(backend_error)?!=1 {return Err(stale());}
            }
            for (&inode,guard) in &publication.upserts {write_guard(tx,inode,guard,delta.created().contains_key(&inode))?;}
            if tx.execute("UPDATE mount_rs_metadata SET revision=?1,namespace=?2 WHERE id=1 AND write_mode='MRC5' AND revision=?3",params![publication.anchor.generation,json,anchor.generation]).map_err(backend_error)?!=1 {return Err(stale());}
            Ok(publication)
        })
    }
}
