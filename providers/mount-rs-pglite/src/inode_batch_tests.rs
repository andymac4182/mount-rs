use super::*;
#[test]
fn frontend_bound_includes_describe_unicode_and_overflow() {
    assert_eq!(
        encoded_bound(1, 0, 0),
        Some(PREFIX.len() + 24 + 46 + 40 + 16)
    );
    assert_eq!(
        encoded_bound(64, 1020, 1000).unwrap() - encoded_bound(64, 255, 1000).unwrap(),
        64 * (1020 - 255)
    );
    assert!(encoded_bound(usize::MAX, 1, 1).is_none());
    assert!(encoded_bound(1, usize::MAX, 1).is_none());
}

use std::sync::{Arc, Mutex};
use tokio::sync::Notify;
#[derive(Clone, Copy)]
enum Action {
    Record,
    FailSecond,
    PauseSecond,
}

struct Control {
    action: Action,
    rows: Mutex<Vec<usize>>,
    reached: Notify,
    release: Notify,
}

tokio::task_local! {
static CONTROL: Arc<Control>;
}

impl Control {
    fn new(action: Action) -> Arc<Self> {
        Arc::new(Self {
            action,
            rows: Mutex::new(vec![]),
            reached: Notify::new(),
            release: Notify::new(),
        })
    }
}

async fn action(rows: usize) -> bool {
    let Ok(control) = CONTROL.try_with(Arc::clone) else {
        return false;
    };
    let index = {
        let mut seen = control.rows.lock().unwrap();
        seen.push(rows);
        seen.len()
    };
    if index == 2 {
        match control.action {
            Action::FailSecond => return true,
            Action::PauseSecond => {
                control.reached.notify_one();
                control.release.notified().await;
            }
            Action::Record => {}
        }
    }
    false
}

pub(super) async fn before_batch(rows: &mut [(i64, String)]) {
    if action(rows.len()).await {
        rows[0].0 = 1;
    }
}

pub(super) async fn before_single(inode: i64) -> i64 {
    if action(1).await { 1 } else { inode }
}

fn fixture(count: usize) -> Namespace {
    use mount_rs_core::chunking::{Chunker, FixedSizeChunker};
    use mount_rs_core::storage::{DirectoryEntry, FileLayout, NodeData};
    use mount_rs_core::{S_IFDIR, S_IFREG, Stats};
    let stats = Stats {
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
    };
    let chunker = FixedSizeChunker::new(4096).unwrap().config();
    let mut nodes = BTreeMap::new();
    let mut entries = vec![];
    for inode in 2..=count as u64 {
        let mut stats = stats.clone();
        stats.ino = inode;
        stats.mode = S_IFREG | 0o644;
        stats.nlink = 1;
        nodes.insert(
            inode,
            NodeMetadata {
                stats,
                data: NodeData::File(FileLayout {
                    chunker: chunker.clone(),
                    extents: vec![],
                }),
            },
        );
        entries.push(DirectoryEntry {
            name: format!("f{inode}"),
            inode,
        });
    }
    nodes.insert(
        1,
        NodeMetadata {
            stats,
            data: NodeData::Directory { entries },
        },
    );
    Namespace {
        format_version: 1,
        root: 1,
        next_inode: count as u64 + 1,
        default_uid: 0,
        default_gid: 0,
        umask: 0o022,
        default_chunker: chunker,
        nodes,
    }
}

