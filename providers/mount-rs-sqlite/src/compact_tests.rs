//! Actual owned SQLite transactions; these are not core/runtime integration tests.
use super::*;
use futures_lite::future::block_on as run;
use mount_rs_core::{
    FsDriver, S_IFREG,
    chunking::{Chunker, FixedSizeChunker},
    storage::{BlockExtent, DirectoryEntry, FileLayout, NodeData, compact::*},
};
use mount_rs_memfs::MemoryFs;

struct Fixture {
    store: SqliteMetadataStore,
    path: std::path::PathBuf,
    backing: ConcurrentBackingId,
    _dir: tempfile::TempDir,
}
fn fresh_namespace() -> Namespace {
    let stats = run(MemoryFs::empty().stat("/")).unwrap();
    Namespace {
        format_version: 1,
        root: stats.ino,
        next_inode: stats.ino + 1,
        default_uid: 0,
        default_gid: 0,
        umask: 0o022,
        default_chunker: FixedSizeChunker::new(4096).unwrap().config(),
        nodes: BTreeMap::from([(
            stats.ino,
            NodeMetadata {
                stats,
                data: NodeData::Directory { entries: vec![] },
            },
        )]),
    }
}
fn fixture(enroll: bool) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("metadata.db");
    let store = SqliteMetadataStore::open(&path).unwrap();
    let backing = ConcurrentBackingId::from_bytes([0x62; 16]).unwrap();
    run(store.prepare_bound_concurrent_mode(backing)).unwrap();
    run(store.publish_bound_if_revision(backing, 0, fresh_namespace())).unwrap();
    if enroll {
        run(store.prepare_compact_inode_mode(backing, 1)).unwrap();
    }
    Fixture {
        store,
        path,
        backing,
        _dir: dir,
    }
}
fn add_file(ns: &mut Namespace, name: &str) -> u64 {
    let id = ns.next_inode;
    let mut stats = ns.nodes[&ns.root].stats.clone();
    stats.ino = id;
    stats.mode = S_IFREG | 0o644;
    stats.nlink = 1;
    stats.size = 0;
    stats.blocks = 0;
    ns.nodes.insert(
        id,
        NodeMetadata {
            stats,
            data: NodeData::File(FileLayout {
                chunker: ns.default_chunker.clone(),
                extents: vec![],
            }),
        },
    );
    let NodeData::Directory { entries } = &mut ns.nodes.get_mut(&ns.root).unwrap().data else {
        panic!()
    };
    entries.push(DirectoryEntry {
        name: name.into(),
        inode: id,
    });
    ns.next_inode += 1;
    id
}
fn create(f: &Fixture, name: &str) -> u64 {
    let snapshot = run(f.store.load_compact_snapshot(f.backing)).unwrap();
    let mut ns = snapshot.namespace().unwrap();
    let id = add_file(&mut ns, name);
    let delta =
        CompactStructuralDelta::capture(&snapshot, &ns, StructuralScope::FileCreate).unwrap();
    run(f.store.publish_compact_structure(&delta)).unwrap();
    id
}
type RawGuards = Vec<(String, u64, u64, u64, String)>;
fn raw(f: &Fixture) -> (u64, String, RawGuards) {
    let conn = f.store.0.lock().unwrap();
    let (generation, anchor) = conn
        .query_row(
            "SELECT revision,namespace FROM mount_rs_metadata WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    let rows=conn.prepare("SELECT inode,incarnation,epoch,revision,node FROM mount_rs_compact_guards ORDER BY inode").unwrap().query_map([],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).unwrap().collect::<std::result::Result<Vec<_>,_>>().unwrap();
    (generation, anchor, rows)
}
#[test]
fn compact_enrollment_fences_old_readers_and_reopens() {
    let f = fixture(false);
    assert_eq!(
        f.store.compact_inode_capability(),
        CompactInodeCapability::V1
    );
    run(f.store.prepare_compact_inode_mode(f.backing, 1)).unwrap();
    let snapshot = run(f.store.load_compact_snapshot(f.backing)).unwrap();
    assert_eq!(snapshot.anchor.generation, 2);
    assert_eq!(snapshot.namespace().unwrap().nodes.len(), 1);
    assert_eq!(
        snapshot.guards[&snapshot.anchor.root].identity,
        PhysicalInodeIdentity {
            incarnation: 2,
            epoch: 2,
            revision: 0
        }
    );
    let conn = f.store.0.lock().unwrap();
    let (generation,body):(u64,Option<String>)=conn.query_row("SELECT revision, CASE WHEN typeof(revision)='integer' AND revision>0 AND revision=?1 THEN NULL ELSE namespace END FROM mount_rs_metadata WHERE id=1",[1],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(generation, 2);
    let body = body.unwrap();
    assert!(serde_json::from_str::<Namespace>(&body).is_err());
    assert!(decode_inode_namespace(body.as_bytes()).is_err());
    assert_eq!(
        decode_compact_anchor(body.as_bytes()).unwrap(),
        snapshot.anchor
    );
    drop(conn);
    assert!(run(f.store.load_if_changed(1)).is_err());
    assert!(
        run(f
            .store
            .publish_bound_if_revision(f.backing, 1, fresh_namespace()))
        .is_err()
    );
    let reopened = SqliteMetadataStore::open(&f.path).unwrap();
    assert_eq!(
        run(reopened.load_compact_snapshot(f.backing)).unwrap(),
        snapshot
    );
}
#[test]
fn compact_enrollment_rejects_existing_or_foreign_authority() {
    for case in [
        "nonempty",
        "mrc4",
        "backing",
        "revision",
        "owner",
        "expiry",
        "stamp",
        "uninitialized",
    ] {
        let f = fixture(false);
        let mut backing = f.backing;
        let mut expected = 1;
        match case {
            "nonempty" => {
                let mut ns = fresh_namespace();
                add_file(&mut ns, "existing");
                run(f.store.publish_bound_if_revision(backing, 1, ns)).unwrap();
                expected = 2;
            }
            "mrc4" => run(f.store.prepare_inode_mode(backing, 1)).unwrap(),
            "backing" => backing = ConcurrentBackingId::from_bytes([0x33; 16]).unwrap(),
            "revision" => expected = 9,
            "owner" => {
                f.store
                    .0
                    .lock()
                    .unwrap()
                    .execute("UPDATE mount_rs_metadata SET owner='other'", [])
                    .unwrap();
            }
            "expiry" => {
                f.store
                    .0
                    .lock()
                    .unwrap()
                    .execute("UPDATE mount_rs_metadata SET expires=999", [])
                    .unwrap();
            }
            "stamp" => {
                f.store
                    .0
                    .lock()
                    .unwrap()
                    .execute("UPDATE mount_rs_metadata SET physical_ino='0'", [])
                    .unwrap();
            }
            "uninitialized" => {
                f.store
                    .0
                    .lock()
                    .unwrap()
                    .execute("UPDATE mount_rs_metadata SET namespace=NULL,revision=0", [])
                    .unwrap();
                expected = 0;
            }
            _ => unreachable!(),
        }
        let before: (Option<String>, u64) = f
            .store
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT write_mode,revision FROM mount_rs_metadata",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(
            run(f.store.prepare_compact_inode_mode(backing, expected)).is_err(),
            "{case}"
        );
        let after = f
            .store
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT write_mode,revision FROM mount_rs_metadata",
                [],
                |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, u64>(1)?)),
            )
            .unwrap();
        assert_eq!(before, after, "{case}");
    }
}
#[test]
fn compact_selected_create_preserves_physical_identity_and_real_bytes() {
    let f = fixture(true);
    let blocks = SqliteBlockStore::open(f._dir.path().join("blocks.db")).unwrap();
    let id = create(&f, "a");
    let selected = run(f.store.load_compact_inode(f.backing, id)).unwrap();
    let mut node = selected.guard.node.clone();
    let bytes = b"actual immutable bytes before independent create";
    let block = run(blocks.put(bytes)).unwrap();
    run(blocks.flush()).unwrap();
    node.stats.size = bytes.len() as u64;
    let NodeData::File(layout) = &mut node.data else {
        panic!()
    };
    layout.extents = vec![BlockExtent {
        file_offset: 0,
        block,
        block_offset: 0,
        length: bytes.len() as u64,
    }];
    let updated = run(f.store.publish_compact_inode(
        f.backing,
        id,
        selected.generation,
        selected.guard.identity,
        node,
    ))
    .unwrap();
    let before = raw(&f);
    create(&f, "b");
    let current = run(f.store.load_compact_inode(f.backing, id)).unwrap();
    assert_eq!(current.guard, updated.guard);
    assert!(current.generation > updated.generation);
    assert_eq!(
        current
            .guard
            .identity
            .logical_version(current.generation)
            .unwrap()
            .inode_revision,
        0
    );
    assert!(
        run(f.store.publish_compact_inode(
            f.backing,
            id,
            current.generation,
            selected.guard.identity,
            selected.guard.node
        ))
        .unwrap_err()
        .is(ErrorCode::Eagain)
    );
    let after = raw(&f);
    assert_eq!(
        before.2.iter().find(|r| r.0 == id.to_string()),
        after.2.iter().find(|r| r.0 == id.to_string())
    );
    let mut node = current.guard.node.clone();
    node.stats.mtime_ms += 1;
    let latest = run(f.store.publish_compact_inode(
        f.backing,
        id,
        current.generation,
        current.guard.identity,
        node,
    ))
    .unwrap();
    assert_eq!(latest.guard.identity.revision, 1);
    assert_eq!(raw(&f).1, after.1, "selected write preserves anchor bytes");
    let reopened = SqliteMetadataStore::open(&f.path).unwrap();
    let snapshot = run(reopened.load_compact_snapshot(f.backing)).unwrap();
    let NodeData::File(layout) = &snapshot.guards[&id].node.data else {
        panic!()
    };
    let fresh_blocks = SqliteBlockStore::open(f._dir.path().join("blocks.db")).unwrap();
    assert_eq!(
        run(fresh_blocks.get(&layout.extents[0].block)).unwrap(),
        bytes
    );
}
#[test]
fn compact_structural_conflicts_and_full_path_do_not_overwrite() {
    let f = fixture(true);
    let id = create(&f, "a");
    let base = run(f.store.load_compact_snapshot(f.backing)).unwrap();
    let mut candidate = base.namespace().unwrap();
    add_file(&mut candidate, "b");
    let full = CompactStructuralDelta::capture(&base, &candidate, StructuralScope::Full).unwrap();
    let targeted =
        CompactStructuralDelta::capture(&base, &candidate, StructuralScope::FileCreate).unwrap();
    let selected = run(f.store.load_compact_inode(f.backing, id)).unwrap();
    let mut node = selected.guard.node;
    node.stats.mtime_ms += 1;
    let updated = run(f.store.publish_compact_inode(
        f.backing,
        id,
        selected.generation,
        selected.guard.identity,
        node,
    ))
    .unwrap();
    assert!(
        run(f.store.publish_compact_structure(&full))
            .unwrap_err()
            .is(ErrorCode::Eagain)
    );
    run(f.store.publish_compact_structure(&targeted)).unwrap();
    assert_eq!(
        run(f.store.load_compact_inode(f.backing, id))
            .unwrap()
            .guard,
        updated.guard
    );
    assert!(
        run(f.store.publish_compact_structure(&targeted))
            .unwrap_err()
            .is(ErrorCode::Eagain)
    );
    let base = run(f.store.load_compact_snapshot(f.backing)).unwrap();
    let mut ns = base.namespace().unwrap();
    ns.default_gid = 123;
    let delta = CompactStructuralDelta::capture(&base, &ns, StructuralScope::Full).unwrap();
    let before = raw(&f);
    run(f.store.publish_compact_structure(&delta)).unwrap();
    assert_eq!(
        before.2,
        raw(&f).2,
        "unchanged guards retain physical bytes in full publication"
    );
    assert_eq!(
        serde_json::to_value(
            run(f.store.load_compact_snapshot(f.backing))
                .unwrap()
                .namespace()
                .unwrap()
        )
        .unwrap(),
        serde_json::to_value(ns).unwrap()
    );
}
#[test]
fn compact_corrupt_membership_parent_and_physical_tokens_fail_closed() {
    for case in ["phantom", "parent", "epoch", "incarnation", "links"] {
        let f = fixture(true);
        let id = create(&f, "a");
        let base = run(f.store.load_compact_snapshot(f.backing)).unwrap();
        let mut ns = base.namespace().unwrap();
        add_file(&mut ns, "b");
        let full = CompactStructuralDelta::capture(&base, &ns, StructuralScope::Full).unwrap();
        let target =
            CompactStructuralDelta::capture(&base, &ns, StructuralScope::FileCreate).unwrap();
        let conn = f.store.0.lock().unwrap();
        match case {
            "phantom" => {
                conn.execute(
                    "UPDATE mount_rs_compact_guards SET inode='999' WHERE inode=?1",
                    [id.to_string()],
                )
                .unwrap();
            }
            "parent" => {
                let mut node = base.guards[&id].node.clone();
                node.stats.ino = base.anchor.root;
                conn.execute(
                    "UPDATE mount_rs_compact_guards SET node=?1 WHERE inode=?2",
                    params![
                        serde_json::to_string(&node).unwrap(),
                        base.anchor.root.to_string()
                    ],
                )
                .unwrap();
            }
            "epoch" => {
                conn.execute(
                    "UPDATE mount_rs_compact_guards SET epoch=999 WHERE inode=?1",
                    [id.to_string()],
                )
                .unwrap();
            }
            "incarnation" => {
                conn.execute_batch("PRAGMA ignore_check_constraints=ON")
                    .unwrap();
                conn.execute(
                    "UPDATE mount_rs_compact_guards SET incarnation=999 WHERE inode=?1",
                    [id.to_string()],
                )
                .unwrap();
            }
            "links" => {
                let mut node = base.guards[&id].node.clone();
                node.stats.nlink = 9;
                conn.execute(
                    "UPDATE mount_rs_compact_guards SET node=?1 WHERE inode=?2",
                    params![serde_json::to_string(&node).unwrap(), id.to_string()],
                )
                .unwrap();
            }
            _ => unreachable!(),
        };
        drop(conn);
        let before = raw(&f);
        assert!(
            run(f.store.load_compact_snapshot(f.backing)).is_err(),
            "{case}"
        );
        assert!(
            run(f.store.publish_compact_structure(&full)).is_err(),
            "{case}"
        );
        if case == "parent" {
            assert!(run(f.store.publish_compact_structure(&target)).is_err());
        }
        if ["epoch", "incarnation"].contains(&case) {
            assert!(run(f.store.load_compact_inode(f.backing, id)).is_err());
        }
        assert_eq!(raw(&f), before);
    }
}
#[test]
fn compact_rollback_and_sql_integer_overflow_are_atomic() {
    let f = fixture(true);
    let id = create(&f, "a");
    let base = run(f.store.load_compact_snapshot(f.backing)).unwrap();
    let mut ns = base.namespace().unwrap();
    add_file(&mut ns, "b");
    let delta = CompactStructuralDelta::capture(&base, &ns, StructuralScope::FileCreate).unwrap();
    f.store.0.lock().unwrap().execute_batch("CREATE TRIGGER reject_compact_insert BEFORE INSERT ON mount_rs_compact_guards BEGIN SELECT RAISE(ABORT,'known rollback'); END").unwrap();
    let before = raw(&f);
    assert!(run(f.store.publish_compact_structure(&delta)).is_err());
    assert_eq!(raw(&f), before);
    f.store
        .0
        .lock()
        .unwrap()
        .execute_batch("DROP TRIGGER reject_compact_insert")
        .unwrap();
    f.store
        .0
        .lock()
        .unwrap()
        .execute(
            "UPDATE mount_rs_compact_guards SET epoch=?1,revision=?2 WHERE inode=?3",
            params![base.anchor.generation, i64::MAX, id.to_string()],
        )
        .unwrap();
    let loaded = run(f.store.load_compact_inode(f.backing, id)).unwrap();
    let before = raw(&f);
    assert!(
        run(f.store.publish_compact_inode(
            f.backing,
            id,
            loaded.generation,
            loaded.guard.identity,
            loaded.guard.node
        ))
        .unwrap_err()
        .is(ErrorCode::Eoverflow)
    );
    assert_eq!(raw(&f), before);
    let mut anchor = base.anchor.clone();
    anchor.generation = i64::MAX as u64;
    f.store
        .0
        .lock()
        .unwrap()
        .execute(
            "UPDATE mount_rs_metadata SET revision=?1,namespace=?2",
            params![
                i64::MAX,
                String::from_utf8(encode_compact_anchor(&anchor).unwrap()).unwrap()
            ],
        )
        .unwrap();
    let snapshot = run(f.store.load_compact_snapshot(f.backing)).unwrap();
    let delta = CompactStructuralDelta::capture(
        &snapshot,
        &snapshot.namespace().unwrap(),
        StructuralScope::Full,
    )
    .unwrap();
    let before = raw(&f);
    assert!(
        run(f.store.publish_compact_structure(&delta))
            .unwrap_err()
            .is(ErrorCode::Eoverflow)
    );
    assert_eq!(raw(&f), before);
}

