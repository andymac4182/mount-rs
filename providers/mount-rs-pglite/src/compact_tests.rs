//! Actual provider tests. External URLs must name disposable, owned fixtures.
use super::*;
use mount_rs_core::{
    FsDriver, S_IFREG,
    chunking::{Chunker, FixedSizeChunker},
    storage::{BlockExtent, DirectoryEntry, FileLayout, NodeData, compact::*},
};
use mount_rs_memfs::MemoryFs;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
fn url() -> String {
    std::env::var("PGLITE_DATABASE_URL").expect("owned PGLITE_DATABASE_URL required")
}
async fn raw_client() -> Client {
    raw_client_at(&url()).await
}
async fn raw_client_at(connection_string: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(connection_string, NoTls)
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}
async fn fresh_namespace() -> Namespace {
    let stats = MemoryFs::empty().stat("/").await.unwrap();
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
struct Fixture {
    store: PgliteMetadataStore,
    blocks: PgliteBlockStore,
    backing: ConcurrentBackingId,
    volume: String,
    connection_string: String,
}
impl Fixture {
    async fn new(enroll: bool) -> Self {
        Self::new_at(enroll, url()).await
    }
    async fn new_at(enroll: bool, connection_string: String) -> Self {
        let volume = format!("compact-{}", uuid::Uuid::new_v4());
        let store = PgliteMetadataStore::connect_with_key(&connection_string, &volume)
            .await
            .unwrap();
        let blocks = PgliteBlockStore::connect_with_key(&connection_string, &volume)
            .await
            .unwrap();
        let backing = blocks.prepare_concurrent_backing().await.unwrap();
        store.prepare_bound_concurrent_mode(backing).await.unwrap();
        store
            .publish_bound_if_revision(backing, 0, fresh_namespace().await)
            .await
            .unwrap();
        if enroll {
            store.prepare_compact_inode_mode(backing, 1).await.unwrap();
        }
        Self {
            store,
            blocks,
            backing,
            volume,
            connection_string,
        }
    }
    async fn reopen(&self) -> PgliteMetadataStore {
        PgliteMetadataStore::connect_with_key(&self.connection_string, &self.volume)
            .await
            .unwrap()
    }
    async fn snapshot(&self) -> CompactSnapshot {
        self.store
            .load_compact_snapshot(self.backing)
            .await
            .unwrap()
    }
    async fn create(&self, name: &str) -> u64 {
        let snapshot = self.snapshot().await;
        let mut ns = snapshot.namespace().unwrap();
        let id = add_file(&mut ns, name);
        let delta =
            CompactStructuralDelta::capture(&snapshot, &ns, StructuralScope::FileCreate).unwrap();
        self.store.publish_compact_structure(&delta).await.unwrap();
        id
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
#[test]
#[ignore = "requires owned PostgreSQL-compatible endpoint"]
fn compact_capability_and_enrollment_fences() {
    runtime().block_on(async {
    let f=Fixture::new(false).await;
    assert_eq!(f.store.compact_inode_capability(),CompactInodeCapability::V1);
    f.store.prepare_compact_inode_mode(f.backing,1).await.unwrap();
    let snapshot=f.snapshot().await; assert_eq!(snapshot.anchor.generation,2);assert_eq!(snapshot.guards.len(),1);
    assert!(f.store.load().await.is_err()); assert!(f.store.load_if_changed(2).await.is_err());
    let client=raw_client().await;let row=client.query_typed_one("SELECT revision,CASE WHEN revision=1 THEN NULL ELSE namespace END FROM mount_rs_metadata WHERE volume_key=$1",&[(&f.volume,Type::TEXT)]).await.unwrap();assert_eq!(row.get::<_,i64>(0),2);let old_payload=row.get::<_,String>(1);assert!(serde_json::from_str::<Namespace>(&old_payload).is_err());assert!(decode_inode_namespace(old_payload.as_bytes()).is_err());
    assert!(f.store.load_inode_if_changed(f.backing,snapshot.anchor.root,Some(InodeVersion{structural_generation:2,inode_revision:0})).await.is_err());
    assert!(f.store.inode_mode_state().await.is_err());
    assert!(f.store.load_inode_snapshot_if_changed(f.backing,Some(2)).await.is_err());
    assert_eq!(f.reopen().await.load_compact_snapshot(f.backing).await.unwrap(),snapshot);
});
}
#[test]
#[ignore = "requires owned PostgreSQL-compatible endpoint"]
fn compact_semantic_create_and_selected_body() {
    runtime().block_on(async {
        let f = Fixture::new(true).await;
        let inode = f.create("a").await;
        let old = f.store.load_compact_inode(f.backing, inode).await.unwrap();
        let mut node = old.guard.node.clone();
        node.stats.mtime_ms += 1;
        let new = f
            .store
            .publish_compact_inode(
                f.backing,
                inode,
                old.generation,
                old.guard.identity,
                node.clone(),
            )
            .await
            .unwrap();
        assert_eq!(new.guard.node, node);
        assert_eq!(new.guard.identity.revision, 1);
        assert_eq!(
            f.store
                .publish_compact_inode(f.backing, inode, old.generation, old.guard.identity, node)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Eagain
        );
        f.create("b").await;
        let fresh = f.store.load_compact_inode(f.backing, inode).await.unwrap();
        assert_eq!(fresh.guard, new.guard);
        assert_eq!(
            fresh
                .guard
                .identity
                .logical_version(fresh.generation)
                .unwrap()
                .inode_revision,
            0
        );
    });
}

fn scoped_url(schema: &str) -> String {
    let base = url();
    if base.starts_with("postgres://") || base.starts_with("postgresql://") {
        let separator = if base.contains('?') { '&' } else { '?' };
        format!("{base}{separator}options=-csearch_path%3D{schema}")
    } else {
        format!("{base} options='-c search_path={schema}'")
    }
}
#[test]
#[ignore = "native PostgreSQL only: owned isolated schemas"]
fn compact_strict_schema_before_authority() {
    runtime().block_on(async {
    let client=raw_client().await;
    let correct="CREATE TABLE mount_rs_compact_guards(volume_key TEXT NOT NULL,inode BIGINT NOT NULL,incarnation BIGINT NOT NULL,epoch BIGINT NOT NULL,revision BIGINT NOT NULL,node TEXT NOT NULL,PRIMARY KEY(volume_key,inode))";
    let cases=[
        correct.replace("inode BIGINT","inode TEXT"),
        correct.replace("epoch BIGINT NOT NULL","epoch BIGINT"),
        correct.replace("PRIMARY KEY(volume_key,inode)","PRIMARY KEY(inode,volume_key)"),
        correct.replace("PRIMARY KEY(volume_key,inode)","PRIMARY KEY(volume_key,inode,incarnation)"),
        correct.replace("node TEXT NOT NULL","node TEXT NOT NULL,extra TEXT NOT NULL"),
        correct.replace("revision BIGINT NOT NULL","revision BIGINT GENERATED ALWAYS AS (epoch) STORED NOT NULL"),
        correct.replace("incarnation BIGINT NOT NULL","incarnation BIGINT GENERATED ALWAYS AS IDENTITY NOT NULL"),
        format!("{correct}; CREATE INDEX unexpected_expression ON mount_rs_compact_guards(lower(volume_key))"),
        format!("{correct}; CREATE INDEX unexpected_partial ON mount_rs_compact_guards(inode) WHERE inode>0"),
    ];
    for (i,sql) in cases.into_iter().enumerate() {
        let schema=format!("compact_schema_{}_{}",std::process::id(),i);
        client.batch_execute(&format!("CREATE SCHEMA {schema}; SET search_path={schema}; {sql}")).await.unwrap();
        let result=PgliteMetadataStore::connect_with_key(&scoped_url(&schema),"schema-refusal").await;
        assert!(result.is_err(),"incompatible compact schema {i} accepted");
        let count:i64=client.query_one("SELECT count(*) FROM mount_rs_metadata",&[]).await.unwrap().get(0);assert_eq!(count,0);
        client.batch_execute(&format!("SET search_path=public; DROP SCHEMA {schema} CASCADE")).await.unwrap();
    }
    let schema=format!("compact_valid_{}",std::process::id());client.batch_execute(&format!("CREATE SCHEMA {schema}; SET search_path={schema}; {correct}")).await.unwrap();
    let store=PgliteMetadataStore::connect_with_key(&scoped_url(&schema),"valid-schema").await.unwrap();store.close().await.unwrap();
    let count:i64=client.query_one("SELECT count(*) FROM mount_rs_metadata",&[]).await.unwrap().get(0);assert_eq!(count,1);
    client.batch_execute(&format!("SET search_path=public; DROP SCHEMA {schema} CASCADE")).await.unwrap();
});
}

pub(super) async fn checkpoint(point: &str) -> Result<()> {
    let Ok(control) = CONTROL.try_with(Arc::clone) else {
        return Ok(());
    };
    if control.point != point {
        return Ok(());
    }
    control.reached.notify_one();
    match control.action {
        Action::Pause => control.release.notified().await,
        Action::Fail => return Err(backend_error("synthetic compact diagnostic failure")),
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Action {
    Pause,
    Fail,
}
struct Control {
    point: &'static str,
    action: Action,
    reached: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
tokio::task_local! {static CONTROL:Arc<Control>;}
impl Control {
    fn new(point: &'static str, action: Action) -> Arc<Self> {
        Arc::new(Self {
            point,
            action,
            reached: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        })
    }
    async fn wait(&self) {
        tokio::time::timeout(Duration::from_secs(5), self.reached.notified())
            .await
            .expect("controlled seam reached");
    }
}
type Raw = (String, Vec<(i64, i64, i64, i64, String, String)>);
async fn raw(f: &Fixture) -> Raw {
    let c = raw_client_at(&f.connection_string).await;
    let authority: String = c
        .query_typed_one(
            "SELECT row_to_json(m)::text FROM mount_rs_metadata m WHERE volume_key=$1",
            &[(&f.volume, Type::TEXT)],
        )
        .await
        .unwrap()
        .get(0);
    let rows=c.query_typed("SELECT inode,incarnation,epoch,revision,node,xmin::text FROM mount_rs_compact_guards WHERE volume_key=$1 ORDER BY inode",&[(&f.volume,Type::TEXT)]).await.unwrap().into_iter().map(|r|(r.get(0),r.get(1),r.get(2),r.get(3),r.get(4),r.get(5))).collect();
    (authority, rows)
}
async fn update(
    store: &PgliteMetadataStore,
    backing: ConcurrentBackingId,
    inode: u64,
) -> LoadedCompactInode {
    let old = store.load_compact_inode(backing, inode).await.unwrap();
    let mut node = old.guard.node.clone();
    node.stats.mtime_ms += 1;
    store
        .publish_compact_inode(backing, inode, old.generation, old.guard.identity, node)
        .await
        .unwrap()
}
async fn pid(store: &PgliteMetadataStore) -> i32 {
    store
        .0
        .lock_client()
        .await
        .unwrap()
        .as_ref()
        .unwrap()
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .unwrap()
        .get(0)
}
async fn wait_lock(pid: i32) {
    let c = raw_client().await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let row = c
                .query_one(
                    "SELECT wait_event_type='Lock' FROM pg_stat_activity WHERE pid=$1",
                    &[&pid],
                )
                .await
                .unwrap();
            if row.get::<_, Option<bool>>(0) == Some(true) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("actual native PostgreSQL row-lock wait");
}
fn create_delta(base: &CompactSnapshot, name: &str) -> CompactStructuralDelta {
    let mut ns = base.namespace().unwrap();
    add_file(&mut ns, name);
    CompactStructuralDelta::capture(base, &ns, StructuralScope::FileCreate).unwrap()
}
#[test]
#[ignore = "native PostgreSQL only: controlled transaction interleave"]
fn compact_coherent_snapshot_across_two_selected_commits() {
    runtime().block_on(async {
        let f = Fixture::new(true).await;
        let a = f.create("a").await;
        let b = f.create("b").await;
        let before = f.snapshot().await;
        let reader = f.reopen().await;
        let backing = f.backing;
        let control = Control::new("snapshot_anchor", Action::Pause);
        let ctl = control.clone();
        let task =
            tokio::spawn(CONTROL.scope(
                ctl,
                async move { reader.load_compact_snapshot(backing).await },
            ));
        control.wait().await;
        update(&f.store, f.backing, a).await;
        update(&f.store, f.backing, b).await;
        control.release.notify_one();
        let seen = task.await.unwrap().unwrap();
        let after = f.snapshot().await;
        assert_eq!(before.anchor, after.anchor);
        assert_eq!(seen, before, "RR view must predate both selected commits");
        assert_ne!(seen.guards[&a], after.guards[&a]);
        assert_ne!(seen.guards[&b], after.guards[&b]);
    });
}
#[test]
#[ignore = "native PostgreSQL only: controlled transaction interleave"]
fn compact_selected_read_coherent_with_structure_commit() {
    runtime().block_on(async {
        let f = Fixture::new(true).await;
        let a = f.create("a").await;
        let before = f.snapshot().await;
        let reader = f.reopen().await;
        let backing = f.backing;
        let control = Control::new("selected_anchor", Action::Pause);
        let ctl = control.clone();
        let task =
            tokio::spawn(CONTROL.scope(
                ctl,
                async move { reader.load_compact_inode(backing, a).await },
            ));
        control.wait().await;
        let mut ns = before.namespace().unwrap();
        ns.nodes.get_mut(&a).unwrap().stats.mtime_ms += 7;
        let delta = CompactStructuralDelta::capture(&before, &ns, StructuralScope::Full).unwrap();
        f.store.publish_compact_structure(&delta).await.unwrap();
        control.release.notify_one();
        let seen = task.await.unwrap().unwrap();
        assert_eq!(seen.generation, before.anchor.generation);
        assert_eq!(seen.guard, before.guards[&a]);
    });
}
#[test]
#[ignore = "native PostgreSQL only: independent selected writers"]
fn compact_selected_different_inodes_and_structure_have_no_root_lock_cycle() {
    runtime().block_on(async {
        let f = Fixture::new(true).await;
        let a = f.create("a").await;
        let b = f.create("b").await;
        let writer = f.reopen().await;
        let old = writer.load_compact_inode(f.backing, a).await.unwrap();
        let mut node = old.guard.node.clone();
        node.stats.mtime_ms += 1;
        let control = Control::new("selected_lock", Action::Pause);
        let ctl = control.clone();
        let backing = f.backing;
        let task = tokio::spawn(CONTROL.scope(ctl, async move {
            writer
                .publish_compact_inode(backing, a, old.generation, old.guard.identity, node)
                .await
        }));
        control.wait().await;
        tokio::time::timeout(Duration::from_secs(5), update(&f.store, backing, b))
            .await
            .unwrap();
        // FileCreate locks root+parent and can commit while unrelated guard a held.
        tokio::time::timeout(Duration::from_secs(5), f.create("c"))
            .await
            .unwrap();
        control.release.notify_one();
        assert_eq!(
            task.await.unwrap().unwrap_err().code,
            ErrorCode::Eagain,
            "fresh authority after lock sees advanced generation"
        );
    });
}
#[test]
#[ignore = "native PostgreSQL only: observed same-inode lock wait"]
fn compact_selected_same_inode_one_winner_after_real_wait() {
    runtime().block_on(async {
        let f = Fixture::new(true).await;
        let a = f.create("a").await;
        let old = f.store.load_compact_inode(f.backing, a).await.unwrap();
        let backing = f.backing;
        let first = f.reopen().await;
        let second = f.reopen().await;
        let second_pid = pid(&second).await;
        let control = Control::new("selected_lock", Action::Pause);
        let ctl = control.clone();
        let x = old.clone();
        let winner = tokio::spawn(CONTROL.scope(ctl, async move {
            let mut n = x.guard.node;
            n.stats.mtime_ms += 1;
            first
                .publish_compact_inode(backing, a, x.generation, x.guard.identity, n)
                .await
        }));
        control.wait().await;
        let loser = tokio::spawn(async move {
            let mut n = old.guard.node;
            n.stats.mtime_ms += 2;
            second
                .publish_compact_inode(backing, a, old.generation, old.guard.identity, n)
                .await
        });
        wait_lock(second_pid).await;
        control.release.notify_one();
        let result = winner.await.unwrap().unwrap();
        assert_eq!(loser.await.unwrap().unwrap_err().code, ErrorCode::Eagain);
        assert_eq!(f.snapshot().await.guards[&a], result.guard);
    });
}
#[test]
#[ignore = "native PostgreSQL only: observed structure-to-selected wait"]
fn compact_selected_waits_for_structure_then_rejects_stale_generation() {
    runtime().block_on(async {
        let f = Fixture::new(true).await;
        let a = f.create("a").await;
        let before = f.snapshot().await;
        let mut ns = before.namespace().unwrap();
        ns.nodes.get_mut(&a).unwrap().stats.mtime_ms += 1;
        let delta = CompactStructuralDelta::capture(&before, &ns, StructuralScope::Full).unwrap();
        let structure = f.reopen().await;
        let selected = f.reopen().await;
        let selected_pid = pid(&selected).await;
        let backing = f.backing;
        let control = Control::new("precommit", Action::Pause);
        let ctl = control.clone();
        let first = tokio::spawn(CONTROL.scope(ctl, async move {
            structure.publish_compact_structure(&delta).await
        }));
        control.wait().await;
        let second = tokio::spawn(async move {
            selected
                .publish_compact_inode(
                    backing,
                    a,
                    before.anchor.generation,
                    before.guards[&a].identity,
                    before.guards[&a].node.clone(),
                )
                .await
        });
        wait_lock(selected_pid).await;
        control.release.notify_one();
        first.await.unwrap().unwrap();
        assert_eq!(second.await.unwrap().unwrap_err().code, ErrorCode::Eagain);
    });
}
#[test]
#[ignore = "native PostgreSQL only: cancellation after local DML before COMMIT"]
fn compact_precommit_cancel_and_abort_restore_fresh_raw_bytes() {
    runtime().block_on(async {
        for cancel in [false, true] {
            let f = Fixture::new(true).await;
            f.create("a").await;
            let before = raw(&f).await;
            let delta = create_delta(&f.snapshot().await, "b");
            let writer = f.reopen().await;
            let control = Control::new(
                "precommit",
                if cancel { Action::Pause } else { Action::Fail },
            );
            let ctl = control.clone();
            let task = tokio::spawn(CONTROL.scope(ctl, async move {
                writer.publish_compact_structure(&delta).await
            }));
            if cancel {
                control.wait().await;
                task.abort();
                assert!(task.await.unwrap_err().is_cancelled());
            } else {
                assert!(task.await.unwrap().is_err());
            }
            // Independent transaction waits behind any asynchronous Drop rollback.
            let c = raw_client().await;
            c.query_one(
                "SELECT volume_key FROM mount_rs_metadata WHERE volume_key=$1 FOR UPDATE",
                &[&f.volume],
            )
            .await
            .unwrap();
            assert_eq!(raw(&f).await, before);
        }
    });
}
#[test]
#[ignore = "native PostgreSQL only: synthetic lost acknowledgement after real COMMIT"]
fn compact_postcommit_ack_error_is_not_replayable_and_commits_once() {
    runtime().block_on(async {
        let f = Fixture::new(true).await;
        let a = f.create("a").await;
        let old = f.store.load_compact_inode(f.backing, a).await.unwrap();
        let mut node = old.guard.node.clone();
        node.stats.mtime_ms += 1;
        let expected = node.clone();
        let control = Control::new("ack", Action::Fail);
        let error = CONTROL
            .scope(
                control,
                f.store.publish_compact_inode(
                    f.backing,
                    a,
                    old.generation,
                    old.guard.identity,
                    node,
                ),
            )
            .await
            .unwrap_err();
        assert_ne!(error.code, ErrorCode::Eagain);
        let fresh = f
            .reopen()
            .await
            .load_compact_snapshot(f.backing)
            .await
            .unwrap();
        assert_eq!(fresh.guards[&a].node, expected);
        assert_eq!(
            fresh.guards[&a].identity.revision,
            old.guard.identity.revision + 1
        );
        assert_eq!(fresh.anchor.generation, old.generation);
    });
}

async fn write_bytes(f: &Fixture, inode: u64, bytes: &[u8]) {
    let old = f.store.load_compact_inode(f.backing, inode).await.unwrap();
    let mut node = old.guard.node.clone();
    let block = f.blocks.put(bytes).await.unwrap();
    f.blocks.flush().await.unwrap();
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
    f.store
        .publish_compact_inode(f.backing, inode, old.generation, old.guard.identity, node)
        .await
        .unwrap();
}
fn metric(delta: &profile::Snapshot, name: &str) -> (u64, u64) {
    delta
        .entries
        .iter()
        .find(|e| e.name == name)
        .map(|e| (e.calls, e.units))
        .unwrap_or_default()
}
#[test]
#[ignore = "requires owned PostgreSQL-compatible endpoint and MOUNT_RS_PROFILE_IO=1"]
fn compact_128_complete_guards_and_payload_bytes_with_actual_counters() {
    runtime().block_on(async {
    assert!(profile::enabled());let f=Fixture::new(true).await;let mut expected=BTreeMap::new();
    for i in 0..128 {let id=f.create(&format!("file-{i}")).await;let bytes=format!("full actual immutable bytes for file {i}: {}","abcdef0123".repeat(30)).into_bytes();write_bytes(&f,id,&bytes).await;expected.insert(id,bytes);}
    let base=f.snapshot().await;let before_raw=raw(&f).await;let delta=create_delta(&base,"next");let new=delta.base_anchor().next_inode;
    let profile_before=profile::snapshot();f.store.publish_compact_structure(&delta).await.unwrap();let create_profile=profile::snapshot().delta(&profile_before).unwrap();
    assert_eq!(metric(&create_profile,"provider.inode_returned_bytes").0,1);assert_eq!(metric(&create_profile,"provider.inode_serialized_bytes").0,3);assert_eq!(metric(&create_profile,"provider.compact_anchor_returned_bytes").0,1);assert_eq!(metric(&create_profile,"provider.compact_anchor_serialized_bytes").0,1);
    let after_raw=raw(&f).await;assert_ne!(before_raw.0,after_raw.0);let changed:Vec<_>=after_raw.1.iter().filter(|r|!before_raw.1.contains(r)).map(|r|r.0 as u64).collect();assert_eq!(changed,vec![base.anchor.root,new]);
    let before=profile::snapshot();let selected=f.store.load_compact_inode(f.backing,new).await.unwrap();let load_profile=profile::snapshot().delta(&before).unwrap();assert_eq!(metric(&load_profile,"provider.inode_returned_bytes").0,1);
    let before=profile::snapshot();let mut node=selected.guard.node.clone();node.stats.mtime_ms+=1;f.store.publish_compact_inode(f.backing,new,selected.generation,selected.guard.identity,node).await.unwrap();let selected_profile=profile::snapshot().delta(&before).unwrap();assert_eq!(metric(&selected_profile,"provider.inode_returned_bytes").0,1);assert_eq!(metric(&selected_profile,"provider.inode_serialized_bytes").0,2);assert_eq!(metric(&selected_profile,"provider.compact_anchor_serialized_bytes").0,0);
    let selected_raw=raw(&f).await;assert_eq!(selected_raw.0,after_raw.0);let changed:Vec<_>=selected_raw.1.iter().filter(|r|!after_raw.1.contains(r)).map(|r|r.0 as u64).collect();assert_eq!(changed,vec![new]);
    let fresh=f.reopen().await;let blocks=PgliteBlockStore::connect_with_key(&url(),&f.volume).await.unwrap();let snapshot=fresh.load_compact_snapshot(f.backing).await.unwrap();
    for (inode,bytes) in expected {assert_eq!(base.guards[&inode],snapshot.guards[&inode]);let node=&snapshot.guards[&inode].node;let NodeData::File(layout)=&node.data else{panic!()};let mut actual=vec![0;node.stats.size as usize];for extent in &layout.extents{let block=blocks.get(&extent.block).await.unwrap();actual[extent.file_offset as usize..(extent.file_offset+extent.length) as usize].copy_from_slice(&block[extent.block_offset as usize..(extent.block_offset+extent.length) as usize]);}assert_eq!(actual,bytes);}
    eprintln!("compact128 complete physical guards + full fresh backing bytes:128 PASS; create actual rewritten rows=2 (parent/new) + anchor; selected actual rewritten rows=1 + unchanged anchor; capture/setup excluded");
    eprintln!("create={} load={} selected={}",serde_json::to_string(&create_profile).unwrap(),serde_json::to_string(&load_profile).unwrap(),serde_json::to_string(&selected_profile).unwrap());
});
}
#[test]
#[ignore = "native PostgreSQL only: enrollment refusal raw oracles"]
fn compact_enrollment_refuses_foreign_nonfresh_and_history() {
    runtime().block_on(async {
    for case in ["backing","revision","owner","expiry","delegation","mode","nonempty","uninitialized","history","blocks"] {
        let f=Fixture::new(false).await;let c=raw_client().await;let mut backing=f.backing;let mut expected=1;
        match case {
            "backing"=>backing=ConcurrentBackingId::from_bytes([0x33;16]).unwrap(),"revision"=>expected=7,
            "owner"=>{c.execute("UPDATE mount_rs_metadata SET owner='other' WHERE volume_key=$1",&[&f.volume]).await.unwrap();},
            "expiry"=>{c.execute("UPDATE mount_rs_metadata SET expires=1 WHERE volume_key=$1",&[&f.volume]).await.unwrap();},
            "delegation"=>{c.execute("UPDATE mount_rs_metadata SET delegation='{}' WHERE volume_key=$1",&[&f.volume]).await.unwrap();},
            "mode"=>{f.store.prepare_inode_mode(f.backing,1).await.unwrap();},
            "nonempty"=>{let mut ns=fresh_namespace().await;add_file(&mut ns,"old");f.store.publish_bound_if_revision(f.backing,1,ns).await.unwrap();expected=2;},
            "uninitialized"=>{c.execute("UPDATE mount_rs_metadata SET namespace=NULL,revision=0 WHERE volume_key=$1",&[&f.volume]).await.unwrap();expected=0;},
            "history"=>{c.execute("UPDATE mount_rs_version_state SET next_sequence=2 WHERE volume_key=$1",&[&f.volume]).await.unwrap();},
            "blocks"=>{c.execute("UPDATE mount_rs_block_authority SET backing_id=$2 WHERE volume_key=$1",&[&f.volume,&ConcurrentBackingId::from_bytes([0x55;16]).unwrap().to_hex()]).await.unwrap();},_=>unreachable!()
        }
        let before=raw(&f).await;assert!(f.store.prepare_compact_inode_mode(backing,expected).await.is_err(),"{case}");assert_eq!(before,raw(&f).await,"{case}");
    }
});
}
#[test]
#[ignore = "native PostgreSQL only: authoritative fence corruptions"]
fn compact_all_operations_refuse_corrupted_authority_without_changes() {
    runtime().block_on(async {
        for assignment in [
            "write_mode='MRC4'",
            "backing_id='33333333333333333333333333333333'",
            "owner='foreign'",
            "fence=1",
            "expires=1",
            "delegation='{}'",
            "revision=revision+1",
        ] {
            let f = Fixture::new(true).await;
            let id = f.create("a").await;
            let base = f.snapshot().await;
            let delta = create_delta(&base, "b");
            let c = raw_client().await;
            c.execute(
                &format!("UPDATE mount_rs_metadata SET {assignment} WHERE volume_key=$1"),
                &[&f.volume],
            )
            .await
            .unwrap();
            let before = raw(&f).await;
            assert!(f.store.load_compact_snapshot(f.backing).await.is_err());
            assert!(f.store.load_compact_inode(f.backing, id).await.is_err());
            assert!(
                f.store
                    .publish_compact_inode(
                        f.backing,
                        id,
                        base.anchor.generation,
                        base.guards[&id].identity,
                        base.guards[&id].node.clone()
                    )
                    .await
                    .is_err()
            );
            assert!(f.store.publish_compact_structure(&delta).await.is_err());
            assert_eq!(raw(&f).await, before);
        }
    });
}
#[test]
#[ignore = "native PostgreSQL only: corrupted full keyspace/body/identity/graph"]
fn compact_exact_keyspace_body_and_graph_corruptions_fail_closed() {
    runtime().block_on(async {
    for case in ["extra","missing","incarnation","future_epoch","negative_revision","wrong_body_inode","unknown_body_field","header","graph","membership"] {
        let f=Fixture::new(true).await;let id=f.create("a").await;let base=f.snapshot().await;let mut ns=base.namespace().unwrap();ns.nodes.get_mut(&id).unwrap().stats.mtime_ms+=1;let delta=CompactStructuralDelta::capture(&base,&ns,StructuralScope::Full).unwrap();let c=raw_client().await;
        match case {
            "extra"=>{c.execute("INSERT INTO mount_rs_compact_guards SELECT volume_key,999,incarnation,epoch,revision,node FROM mount_rs_compact_guards WHERE volume_key=$1 AND inode=$2",&[&f.volume,&(id as i64)]).await.unwrap();},
            "missing"=>{c.execute("DELETE FROM mount_rs_compact_guards WHERE volume_key=$1 AND inode=$2",&[&f.volume,&(id as i64)]).await.unwrap();},
            "incarnation"|"future_epoch"|"negative_revision"=>{let assignment=match case{"incarnation"=>"incarnation=0","future_epoch"=>"epoch=999",_=>"revision=-1"};c.execute(&format!("UPDATE mount_rs_compact_guards SET {assignment} WHERE volume_key=$1 AND inode=$2"),&[&f.volume,&(id as i64)]).await.unwrap();},
            "wrong_body_inode"=>{let mut node=base.guards[&id].node.clone();node.stats.ino=99;c.execute("UPDATE mount_rs_compact_guards SET node=$3 WHERE volume_key=$1 AND inode=$2",&[&f.volume,&(id as i64),&serde_json::to_string(&node).unwrap()]).await.unwrap();},
            "unknown_body_field"=>{let mut value=serde_json::to_value(&base.guards[&id].node).unwrap();value["future_field"]=serde_json::json!(true);c.execute("UPDATE mount_rs_compact_guards SET node=$3 WHERE volume_key=$1 AND inode=$2",&[&f.volume,&(id as i64),&value.to_string()]).await.unwrap();},
            "header"=>{c.execute("UPDATE mount_rs_metadata SET namespace=replace(namespace,'mount-rs-compact-inodes','wrong-layout') WHERE volume_key=$1",&[&f.volume]).await.unwrap();},
            "graph"=>{let mut node=base.guards[&base.anchor.root].node.clone();let NodeData::Directory{entries}=&mut node.data else{panic!()};entries.clear();c.execute("UPDATE mount_rs_compact_guards SET node=$3 WHERE volume_key=$1 AND inode=$2",&[&f.volume,&(base.anchor.root as i64),&serde_json::to_string(&node).unwrap()]).await.unwrap();},
            "membership"=>{let mut a=base.anchor.clone();a.members.push(999);a.next_inode=1000;c.execute("UPDATE mount_rs_metadata SET namespace=$2 WHERE volume_key=$1",&[&f.volume,&String::from_utf8(encode_compact_anchor(&a).unwrap()).unwrap()]).await.unwrap();},_=>unreachable!()
        }
        let before=raw(&f).await;assert!(f.store.load_compact_snapshot(f.backing).await.is_err(),"{case}");assert!(f.store.publish_compact_structure(&delta).await.is_err(),"{case}");assert_eq!(raw(&f).await,before,"{case}");
    }
});
}
#[test]
#[ignore = "requires owned PostgreSQL-compatible endpoint"]
fn compact_full_unlink_orphan_write_and_final_removal() {
    runtime().block_on(async {
        let f = Fixture::new(true).await;
        let a = f.create("a").await;
        let b = f.create("b").await;
        let before = f.snapshot().await;
        let mut ns = before.namespace().unwrap();
        let NodeData::Directory { entries } = &mut ns.nodes.get_mut(&ns.root).unwrap().data else {
            panic!()
        };
        entries.retain(|e| e.inode != a);
        ns.nodes.get_mut(&a).unwrap().stats.nlink = 0;
        let delta = CompactStructuralDelta::capture(&before, &ns, StructuralScope::Full).unwrap();
        f.store.publish_compact_structure(&delta).await.unwrap();
        write_bytes(
            &f,
            a,
            b"actual bytes written after unlink through retained inode",
        )
        .await;
        let orphan = f.snapshot().await;
        assert_eq!(orphan.guards[&a].node.stats.nlink, 0);
        assert_eq!(orphan.guards[&b], before.guards[&b]);
        let mut ns = orphan.namespace().unwrap();
        ns.nodes.remove(&a);
        let delta = CompactStructuralDelta::capture(&orphan, &ns, StructuralScope::Full).unwrap();
        let publication = f.store.publish_compact_structure(&delta).await.unwrap();
        assert_eq!(publication.removed.into_iter().collect::<Vec<_>>(), vec![a]);
        assert!(publication.upserts.is_empty());
        let after = f.snapshot().await;
        assert!(!after.guards.contains_key(&a));
        assert_eq!(after.guards[&b], before.guards[&b]);
    });
}
#[test]
#[ignore = "native PostgreSQL only: stale Full versus targeted create and duplicate allocation/name"]
fn compact_stale_full_targeted_create_and_duplicate_races() {
    runtime().block_on(async {
        let f = Fixture::new(true).await;
        let a = f.create("a").await;
        let base = f.snapshot().await;
        let create = create_delta(&base, "b");
        let mut ns = base.namespace().unwrap();
        ns.nodes.get_mut(&a).unwrap().stats.mtime_ms += 1;
        let full = CompactStructuralDelta::capture(&base, &ns, StructuralScope::Full).unwrap();
        let newer = update(&f.store, f.backing, a).await;
        let before = raw(&f).await;
        assert_eq!(
            f.store
                .publish_compact_structure(&full)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Eagain
        );
        assert_eq!(before, raw(&f).await);
        f.store.publish_compact_structure(&create).await.unwrap();
        assert_eq!(f.snapshot().await.guards[&a], newer.guard);
        let committed = raw(&f).await;
        assert_eq!(
            f.store
                .publish_compact_structure(&create)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Eagain
        );
        assert_eq!(committed, raw(&f).await);
        let base = f.snapshot().await;
        let delta = create_delta(&base, "c");
        let c = raw_client().await;
        let mut parent = base.guards[&base.anchor.root].node.clone();
        let NodeData::Directory { entries } = &mut parent.data else {
            panic!()
        };
        entries.push(DirectoryEntry {
            name: "c".into(),
            inode: a,
        });
        c.execute(
            "UPDATE mount_rs_compact_guards SET node=$3 WHERE volume_key=$1 AND inode=$2",
            &[
                &f.volume,
                &(base.anchor.root as i64),
                &serde_json::to_string(&parent).unwrap(),
            ],
        )
        .await
        .unwrap();
        let before = raw(&f).await;
        assert!(f.store.publish_compact_structure(&delta).await.is_err());
        assert_eq!(before, raw(&f).await);
        // Unexpected allocated key absent from anchor must be refused pre-DML.
        let f = Fixture::new(true).await;
        let base = f.snapshot().await;
        let delta = create_delta(&base, "x");
        let mut node = delta.created()[&base.anchor.next_inode].clone();
        node.stats.mtime_ms += 1;
        c.execute(
            "INSERT INTO mount_rs_compact_guards VALUES($1,$2,2,2,0,$3)",
            &[
                &f.volume,
                &(base.anchor.next_inode as i64),
                &serde_json::to_string(&node).unwrap(),
            ],
        )
        .await
        .unwrap();
        let before = raw(&f).await;
        assert_eq!(
            f.store
                .publish_compact_structure(&delta)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Eagain
        );
        assert_eq!(before, raw(&f).await);
    });
}

#[test]
#[ignore = "native PostgreSQL only: SQL rollback and same-client recovery on isolated schema"]
fn compact_real_sql_abort_rolls_back_every_local_write() {
    runtime().block_on(async {
        let admin = raw_client().await;
        let schema = format!("compact_abort_{}", std::process::id());
        admin
            .batch_execute(&format!("CREATE SCHEMA {schema}"))
            .await
            .unwrap();
        let f = Fixture::new_at(true, scoped_url(&schema)).await;
        let id = f.create("a").await;
        let base = f.snapshot().await;
        let delta = create_delta(&base, "b");
        let c = raw_client_at(&f.connection_string).await;
        c.batch_execute(&format!(
            "ALTER TABLE mount_rs_metadata ADD CONSTRAINT test_abort CHECK(revision<={})",
            base.anchor.generation
        ))
        .await
        .unwrap();
        let before = raw(&f).await;
        let error = f.store.publish_compact_structure(&delta).await.unwrap_err();
        assert_ne!(error.code, ErrorCode::Eagain);
        assert!(error.to_string().contains("23514"));
        assert_eq!(before, raw(&f).await);
        update(&f.store, f.backing, id).await;
        f.store.close().await.unwrap();
        f.blocks.close().await.unwrap();
        admin
            .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
            .await
            .unwrap();
    });
}
#[test]
#[ignore = "native PostgreSQL only: signed/body/read/mutation bounds before DML"]
fn compact_bounds_refuse_before_any_mutation() {
    runtime().block_on(async {
    let f=Fixture::new(true).await;let id=f.create("a").await;let base=f.snapshot().await;let before=raw(&f).await;
    let mut identity=base.guards[&id].identity;identity.revision=u64::MAX;
    assert_eq!(f.store.publish_compact_inode(f.backing,id,base.anchor.generation,identity,base.guards[&id].node.clone()).await.unwrap_err().code,ErrorCode::Eoverflow);
    let mut ns=base.namespace().unwrap();let high=i64::MAX as u64+1;let mut node=ns.nodes[&id].clone();node.stats.ino=high;ns.nodes.remove(&id);ns.nodes.insert(high,node);ns.next_inode=high+1;let NodeData::Directory{entries}=&mut ns.nodes.get_mut(&ns.root).unwrap().data else{panic!()};entries[0].inode=high;
    let delta=CompactStructuralDelta::capture(&base,&ns,StructuralScope::Full).unwrap();assert_eq!(f.store.publish_compact_structure(&delta).await.unwrap_err().code,ErrorCode::Eoverflow);assert_eq!(before,raw(&f).await);
    let mut oversized=base.guards[&id].node.clone();oversized.data=NodeData::Symlink{target:"x".repeat(16*1024*1024)};oversized.stats.mode=mount_rs_core::S_IFLNK|0o777;
    assert_eq!(f.store.publish_compact_inode(f.backing,id,base.anchor.generation,base.guards[&id].identity,oversized).await.unwrap_err().code,ErrorCode::Efbig);assert_eq!(before,raw(&f).await);
    let c=raw_client().await;c.execute("UPDATE mount_rs_compact_guards SET node=repeat('x',16777217) WHERE volume_key=$1 AND inode=$2",&[&f.volume,&(id as i64)]).await.unwrap();let before=raw(&f).await;assert_eq!(f.store.load_compact_snapshot(f.backing).await.unwrap_err().code,ErrorCode::Efbig);assert_eq!(before,raw(&f).await);
    // Four individually admissible 9MiB nodes exceed the shared 64MiB budget
    // when input/mutation and canonical read encodings are accounted.
    let f=Fixture::new(true).await;for i in 0..4{f.create(&format!("f{i}")).await;}let base=f.snapshot().await;let mut ns=base.namespace().unwrap();for node in ns.nodes.values_mut().filter(|n|n.stats.ino!=ns.root){node.data=NodeData::Symlink{target:"z".repeat(9*1024*1024)};node.stats.mode=mount_rs_core::S_IFLNK|0o777;}
    let delta=CompactStructuralDelta::capture(&base,&ns,StructuralScope::Full).unwrap();let before=raw(&f).await;assert_eq!(f.store.publish_compact_structure(&delta).await.unwrap_err().code,ErrorCode::Efbig);assert_eq!(before,raw(&f).await);
    for (&inode,node) in ns.nodes.iter().filter(|&(&id,_)|id!=ns.root){c.execute("UPDATE mount_rs_compact_guards SET node=$3 WHERE volume_key=$1 AND inode=$2",&[&f.volume,&(inode as i64),&serde_json::to_string(node).unwrap()]).await.unwrap();}
    let before=raw(&f).await;assert_eq!(f.store.load_compact_snapshot(f.backing).await.unwrap_err().code,ErrorCode::Efbig);let delta=CompactStructuralDelta::capture(&base,&base.namespace().unwrap(),StructuralScope::Full).unwrap();assert_eq!(f.store.publish_compact_structure(&delta).await.unwrap_err().code,ErrorCode::Efbig);assert_eq!(before,raw(&f).await);
    // Each side fits alone: four 4MiB current bodies cost ~32MiB with
    // validation serialization; two 9MiB planned bodies cost ~36MiB with
    // input/mutation accounting. Their combined transaction must refuse.
    for (&inode,node) in ns.nodes.iter_mut().filter(|&(&id,_)|id!=ns.root){node.data=NodeData::Symlink{target:"r".repeat(4*1024*1024)};c.execute("UPDATE mount_rs_compact_guards SET node=$3 WHERE volume_key=$1 AND inode=$2",&[&f.volume,&(inode as i64),&serde_json::to_string(node).unwrap()]).await.unwrap();}
    let base=f.snapshot().await;let mut candidate=base.namespace().unwrap();for node in candidate.nodes.values_mut().filter(|n|n.stats.ino!=candidate.root).take(2){node.data=NodeData::Symlink{target:"m".repeat(9*1024*1024)};}
    let delta=CompactStructuralDelta::capture(&base,&candidate,StructuralScope::Full).unwrap();let before=raw(&f).await;assert_eq!(f.store.publish_compact_structure(&delta).await.unwrap_err().code,ErrorCode::Efbig);assert_eq!(before,raw(&f).await);
    eprintln!("actual bounds: signed inode/identity overflow,16MiB body projection,64MiB aggregate mutation/read refusal unchanged raw bytes PASS; combined sublimit read~32MiB + input/mutation~36MiB refused pre-DML PASS");
});
}
#[test]
#[ignore = "native PostgreSQL only: post-open NULL corruption returns error"]
fn compact_corrupted_null_after_open_returns_error_without_panicking() {
    runtime().block_on(async {
    let admin=raw_client().await;let schema=format!("compact_null_{}",std::process::id());admin.batch_execute(&format!("CREATE SCHEMA {schema}")).await.unwrap();let f=Fixture::new_at(true,scoped_url(&schema)).await;let c=raw_client_at(&f.connection_string).await;
    c.batch_execute("ALTER TABLE mount_rs_compact_guards ALTER COLUMN epoch DROP NOT NULL; UPDATE mount_rs_compact_guards SET epoch=NULL").await.unwrap();assert!(f.store.load_compact_snapshot(f.backing).await.is_err());assert!(PgliteMetadataStore::connect_with_key(&f.connection_string,&f.volume).await.is_err());
    f.store.close().await.unwrap();f.blocks.close().await.unwrap();admin.batch_execute(&format!("DROP SCHEMA {schema} CASCADE")).await.unwrap();
});
}

#[test]
#[ignore = "native PostgreSQL only: simultaneous structural allocation"]
fn compact_duplicate_create_waits_on_root_and_has_one_winner() {
    runtime().block_on(async {
        let f = Fixture::new(true).await;
        let base = f.snapshot().await;
        let one = create_delta(&base, "one");
        let two = create_delta(&base, "two");
        let first = f.reopen().await;
        let second = f.reopen().await;
        let second_pid = pid(&second).await;
        let ctl = Control::new("structure_lock", Action::Pause);
        let control = ctl.clone();
        let winner = tokio::spawn(CONTROL.scope(control, async move {
            first.publish_compact_structure(&one).await
        }));
        ctl.wait().await;
        let loser = tokio::spawn(async move { second.publish_compact_structure(&two).await });
        wait_lock(second_pid).await;
        let c = raw_client().await;
        let query: String = c
            .query_one(
                "SELECT query FROM pg_stat_activity WHERE pid=$1",
                &[&second_pid],
            )
            .await
            .unwrap()
            .get(0);
        assert!(query.contains("mount_rs_metadata"));
        ctl.release.notify_one();
        winner.await.unwrap().unwrap();
        assert_eq!(loser.await.unwrap().unwrap_err().code, ErrorCode::Eagain);
        let after = f.snapshot().await;
        assert_eq!(after.guards.len(), 2);
        assert_eq!(after.anchor.generation, base.anchor.generation + 1);
        let NodeData::Directory { entries } = &after.guards[&after.anchor.root].node.data else {
            panic!()
        };
        assert_eq!(
            entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            vec!["one"]
        );
    });
}
#[test]
#[ignore = "requires owned PostgreSQL-compatible endpoint"]
fn compact_selected_refuses_structural_policy_changes() {
    runtime().block_on(async {
        let f = Fixture::new(true).await;
        let id = f.create("a").await;
        let old = f.store.load_compact_inode(f.backing, id).await.unwrap();
        let before = raw(&f).await;
        for case in ["inode", "mode", "uid", "nlink", "kind"] {
            let mut node = old.guard.node.clone();
            match case {
                "inode" => node.stats.ino += 1,
                "mode" => node.stats.mode ^= 1,
                "uid" => node.stats.uid += 1,
                "nlink" => node.stats.nlink = 0,
                "kind" => {
                    node.data = NodeData::Directory { entries: vec![] };
                    node.stats.mode = mount_rs_core::S_IFDIR | 0o755;
                }
                _ => unreachable!(),
            };
            assert!(
                f.store
                    .publish_compact_inode(f.backing, id, old.generation, old.guard.identity, node)
                    .await
                    .is_err(),
                "{case}"
            );
            assert_eq!(before, raw(&f).await);
        }
    });
}