fn owned_key() -> String {
    format!(
        "guard-fault-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}

async fn cleanup(url: &str, key: &str) {
    let (c, connection) = tokio_postgres::connect(url, tokio_postgres::NoTls)
        .await
        .unwrap();
    let task = tokio::spawn(connection);
    for table in ["mount_rs_inode_guards", "mount_rs_metadata"] {
        c.execute_typed(
            &format!("DELETE FROM {table} WHERE volume_key=$1"),
            &[(&key, Type::TEXT)],
        )
        .await
        .unwrap();
    }
    drop(c);
    task.await.unwrap().unwrap();
}

#[test]
#[ignore = "requires actual pglite and PGLITE_DATABASE_URL"]
fn actual_guard_batch_boundaries_and_rollback_cancel() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(actual_guard_batch_boundaries_and_rollback_cancel_body());
}
async fn actual_guard_batch_boundaries_and_rollback_cancel_body() {
    let url = std::env::var("PGLITE_DATABASE_URL").unwrap();
    for count in [1, 63, 64, 65, 131] {
        let key = owned_key();
        let mut store = PgliteMetadataStore::connect_with_key(&url, &key)
            .await
            .unwrap();
        let backing = ConcurrentBackingId::from_bytes([7; 16]).unwrap();
        store.prepare_bound_concurrent_mode(backing).await.unwrap();
        let expected = fixture(count);
        store
            .publish_bound_if_revision(backing, 0, expected.clone())
            .await
            .unwrap();
        let control = Control::new(Action::Record);
        CONTROL
            .scope(control.clone(), store.prepare_inode_mode(backing, 1))
            .await
            .unwrap();
        let counts = control.rows.lock().unwrap().clone();
        let expected_counts: Vec<_> = (0..count)
            .collect::<Vec<_>>()
            .chunks(64)
            .map(|v| v.len())
            .collect();
        assert_eq!(counts, expected_counts);
        let before = store.load_inode_snapshot(backing).await.unwrap();
        assert_eq!(before.inode_revisions.len(), count);
        assert_eq!(
            serde_json::to_vec(&before.namespace).unwrap(),
            serde_json::to_vec(&expected).unwrap()
        );
        if count == 131 {
            let mut changed = before.namespace.clone();
            changed.nodes.get_mut(&2).unwrap().stats.mtime_ms = 999;
            let fault = Control::new(Action::FailSecond);
            let error = CONTROL
                .scope(
                    fault.clone(),
                    store.publish_structure_if_versions(
                        backing,
                        2,
                        &before.inode_revisions,
                        changed.clone(),
                    ),
                )
                .await
                .unwrap_err();
            assert_ne!(error.code, ErrorCode::Eagain);
            assert_eq!(*fault.rows.lock().unwrap(), [64, 64]);
            // Pinned PGlite emits a duplicate ReadyForQuery on a real SQL error
            // even for the old typed singleton path (separate direct/proxy controls).
            // Qualify rollback visibility with a fresh connection, preserving that
            // same-client SQL-error recovery remains an explicit failed backend gate.
            store.close().await.unwrap();
            store = PgliteMetadataStore::connect_with_key(&url, &key)
                .await
                .unwrap();
            let after = store.load_inode_snapshot(backing).await.unwrap();
            assert_eq!(after.structural_generation, 2);
            assert_eq!(
                serde_json::to_vec(&after.namespace).unwrap(),
                serde_json::to_vec(&before.namespace).unwrap()
            );
            let pause = Control::new(Action::PauseSecond);
            let task = tokio::spawn({
                let store = store.clone();
                let pause = pause.clone();
                let versions = before.inode_revisions.clone();
                let changed = changed.clone();
                async move {
                    CONTROL
                        .scope(
                            pause,
                            store.publish_structure_if_versions(backing, 2, &versions, changed),
                        )
                        .await
                }
            });
            tokio::time::timeout(std::time::Duration::from_secs(10), pause.reached.notified())
                .await
                .unwrap();
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
            assert_eq!(*pause.rows.lock().unwrap(), [64, 64]);
            let after = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                store.load_inode_snapshot(backing),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(after.structural_generation, 2);
            assert_eq!(after.inode_revisions, before.inode_revisions);
            assert_eq!(
                serde_json::to_vec(&after.namespace).unwrap(),
                serde_json::to_vec(&before.namespace).unwrap()
            );
            assert_eq!(
                store
                    .publish_structure_if_versions(backing, 2, &before.inode_revisions, changed)
                    .await
                    .unwrap(),
                3
            );

            // Structural locks are already held when the second batch pause fires.
            // Start a public selected publisher with the old guard while structure is
            // paused; it must never acknowledge the stale generation after release.
            let peer = PgliteMetadataStore::connect_with_key(&url, &key)
                .await
                .unwrap();
            let current = store.load_inode_snapshot(backing).await.unwrap();
            let selected = peer.load_inode(backing, 3).await.unwrap();
            let mut selected_node = selected.node.clone();
            selected_node.stats.mtime_ms = 1234;
            let gate = Control::new(Action::PauseSecond);
            let structure = tokio::spawn({
                let store = store.clone();
                let gate = gate.clone();
                let versions = current.inode_revisions.clone();
                let mut ns = current.namespace.clone();
                ns.nodes.get_mut(&2).unwrap().stats.mtime_ms = 888;
                async move {
                    CONTROL
                        .scope(
                            gate,
                            store.publish_structure_if_versions(backing, 3, &versions, ns),
                        )
                        .await
                }
            });
            tokio::time::timeout(std::time::Duration::from_secs(10), gate.reached.notified())
                .await
                .unwrap();
            let (started, start) = tokio::sync::oneshot::channel();
            let writer = tokio::spawn({
                let peer = peer.clone();
                async move {
                    started.send(()).unwrap();
                    peer.publish_inode_if_version(backing, 3, selected.version, selected_node)
                        .await
                }
            });
            start.await.unwrap();
            gate.release.notify_one();
            assert_eq!(
                tokio::time::timeout(std::time::Duration::from_secs(10), structure)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap(),
                4
            );
            let result = tokio::time::timeout(std::time::Duration::from_secs(10), writer)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(result.unwrap_err().code, ErrorCode::Eagain);
            let actual = peer.load_inode(backing, 3).await.unwrap();
            assert_ne!(actual.node.stats.mtime_ms, 1234);
            assert_eq!(actual.version.structural_generation, 4);
            peer.close().await.unwrap();
            println!(
                "GUARD_SELECTED_RACE_PASS provider=pglite stale_selected_not_acknowledged=true generation=4"
            );
            println!(
                "GUARD_FAULT_PASS provider=pglite real_second_batch_duplicate_rollback=true cancel_after_first_batch=true clean_cancel_reuse_publication=true sqlerror_same_client_recovery_unqualified=true"
            );
        }

        store.close().await.unwrap();
        cleanup(&url, &key).await;
        println!("GUARD_BOUNDARY_PASS provider=pglite nodes={count} rows={counts:?}");
    }
}

#[test]
#[ignore = "requires actual pglite and PGLITE_DATABASE_URL"]
fn actual_guard_byte_boundary_and_oversized_singletons() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(actual_guard_byte_boundary_and_oversized_singletons_body());
}
async fn actual_guard_byte_boundary_and_oversized_singletons_body() {
    let url = std::env::var("PGLITE_DATABASE_URL").unwrap();
    for scenario in ["exact", "over", "oversized", "byte-split"] {
        let key = format!("{}-é🗃", owned_key());
        let store = PgliteMetadataStore::connect_with_key(&url, &key)
            .await
            .unwrap();
        let backing = ConcurrentBackingId::from_bytes([8; 16]).unwrap();
        store.prepare_bound_concurrent_mode(backing).await.unwrap();
        let mut ns = fixture(if scenario == "oversized" || scenario == "byte-split" {
            8
        } else {
            3
        });
        let ids: Vec<u64> = match scenario {
            "oversized" => vec![2, 5, 8],
            "byte-split" => (2..=8).collect(),
            _ => vec![2],
        };
        for inode in ids {
            let node = ns.nodes.get_mut(&inode).unwrap();
            node.stats.mode = mount_rs_core::S_IFLNK | 0o777;
            node.data = mount_rs_core::storage::NodeData::Symlink {
                target: match scenario {
                    "oversized" => "é\"\\".repeat(70_000),
                    "byte-split" => "x".repeat(90_000),
                    _ => String::new(),
                },
            };
        }

        if scenario == "exact" || scenario == "over" {
            let node_bytes: usize = ns
                .nodes
                .iter()
                .take(2)
                .map(|(_, n)| serde_json::to_string(n).unwrap().len())
                .sum();
            let padding = MAX_BYTES - encoded_bound(2, key.len(), node_bytes).unwrap()
                + usize::from(scenario == "over");
            let mount_rs_core::storage::NodeData::Symlink { target } =
                &mut ns.nodes.get_mut(&2).unwrap().data
            else {
                unreachable!()
            };
            *target = "x".repeat(padding);
            let bytes: usize = ns
                .nodes
                .iter()
                .take(2)
                .map(|(_, n)| serde_json::to_string(n).unwrap().len())
                .sum();
            assert_eq!(
                encoded_bound(2, key.len(), bytes).unwrap(),
                MAX_BYTES + usize::from(scenario == "over")
            );
        }

        ns.validate().unwrap();
        store
            .publish_bound_if_revision(backing, 0, ns.clone())
            .await
            .unwrap();
        let control = Control::new(Action::Record);
        CONTROL
            .scope(control.clone(), store.prepare_inode_mode(backing, 1))
            .await
            .unwrap();
        let rows = control.rows.lock().unwrap().clone();
        match scenario {
            "exact" => assert_eq!(rows, [2, 1]),
            "over" => assert_eq!(rows[0], 1),
            "oversized" => assert_eq!(rows, [1, 1, 2, 1, 2, 1]),
            "byte-split" => assert_eq!(rows, [3, 2, 2, 1]),
            _ => unreachable!(),
        }

        let actual = store.load_inode_snapshot(backing).await.unwrap();
        assert_eq!(actual.structural_generation, 2);
        assert!(actual.inode_revisions.values().all(|r| *r == 0));
        assert_eq!(
            serde_json::to_vec(&actual.namespace).unwrap(),
            serde_json::to_vec(&ns).unwrap()
        );
        store.close().await.unwrap();
        cleanup(&url, &key).await;
        println!(
            "GUARD_SIZE_PASS provider=pglite scenario={scenario} rows={rows:?} exact_namespace=true"
        );
    }
}