#[derive(Default)]
struct TraceState {
    rows: Mutex<BTreeMap<String, u64>>,
    before_guards: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    callback_failed: std::sync::atomic::AtomicBool,
    action_event: u32,
    action_sql: &'static str,
}
unsafe extern "C" fn trace_rows(
    mask: u32,
    context: *mut std::ffi::c_void,
    statement: *mut std::ffi::c_void,
    _: *mut std::ffi::c_void,
) -> i32 {
    let state = unsafe { &*context.cast::<TraceState>() };
    let sql = unsafe { rusqlite::ffi::sqlite3_sql(statement.cast()) };
    if sql.is_null() {
        return 0;
    }
    let sql = unsafe { std::ffi::CStr::from_ptr(sql) }.to_string_lossy();
    if mask == rusqlite::ffi::SQLITE_TRACE_ROW {
        *state
            .rows
            .lock()
            .unwrap()
            .entry(sql.into_owned())
            .or_default() += 1;
    } else if mask == state.action_event && sql.starts_with(state.action_sql) {
        let action = state.before_guards.lock().unwrap().take();
        // Do not let Rust unwinding cross SQLite's C callback boundary.
        if let Some(action) = action
            && std::panic::catch_unwind(std::panic::AssertUnwindSafe(action)).is_err()
        {
            state
                .callback_failed
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
    0
}
struct Trace {
    database: Database,
    state: Box<TraceState>,
}
impl Trace {
    fn new(store: &SqliteMetadataStore, action: Option<Box<dyn FnOnce() + Send>>) -> Self {
        Self::with_action(
            store,
            action,
            rusqlite::ffi::SQLITE_TRACE_STMT,
            "SELECT inode,incarnation",
        )
    }
    fn with_action(
        store: &SqliteMetadataStore,
        action: Option<Box<dyn FnOnce() + Send>>,
        action_event: u32,
        action_sql: &'static str,
    ) -> Self {
        let result = Self {
            database: store.0.clone(),
            state: Box::new(TraceState {
                before_guards: Mutex::new(action),
                action_event,
                action_sql,
                ..Default::default()
            }),
        };
        let conn = result.database.lock().unwrap();
        // Box allocation remains fixed until Drop unregisters under the mutex.
        assert_eq!(
            unsafe {
                rusqlite::ffi::sqlite3_trace_v2(
                    conn.handle(),
                    rusqlite::ffi::SQLITE_TRACE_ROW
                        | rusqlite::ffi::SQLITE_TRACE_STMT
                        | rusqlite::ffi::SQLITE_TRACE_PROFILE,
                    Some(trace_rows),
                    (&*result.state as *const TraceState).cast_mut().cast(),
                )
            },
            rusqlite::ffi::SQLITE_OK
        );
        drop(conn);
        result
    }
    fn guard_rows(&self) -> u64 {
        self.state
            .rows
            .lock()
            .unwrap()
            .iter()
            .filter(|(sql, _)| sql.starts_with("SELECT inode,incarnation"))
            .map(|(_, count)| *count)
            .sum()
    }
}
impl Drop for Trace {
    fn drop(&mut self) {
        let conn = self.database.lock().unwrap();
        unsafe {
            rusqlite::ffi::sqlite3_trace_v2(conn.handle(), 0, None, std::ptr::null_mut());
        }
    }
}

fn write_bytes(
    store: &SqliteMetadataStore,
    blocks: &SqliteBlockStore,
    backing: ConcurrentBackingId,
    id: u64,
    bytes: &[u8],
) -> LoadedCompactInode {
    let current = run(store.load_compact_inode(backing, id)).unwrap();
    let block = run(blocks.put(bytes)).unwrap();
    run(blocks.flush()).unwrap();
    let mut node = current.guard.node;
    node.stats.size = bytes.len() as u64;
    let NodeData::File(layout) = &mut node.data else {
        panic!()
    };
    layout.extents = vec![BlockExtent {
        file_offset: 0,
        block,
        block_offset: 0,
        length: bytes.len() as u64,
    }];
    run(store.publish_compact_inode(
        backing,
        id,
        current.generation,
        current.guard.identity,
        node,
    ))
    .unwrap()
}

#[test]
fn compact_snapshot_is_one_real_sqlite_view_across_selected_commits() {
    let f = fixture(true);
    f.store
        .0
        .lock()
        .unwrap()
        .execute_batch("PRAGMA journal_mode=WAL")
        .unwrap();
    let a = create(&f, "a");
    let b = create(&f, "b");
    let before = run(f.store.load_compact_snapshot(f.backing)).unwrap();
    let writer = SqliteMetadataStore::open(&f.path).unwrap();
    let backing = f.backing;
    let trace = Trace::new(
        &f.store,
        Some(Box::new(move || {
            for id in [a, b] {
                let current = run(writer.load_compact_inode(backing, id)).unwrap();
                let mut node = current.guard.node;
                node.stats.mtime_ms += 10;
                run(writer.publish_compact_inode(
                    backing,
                    id,
                    current.generation,
                    current.guard.identity,
                    node,
                ))
                .unwrap();
            }
        })),
    );
    // Callback commits both writes after the anchor read established the read
    // transaction snapshot, immediately before its guard query executes.
    let during = run(f.store.load_compact_snapshot(f.backing)).unwrap();
    assert!(trace.state.before_guards.lock().unwrap().is_none());
    assert!(
        !trace
            .state
            .callback_failed
            .load(std::sync::atomic::Ordering::Relaxed)
    );
    assert_eq!(during, before);
    drop(trace);
    let after = run(f.store.load_compact_snapshot(f.backing)).unwrap();
    assert_eq!(
        after.anchor, before.anchor,
        "selected commits leave generation unchanged"
    );
    for id in [a, b] {
        assert_ne!(after.guards[&id], before.guards[&id]);
    }
}

#[test]
fn compact_targeted_128_control_counts_actual_rows_and_checks_all_bytes() {
    let f = fixture(true);
    let block_path = f._dir.path().join("blocks.db");
    let blocks = SqliteBlockStore::open(&block_path).unwrap();
    let mut expected = BTreeMap::new();
    for number in 0..128 {
        let id = create(&f, &format!("file-{number}"));
        let bytes = format!(
            "complete immutable contents for file {number}: {}",
            "abc123".repeat(30)
        )
        .into_bytes();
        write_bytes(&f.store, &blocks, f.backing, id, &bytes);
        expected.insert(id, bytes);
    }
    let before = run(f.store.load_compact_snapshot(f.backing)).unwrap();
    let mut ns = before.namespace().unwrap();
    let new = add_file(&mut ns, "new");
    let delta = CompactStructuralDelta::capture(&before, &ns, StructuralScope::FileCreate).unwrap();
    f.store.0.lock().unwrap().execute_batch("CREATE TEMP TABLE compact_writes(kind TEXT,inode TEXT);
        CREATE TEMP TRIGGER count_compact_insert AFTER INSERT ON mount_rs_compact_guards BEGIN INSERT INTO compact_writes VALUES('insert',NEW.inode); END;
        CREATE TEMP TRIGGER count_compact_update AFTER UPDATE ON mount_rs_compact_guards BEGIN INSERT INTO compact_writes VALUES('update',NEW.inode); END;
        CREATE TEMP TRIGGER count_compact_delete AFTER DELETE ON mount_rs_compact_guards BEGIN INSERT INTO compact_writes VALUES('delete',OLD.inode); END;
        CREATE TEMP TRIGGER count_compact_anchor AFTER UPDATE ON mount_rs_metadata BEGIN INSERT INTO compact_writes VALUES('anchor',NULL); END;").unwrap();
    let trace = Trace::new(&f.store, None);
    let receipt = run(f.store.publish_compact_structure(&delta)).unwrap();
    assert_eq!(
        trace.guard_rows(),
        1,
        "targeted create fetches only affected parent"
    );
    assert_eq!(receipt.upserts.len(), 2);
    drop(trace);
    let writes: Vec<(String, Option<String>)> = f
        .store
        .0
        .lock()
        .unwrap()
        .prepare("SELECT kind,inode FROM compact_writes ORDER BY kind")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert_eq!(
        writes,
        vec![
            ("anchor".into(), None),
            ("insert".into(), Some(new.to_string())),
            ("update".into(), Some(before.anchor.root.to_string()))
        ]
    );
    let trace = Trace::new(&f.store, None);
    let loaded = run(f.store.load_compact_inode(f.backing, new)).unwrap();
    assert_eq!(trace.guard_rows(), 1);
    drop(trace);
    f.store
        .0
        .lock()
        .unwrap()
        .execute("DELETE FROM compact_writes", [])
        .unwrap();
    let trace = Trace::new(&f.store, None);
    let mut changed = loaded.guard.node;
    changed.stats.mtime_ms += 1;
    run(f.store.publish_compact_inode(
        f.backing,
        new,
        loaded.generation,
        loaded.guard.identity,
        changed,
    ))
    .unwrap();
    assert_eq!(trace.guard_rows(), 1);
    drop(trace);
    let selected_writes: Vec<(String, Option<String>)> = f
        .store
        .0
        .lock()
        .unwrap()
        .prepare("SELECT kind,inode FROM compact_writes")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert_eq!(
        selected_writes,
        vec![("update".into(), Some(new.to_string()))]
    );
    let trace = Trace::new(&f.store, None);
    let after = run(f.store.load_compact_snapshot(f.backing)).unwrap();
    assert_eq!(trace.guard_rows(), 130);
    drop(trace);
    for &id in expected.keys() {
        assert_eq!(before.guards[&id], after.guards[&id]);
    }
    let backing = f.backing;
    let path = f.path.clone();
    drop(blocks);
    drop(f.store);
    let fresh = SqliteMetadataStore::open(path).unwrap();
    let blocks = SqliteBlockStore::open(block_path).unwrap();
    let snapshot = run(fresh.load_compact_snapshot(backing)).unwrap();
    for (id, bytes) in expected {
        let node = &snapshot.guards[&id].node;
        let NodeData::File(layout) = &node.data else {
            panic!()
        };
        let mut actual = vec![0; node.stats.size as usize];
        for extent in &layout.extents {
            let block = run(blocks.get(&extent.block)).unwrap();
            actual[extent.file_offset as usize..(extent.file_offset + extent.length) as usize]
                .copy_from_slice(
                    &block[extent.block_offset as usize
                        ..(extent.block_offset + extent.length) as usize],
                );
        }
        assert_eq!(actual, bytes);
    }
    eprintln!(
        "compact128 provider control: create guard rows fetched=1, parent updates=1, guard inserts=1, deletes=0, anchor updates=1; selected load guard rows=1; selected publication guard rows=1, updates=1, anchor updates=0; full snapshot guard rows=130; all128 full-byte fresh-store oracles passed"
    );
}

#[test]
fn compact_selected_concurrent_cas_has_one_winner_and_parent_conflicts() {
    let f = fixture(true);
    let id = create(&f, "a");
    let base = run(f.store.load_compact_snapshot(f.backing)).unwrap();
    let mut ns = base.namespace().unwrap();
    add_file(&mut ns, "b");
    let delta = CompactStructuralDelta::capture(&base, &ns, StructuralScope::FileCreate).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let workers: Vec<_> = (1..=2)
        .map(|tick| {
            let store = SqliteMetadataStore::open(&f.path).unwrap();
            let barrier = barrier.clone();
            let backing = f.backing;
            let current = run(store.load_compact_inode(backing, id)).unwrap();
            std::thread::spawn(move || {
                let mut node = current.guard.node;
                node.stats.mtime_ms += tick;
                barrier.wait();
                run(store.publish_compact_inode(
                    backing,
                    id,
                    current.generation,
                    current.guard.identity,
                    node,
                ))
            })
        })
        .collect();
    let results: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert!(
        results
            .iter()
            .find_map(|r| r.as_ref().err())
            .unwrap()
            .is(ErrorCode::Eagain)
    );
    // Physical-token mismatch on the affected parent is rejected even when its
    // body remains byte-for-byte unchanged (simulated out-of-band revision).
    f.store
        .0
        .lock()
        .unwrap()
        .execute(
            "UPDATE mount_rs_compact_guards SET revision=revision+1 WHERE inode=?1",
            [base.anchor.root.to_string()],
        )
        .unwrap();
    let before = raw(&f);
    assert!(
        run(f.store.publish_compact_structure(&delta))
            .unwrap_err()
            .is(ErrorCode::Eagain)
    );
    assert_eq!(raw(&f), before);
}

#[test]
fn compact_all_operations_recheck_authority_and_lease_fences() {
    for case in ["backing", "owner", "expiry", "stamp", "generation", "mode"] {
        let f = fixture(true);
        let id = create(&f, "a");
        let snapshot = run(f.store.load_compact_snapshot(f.backing)).unwrap();
        let delta = CompactStructuralDelta::capture(
            &snapshot,
            &snapshot.namespace().unwrap(),
            StructuralScope::Full,
        )
        .unwrap();
        let old = run(f.store.load_compact_inode(f.backing, id)).unwrap();
        let mut backing = f.backing;
        match case {
            "backing" => backing = ConcurrentBackingId::from_bytes([0x11; 16]).unwrap(),
            "owner" => {
                f.store
                    .0
                    .lock()
                    .unwrap()
                    .execute("UPDATE mount_rs_metadata SET owner='expired-owner'", [])
                    .unwrap();
            }
            "expiry" => {
                f.store
                    .0
                    .lock()
                    .unwrap()
                    .execute("UPDATE mount_rs_metadata SET expires=1", [])
                    .unwrap();
            }
            "stamp" => {
                f.store
                    .0
                    .lock()
                    .unwrap()
                    .execute("UPDATE mount_rs_metadata SET physical_ino='0'", [])
                    .unwrap();
            }
            "generation" => {
                f.store
                    .0
                    .lock()
                    .unwrap()
                    .execute("UPDATE mount_rs_metadata SET revision=revision+1", [])
                    .unwrap();
            }
            "mode" => {
                f.store
                    .0
                    .lock()
                    .unwrap()
                    .execute("UPDATE mount_rs_metadata SET write_mode='MRC4'", [])
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let before = raw(&f);
        assert!(
            run(f.store.load_compact_snapshot(backing)).is_err(),
            "{case}"
        );
        assert!(
            run(f.store.load_compact_inode(backing, id)).is_err(),
            "{case}"
        );
        assert!(
            run(f.store.publish_compact_inode(
                backing,
                id,
                old.generation,
                old.guard.identity,
                old.guard.node
            ))
            .is_err(),
            "{case}"
        );
        if case != "backing" {
            assert!(
                run(f.store.publish_compact_structure(&delta)).is_err(),
                "{case}"
            );
        }
        assert_eq!(raw(&f), before);
    }
}

#[test]
fn compact_full_unlink_retains_orphan_then_removes_only_its_guard() {
    let f = fixture(true);
    let a = create(&f, "a");
    let b = create(&f, "b");
    let base = run(f.store.load_compact_snapshot(f.backing)).unwrap();
    let untouched = base.guards[&b].clone();
    let mut ns = base.namespace().unwrap();
    let NodeData::Directory { entries } = &mut ns.nodes.get_mut(&ns.root).unwrap().data else {
        panic!()
    };
    entries.retain(|entry| entry.inode != a);
    ns.nodes.get_mut(&a).unwrap().stats.nlink = 0;
    let delta = CompactStructuralDelta::capture(&base, &ns, StructuralScope::Full).unwrap();
    run(f.store.publish_compact_structure(&delta)).unwrap();
    let orphan = run(f.store.load_compact_snapshot(f.backing)).unwrap();
    assert_eq!(orphan.guards[&a].node.stats.nlink, 0);
    assert_eq!(orphan.guards[&b], untouched);
    let mut ns = orphan.namespace().unwrap();
    ns.nodes.remove(&a);
    let delta = CompactStructuralDelta::capture(&orphan, &ns, StructuralScope::Full).unwrap();
    let receipt = run(f.store.publish_compact_structure(&delta)).unwrap();
    assert_eq!(receipt.removed, BTreeSet::from([a]));
    assert!(receipt.upserts.is_empty());
    let final_state = run(f.store.load_compact_snapshot(f.backing)).unwrap();
    assert!(!final_state.guards.contains_key(&a));
    assert_eq!(final_state.guards[&b], untouched);
}

#[test]
fn compact_parent_name_precondition_rejects_an_existing_name() {
    let f = fixture(true);
    let a = create(&f, "a");
    let base = run(f.store.load_compact_snapshot(f.backing)).unwrap();
    let mut ns = base.namespace().unwrap();
    add_file(&mut ns, "b");
    let delta = CompactStructuralDelta::capture(&base, &ns, StructuralScope::FileCreate).unwrap();
    let mut parent = base.guards[&base.anchor.root].node.clone();
    let NodeData::Directory { entries } = &mut parent.data else {
        panic!()
    };
    entries.push(DirectoryEntry {
        name: "b".into(),
        inode: a,
    });
    f.store
        .0
        .lock()
        .unwrap()
        .execute(
            "UPDATE mount_rs_compact_guards SET node=?1 WHERE inode=?2",
            params![
                serde_json::to_string(&parent).unwrap(),
                base.anchor.root.to_string()
            ],
        )
        .unwrap();
    let before = raw(&f);
    assert!(
        run(f.store.publish_compact_structure(&delta))
            .unwrap_err()
            .is(ErrorCode::Eagain)
    );
    assert_eq!(raw(&f), before);
}

#[test]
fn compact_postcommit_file_fence_error_is_not_a_replayable_conflict() {
    let f = fixture(true);
    let id = create(&f, "a");
    let old = run(f.store.load_compact_inode(f.backing, id)).unwrap();
    let mut node = old.guard.node.clone();
    node.stats.mtime_ms += 1;
    let original = f.path.clone();
    let moved = f._dir.path().join("temporarily-moved.db");
    let move_target = moved.clone();
    let move_source = original.clone();
    let trace = Trace::with_action(
        &f.store,
        Some(Box::new(move || {
            std::fs::rename(move_source, move_target).unwrap();
        })),
        rusqlite::ffi::SQLITE_TRACE_PROFILE,
        "COMMIT",
    );
    let result = run(f.store.publish_compact_inode(
        f.backing,
        id,
        old.generation,
        old.guard.identity,
        node.clone(),
    ));
    assert!(
        !trace
            .state
            .callback_failed
            .load(std::sync::atomic::Ordering::Relaxed)
    );
    assert!(trace.state.before_guards.lock().unwrap().is_none());
    drop(trace);
    let error = result.unwrap_err();
    assert!(!error.is(ErrorCode::Eagain));
    std::fs::rename(moved, original).unwrap();
    let fresh = SqliteMetadataStore::open(&f.path).unwrap();
    let committed = run(fresh.load_compact_inode(f.backing, id)).unwrap();
    assert_eq!(committed.guard.node, node);
    assert_eq!(
        committed.guard.identity.revision,
        old.guard.identity.revision + 1
    );
}

#[test]
fn compact_begin_and_commit_busy_are_confirmed_rollbacks() {
    for phase in ["begin", "commit"] {
        let f = fixture(true);
        let id = create(&f, "a");
        let current = run(f.store.load_compact_inode(f.backing, id)).unwrap();
        let before = raw(&f);
        let blocker = Connection::open(&f.path).unwrap();
        if phase == "begin" {
            blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
        } else {
            blocker.execute_batch("BEGIN").unwrap();
            let _: String = blocker
                .query_row("SELECT namespace FROM mount_rs_metadata", [], |r| r.get(0))
                .unwrap();
        }
        let mut node = current.guard.node;
        node.stats.mtime_ms += 1;
        assert!(
            run(f.store.publish_compact_inode(
                f.backing,
                id,
                current.generation,
                current.guard.identity,
                node
            ))
            .unwrap_err()
            .is(ErrorCode::Eagain),
            "{phase}"
        );
        assert!(f.store.0.lock().unwrap().is_autocommit());
        blocker.execute_batch("ROLLBACK").unwrap();
        assert_eq!(raw(&f), before, "{phase}");
    }
}

fn reject_incompatible_compact_schema(definition: &str, unreadable_after_enrollment: bool) {
    // Separate fixtures cover open-time validation and the enrollment fence on
    // a handle that was opened before an out-of-band schema replacement.
    let mut accepted = false;
    for reopen in [false, true] {
        let f = fixture(false);
        let capture = || {
            let conn = f.store.0.lock().unwrap();
            let metadata: (String, u64, String, Option<String>) = conn.query_row(
                "SELECT write_mode,revision,namespace,delegation_state FROM mount_rs_metadata WHERE id=1",
                [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            ).unwrap();
            let history: (u64, u64, u64, Option<String>) = conn.query_row(
                "SELECT (SELECT count(*) FROM mount_rs_inode_guards), (SELECT count(*) FROM mount_rs_versions), (SELECT count(*) FROM mount_rs_version_pins), head_id FROM mount_rs_version_state WHERE id=1",
                [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            ).unwrap();
            let compact_count: u64 = conn
                .query_row("SELECT count(*) FROM mount_rs_compact_guards", [], |row| {
                    row.get(0)
                })
                .unwrap();
            (metadata, history, compact_count)
        };
        let before = capture();
        f.store.0.lock().unwrap().execute_batch(&format!(
            "DROP TABLE mount_rs_compact_guards; CREATE TABLE mount_rs_compact_guards({definition})"
        )).unwrap();
        let reopened = reopen.then(|| SqliteMetadataStore::open(&f.path));
        let result = match reopened.as_ref() {
            Some(Ok(store)) => run(store.prepare_compact_inode_mode(f.backing, 1)),
            Some(Err(error)) => Err(error.clone()),
            None => run(f.store.prepare_compact_inode_mode(f.backing, 1)),
        };
        if result.is_ok() {
            accepted = true;
            // On the unfixed implementation, establish the concrete bad state
            // before the rejection assertion fails: enrollment was acknowledged
            // and the integer/TEXT/REAL coerced records cannot be decoded.
            let after = capture();
            assert_eq!(after.0.0, "MRC5");
            assert_eq!(after.0.1, 2);
            assert_eq!(after.2, 1);
            let unreadable = run(f.store.load_compact_snapshot(f.backing)).is_err();
            if unreadable_after_enrollment {
                assert!(unreadable);
            }
            eprintln!(
                "INCOMPATIBLE SCHEMA ACCEPTED: reopen={reopen}, mode=MRC5, revision=2, guards=1, snapshot_unreadable={unreadable}, definition={definition}"
            );
        }
        if result.is_err() {
            assert_eq!(
                capture(),
                before,
                "refusal must preserve MRC2 body/revision/history and empty guards"
            );
        }
    }
    assert!(
        !accepted,
        "incompatible compact schema was acknowledged: {definition}"
    );
}

macro_rules! incompatible_compact_schema_test {
    ($name:ident, $schema:literal, $unreadable:literal) => {
        #[test]
        fn $name() {
            reject_incompatible_compact_schema($schema, $unreadable);
        }
    };
}
incompatible_compact_schema_test!(
    compact_schema_rejects_integer_inode,
    "inode INTEGER PRIMARY KEY NOT NULL, incarnation INTEGER NOT NULL, epoch INTEGER NOT NULL, revision INTEGER NOT NULL, node TEXT NOT NULL",
    true
);
incompatible_compact_schema_test!(
    compact_schema_rejects_text_incarnation,
    "inode TEXT PRIMARY KEY NOT NULL, incarnation TEXT NOT NULL, epoch INTEGER NOT NULL, revision INTEGER NOT NULL, node TEXT NOT NULL",
    true
);
incompatible_compact_schema_test!(
    compact_schema_rejects_real_epoch,
    "inode TEXT PRIMARY KEY NOT NULL, incarnation INTEGER NOT NULL, epoch REAL NOT NULL, revision INTEGER NOT NULL, node TEXT NOT NULL",
    true
);
incompatible_compact_schema_test!(
    compact_schema_rejects_text_revision,
    "inode TEXT PRIMARY KEY NOT NULL, incarnation INTEGER NOT NULL, epoch INTEGER NOT NULL, revision TEXT NOT NULL, node TEXT NOT NULL",
    true
);
incompatible_compact_schema_test!(
    compact_schema_rejects_composite_primary_key,
    "inode TEXT NOT NULL, incarnation INTEGER NOT NULL, epoch INTEGER NOT NULL, revision INTEGER NOT NULL, node TEXT NOT NULL, PRIMARY KEY(inode,incarnation)",
    false
);
incompatible_compact_schema_test!(
    compact_schema_rejects_nullable_columns,
    "inode TEXT PRIMARY KEY, incarnation INTEGER, epoch INTEGER, revision INTEGER, node TEXT",
    false
);