#[test]
#[ignore = "requires actual PGlite and PGLITE_DATABASE_URL; documents pinned upstream recovery gap"]
fn actual_singleton_duplicate_without_proxy_control() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(actual_singleton_duplicate_without_proxy_control_body());
}
async fn actual_singleton_duplicate_without_proxy_control_body() {
    let url = std::env::var("PGLITE_DATABASE_URL").unwrap();
    let key = owned_key();
    let store = PgliteMetadataStore::connect_with_key(&url, &key)
        .await
        .unwrap();
    let backing = ConcurrentBackingId::from_bytes([7; 16]).unwrap();
    store.prepare_bound_concurrent_mode(backing).await.unwrap();
    let ns = fixture(131);
    store
        .publish_bound_if_revision(backing, 0, ns.clone())
        .await
        .unwrap();
    store.prepare_inode_mode(backing, 1).await.unwrap();
    {
        let mut client = store.0.lock_client().await.unwrap();
        let client = client.as_mut().unwrap();
        let tx = client.transaction().await.unwrap();
        let node = encode_inode_node(&ns.nodes[&1]).unwrap();
        let error=tx.execute_typed("INSERT INTO mount_rs_inode_guards(volume_key,inode,generation,revision,node) VALUES($1,$2,$3,0,$4)",&[(&key,Type::TEXT),(&1_i64,Type::INT8),(&2_i64,Type::INT8),(&node,Type::TEXT)]).await.unwrap_err();
        assert_eq!(error.code().unwrap().code(), "23505");
        drop(tx);
        let error = client
            .query_typed_one(
                "SELECT revision FROM mount_rs_metadata WHERE volume_key=$1",
                &[(&key, Type::TEXT)],
            )
            .await
            .unwrap_err();
        let category = match error.to_string().as_str() {
            "unexpected message from server" => "unexpected-message",
            "connection closed" => "connection-closed",
            _ => "other-error",
        };
        println!("PG_DIRECT_SINGLETON_FAILURE category={category}");
    }

    let fresh = PgliteMetadataStore::connect_with_key(&url, &key)
        .await
        .unwrap();
    let snapshot = fresh.load_inode_snapshot(backing).await.unwrap();
    assert_eq!(snapshot.structural_generation, 2);
    assert_eq!(
        serde_json::to_vec(&snapshot.namespace).unwrap(),
        serde_json::to_vec(&ns).unwrap()
    );
    fresh.close().await.unwrap();
    store.close().await.unwrap();
    cleanup(&url, &key).await;
    println!(
        "PG_DIRECT_SINGLETON_CONTROL no_proxy=true sqlstate=23505 same_client_recovery_failed=true fresh_oracle_pass=true"
    );
}

#[test]
#[ignore = "requires owned actual datastore and cleanup manifest"]
fn cleanup_public_sdk_bulk_owned_rows() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(cleanup_public_sdk_bulk_owned_rows_body());
}
async fn cleanup_public_sdk_bulk_owned_rows_body() {
    let manifest =
        std::fs::read_to_string(std::env::var("MOUNT_RS_BULK_CLEANUP_MANIFEST").unwrap()).unwrap();
    let url = std::env::var("PGLITE_DATABASE_URL").unwrap();
    for line in manifest.lines() {
        let (provider, key) = line.split_once('\t').unwrap();
        assert!(["tidb", "pglite"].contains(&provider));
        if provider != "pglite" {
            continue;
        }
        let suffix = key.strip_prefix("sdk-bulk-pglite-").unwrap();
        let (pid, nonce) = suffix.split_once('-').unwrap();
        assert!(!pid.is_empty() && !nonce.is_empty());
        assert!(pid.bytes().chain(nonce.bytes()).all(|b| b.is_ascii_digit()));
        let (c, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
            .await
            .unwrap();
        let task = tokio::spawn(connection);
        for table in [
            "mount_rs_inode_guards",
            "mount_rs_metadata",
            "mount_rs_blocks",
            "mount_rs_block_authority",
        ] {
            c.execute_typed(
                &format!("DELETE FROM {table} WHERE volume_key=$1"),
                &[(&key, Type::TEXT)],
            )
            .await
            .unwrap();
            let row = c
                .query_one(
                    &format!("SELECT COUNT(*) FROM {table} WHERE volume_key=$1"),
                    &[&key],
                )
                .await
                .unwrap();
            assert_eq!(row.get::<_, i64>(0), 0);
        }
        drop(c);
        task.await.unwrap().unwrap();
        println!("SDK_BULK_CLEANUP_PASS provider=pglite exact_owned_tables=4 residual_rows=0");
    }
}

fn late_unsigned_inode() -> Namespace {
    use mount_rs_core::storage::NodeData;
    let mut ns = fixture(131);
    let high = (i64::MAX as u64) + 1;
    let mut node = ns.nodes.remove(&131).unwrap();
    node.stats.ino = high;
    ns.nodes.insert(high, node);
    ns.next_inode = high + 1;
    let NodeData::Directory { entries } = &mut ns.nodes.get_mut(&1).unwrap().data else {
        unreachable!()
    };
    entries.iter_mut().find(|e| e.inode == 131).unwrap().inode = high;
    ns.validate().unwrap();
    ns
}

#[test]
#[ignore = "requires actual datastore"]
fn actual_guard_late_signed_overflow_rolls_back_enrollment_and_structure() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(actual_guard_late_signed_overflow_rolls_back_enrollment_and_structure_body());
}
async fn actual_guard_late_signed_overflow_rolls_back_enrollment_and_structure_body() {
    let url = std::env::var("PGLITE_DATABASE_URL").unwrap();
    for enrollment in [true, false] {
        let key = owned_key();
        let store = PgliteMetadataStore::connect_with_key(&url, &key)
            .await
            .unwrap();
        let backing = ConcurrentBackingId::from_bytes([9; 16]).unwrap();
        store.prepare_bound_concurrent_mode(backing).await.unwrap();
        let initial = if enrollment {
            late_unsigned_inode()
        } else {
            fixture(131)
        };
        store
            .publish_bound_if_revision(backing, 0, initial.clone())
            .await
            .unwrap();
        let control = Control::new(Action::Record);
        let error = if enrollment {
            CONTROL
                .scope(control.clone(), store.prepare_inode_mode(backing, 1))
                .await
                .unwrap_err()
        } else {
            store.prepare_inode_mode(backing, 1).await.unwrap();
            let before = store.load_inode_snapshot(backing).await.unwrap();
            CONTROL
                .scope(
                    control.clone(),
                    store.publish_structure_if_versions(
                        backing,
                        2,
                        &before.inode_revisions,
                        late_unsigned_inode(),
                    ),
                )
                .await
                .unwrap_err()
        };
        assert_eq!(error.code, ErrorCode::Eoverflow);
        assert_eq!(*control.rows.lock().unwrap(), [64, 64]);
        if enrollment {
            assert!(store.inode_mode_state().await.unwrap().is_none());
            let bound = store.load().await.unwrap();
            assert_eq!(bound.revision, 1);
            assert_eq!(
                serde_json::to_vec(&bound.namespace.unwrap()).unwrap(),
                serde_json::to_vec(&initial).unwrap()
            );
            // A second attempt reaches the same local overflow: no partial
            // enrollment guards survived the first transaction rollback.
            assert_eq!(
                store.prepare_inode_mode(backing, 1).await.unwrap_err().code,
                ErrorCode::Eoverflow
            );
        } else {
            let after = store.load_inode_snapshot(backing).await.unwrap();
            assert_eq!(after.structural_generation, 2);
            assert!(after.inode_revisions.values().all(|r| *r == 0));
            assert_eq!(
                serde_json::to_vec(&after.namespace).unwrap(),
                serde_json::to_vec(&initial).unwrap()
            );
        }
        store.close().await.unwrap();
        cleanup(&url, &key).await;
        println!(
            "GUARD_LATE_OVERFLOW_PASS provider=pglite enrollment={enrollment} prior_successful_batches=2 rollback=true"
        );
    }
}
