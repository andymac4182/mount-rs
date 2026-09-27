//! Actual provider protocol regression: record message tags and Parse type OIDs
//! only. SQL, credentials, parameter values and file bytes are never retained.
use super::*;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Clone, Debug, Default)]
struct GuardInsert {
    rows: usize,
    bytes: usize,
    tags: Vec<u8>,
    types: Vec<u32>,
    volume_bytes: usize,
    node_bytes: usize,
}

#[derive(Default)]
struct Trace {
    armed: AtomicBool,
    frames: StdMutex<Vec<(u8, Vec<u32>)>>,
    inserts: StdMutex<Vec<GuardInsert>>,
    order: StdMutex<Vec<String>>,
    drop_commit_ack: AtomicBool,
    commits: AtomicUsize,
    acks_dropped: AtomicUsize,
}
struct Proxy {
    url: String,
    trace: Arc<Trace>,
    worker: std::thread::JoinHandle<()>,
}
impl Proxy {
    fn new(url: &str) -> Self {
        let config: tokio_postgres::Config = url.parse().unwrap();
        let port = config.get_ports()[0];
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let local = listener.local_addr().unwrap().port();
        let trace = Arc::new(Trace::default());
        let worker = std::thread::spawn({
            let trace = Arc::clone(&trace);
            move || {
                let (mut client, _) = listener.accept().unwrap();
                let mut server = TcpStream::connect(("127.0.0.1", port)).unwrap();
                client.set_nodelay(true).unwrap();
                server.set_nodelay(true).unwrap();
                let response = std::thread::spawn({
                    let mut server = server.try_clone().unwrap();
                    let mut client = client.try_clone().unwrap();
                    let trace = Arc::clone(&trace);
                    move || {
                        let mut tag = [0];
                        let mut size = [0; 4];
                        let mut dropping_commit = false;
                        while server.read_exact(&mut tag).is_ok() {
                            server.read_exact(&mut size).unwrap();
                            let length = u32::from_be_bytes(size) as usize;
                            assert!((4..=8 * 1024 * 1024).contains(&length));
                            let mut body = vec![0; length - 4];
                            server.read_exact(&mut body).unwrap();
                            if tag[0] == b'C'
                                && body == b"COMMIT\0"
                                && trace.drop_commit_ack.swap(false, Ordering::SeqCst)
                            {
                                dropping_commit = true;
                            }
                            if dropping_commit {
                                if tag[0] == b'Z' {
                                    assert_eq!(body, b"I");
                                    trace.acks_dropped.fetch_add(1, Ordering::SeqCst);
                                    let _ = client.shutdown(Shutdown::Both);
                                    let _ = server.shutdown(Shutdown::Both);
                                    break;
                                }
                                continue;
                            }
                            if trace.armed.load(Ordering::SeqCst) {
                                let detail = if matches!(tag[0], b'Z' | b'C') {
                                    String::from_utf8(body.clone()).unwrap()
                                } else if tag[0] == b'E' {
                                    body.split(|b| *b == 0)
                                        .find(|field| field.first() == Some(&b'C'))
                                        .map(|field| String::from_utf8_lossy(field).into_owned())
                                        .unwrap_or_default()
                                } else {
                                    String::new()
                                };
                                trace.order.lock().unwrap().push(format!(
                                    "S:{}:{}",
                                    tag[0] as char,
                                    detail.trim_end_matches('\0')
                                ));
                            }
                            if client
                                .write_all(&tag)
                                .and_then(|_| client.write_all(&size))
                                .and_then(|_| client.write_all(&body))
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                });
                let mut size = [0; 4];
                client.read_exact(&mut size).unwrap();
                let mut startup = vec![0; u32::from_be_bytes(size) as usize];
                startup[..4].copy_from_slice(&size);
                client.read_exact(&mut startup[4..]).unwrap();
                server.write_all(&startup).unwrap();
                let mut tag = [0];
                let mut guard: Option<GuardInsert> = None;
                while client.read_exact(&mut tag).is_ok() {
                    client.read_exact(&mut size).unwrap();
                    let length = u32::from_be_bytes(size) as usize;
                    assert!((4..=1024 * 1024).contains(&length));
                    let mut body = vec![0; length - 4];
                    client.read_exact(&mut body).unwrap();
                    if trace.armed.load(Ordering::SeqCst) {
                        if tag[0] == b'Q' && body == b"COMMIT\0" {
                            trace.commits.fetch_add(1, Ordering::SeqCst);
                        }
                        let detail = if tag[0] == b'Q'
                            && [b"COMMIT\0".as_slice(), b"ROLLBACK\0".as_slice()]
                                .contains(&body.as_slice())
                        {
                            String::from_utf8(body.clone()).unwrap()
                        } else {
                            String::new()
                        };
                        trace.order.lock().unwrap().push(format!(
                            "C:{}:{}",
                            tag[0] as char,
                            detail.trim_end_matches('\0')
                        ));
                        let mut types = vec![];
                        if tag[0] == b'P' {
                            let after_name = body.iter().position(|b| *b == 0).unwrap() + 1;
                            let after_sql = after_name
                                + body[after_name..].iter().position(|b| *b == 0).unwrap()
                                + 1;
                            let count = u16::from_be_bytes(
                                body[after_sql..after_sql + 2].try_into().unwrap(),
                            ) as usize;
                            for oid in body[after_sql + 2..].chunks_exact(4).take(count) {
                                types.push(u32::from_be_bytes(oid.try_into().unwrap()));
                            }
                            assert_eq!(types.len(), count);
                            if body[after_name..after_sql - 1]
                                .starts_with(b"INSERT INTO mount_rs_inode_guards(")
                            {
                                guard = Some(GuardInsert {
                                    rows: count / 4,
                                    types: types.clone(),
                                    ..Default::default()
                                });
                            }
                        }
                        trace.frames.lock().unwrap().push((tag[0], types));
                        if let Some(insert) = &mut guard {
                            if tag[0] == b'B' {
                                let mut offset = 2;
                                let formats = u16::from_be_bytes(
                                    body[offset..offset + 2].try_into().unwrap(),
                                ) as usize;
                                offset += 2 + formats * 2;
                                let count = u16::from_be_bytes(
                                    body[offset..offset + 2].try_into().unwrap(),
                                ) as usize;
                                offset += 2;
                                assert_eq!(count, insert.rows * 4);
                                for param in 0..count {
                                    let len = i32::from_be_bytes(
                                        body[offset..offset + 4].try_into().unwrap(),
                                    );
                                    assert!(len >= 0);
                                    let len = len as usize;
                                    offset += 4;
                                    if param % 4 == 0 {
                                        insert.volume_bytes = len;
                                    }
                                    if param % 4 == 3 {
                                        insert.node_bytes += len;
                                    }
                                    offset += len;
                                }
                            }
                            insert.tags.push(tag[0]);
                            insert.bytes += length + 1;
                            if tag[0] == b'D' {
                                assert_eq!(length + 1, 7);
                                assert_eq!(body, b"S\0");
                            }
                        }
                        if tag[0] == b'S'
                            && let Some(insert) = guard.take()
                        {
                            trace.inserts.lock().unwrap().push(insert);
                        }
                    }
                    server.write_all(&tag).unwrap();
                    server.write_all(&size).unwrap();
                    server.write_all(&body).unwrap();
                }
                let _ = server.shutdown(Shutdown::Write);
                response.join().unwrap();
            }
        });
        // This fixture uses the repository-owned local postgres credentials.
        Self {
            url: format!(
                "postgresql://postgres:postgres@127.0.0.1:{local}/postgres?sslmode=disable"
            ),
            trace,
            worker,
        }
    }
}

#[test]
#[ignore = "requires actual PGlite and PGLITE_DATABASE_URL"]
fn inode_queries_send_explicit_types_without_statement_close_roundtrips() {
    let url = std::env::var("PGLITE_DATABASE_URL").unwrap();
    let proxy = Proxy::new(&url);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let volume = format!("inode-wire-{}", uuid::Uuid::new_v4());
        let metadata = PgliteMetadataStore::connect_with_key(&proxy.url, &volume)
            .await
            .unwrap();
        let blocks = PgliteBlockStore::connect_with_key(&url, &volume)
            .await
            .unwrap();
        let backing = blocks.prepare_concurrent_backing().await.unwrap();
        metadata
            .prepare_bound_concurrent_mode(backing)
            .await
            .unwrap();
        let namespace = super::tests::namespace(blocks.put(b"abc").await.unwrap()).await;
        metadata
            .publish_bound_if_revision(backing, 0, namespace.clone())
            .await
            .unwrap();
        proxy.trace.armed.store(true, Ordering::SeqCst);
        metadata.prepare_inode_mode(backing, 1).await.unwrap();
        assert_eq!(
            metadata
                .inode_mode_state()
                .await
                .unwrap()
                .unwrap()
                .structural_generation,
            2
        );
        let inode = namespace.root + 1;
        let file = metadata.load_inode(backing, inode).await.unwrap();
        assert!(
            metadata
                .load_inode_if_changed(backing, inode, Some(file.version))
                .await
                .unwrap()
                .is_none()
        );
        let wrong = ConcurrentBackingId::from_bytes([9; 16]).unwrap();
        assert_eq!(
            metadata
                .load_inode_if_changed(wrong, inode, Some(file.version))
                .await
                .unwrap_err()
                .code,
            ErrorCode::Estale
        );
        // A large signed revision must retain INT8 encoding and exact CAS, not
        // narrow to INT4 or wrap when the next revision reaches its boundary.
        let large = i64::MAX - 1;
        {
            let client = metadata.0.lock_client().await.unwrap();
            client
                .as_ref()
                .unwrap()
                .execute_typed(
                    "UPDATE mount_rs_inode_guards SET revision=$3 WHERE volume_key=$1 AND inode=$2",
                    &[
                        (&volume, Type::TEXT),
                        (&(inode as i64), Type::INT8),
                        (&large, Type::INT8),
                    ],
                )
                .await
                .unwrap();
        }
        let file = metadata.load_inode(backing, inode).await.unwrap();
        assert_eq!(file.version.inode_revision, large as u64);
        let mut changed = file.node.clone();
        changed.stats.mtime_ms += 1;
        let version = metadata
            .publish_inode_if_version(backing, inode, file.version, changed.clone())
            .await
            .unwrap();
        assert_eq!(version.inode_revision, i64::MAX as u64);
        assert_eq!(
            metadata
                .publish_inode_if_version(backing, inode, file.version, changed.clone())
                .await
                .unwrap_err()
                .code,
            ErrorCode::Eagain
        );
        assert_eq!(
            metadata
                .publish_inode_if_version(backing, inode, version, changed)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Eoverflow
        );
        let snapshot = metadata.load_inode_snapshot(backing).await.unwrap();
        assert_eq!(
            metadata
                .publish_structure_if_versions(
                    wrong,
                    snapshot.structural_generation,
                    &snapshot.inode_revisions,
                    snapshot.namespace.clone()
                )
                .await
                .unwrap_err()
                .code,
            ErrorCode::Estale
        );
        assert_eq!(
            metadata
                .publish_structure_if_versions(
                    backing,
                    snapshot.structural_generation,
                    &snapshot.inode_revisions,
                    snapshot.namespace
                )
                .await
                .unwrap(),
            3
        );
        assert_eq!(
            metadata
                .load_inode(backing, inode)
                .await
                .unwrap()
                .version
                .inode_revision,
            0
        );
        // Even an unchanged token must reject a removed authority fence.
        let version = metadata.load_inode(backing, inode).await.unwrap().version;
        {
            let client = metadata.0.lock_client().await.unwrap();
            client
                .as_ref()
                .unwrap()
                .execute_typed(
                    "UPDATE mount_rs_metadata SET fence=0 WHERE volume_key=$1",
                    &[(&volume, Type::TEXT)],
                )
                .await
                .unwrap();
        }
        assert_eq!(
            metadata
                .load_inode_if_changed(backing, inode, Some(version))
                .await
                .unwrap_err()
                .code,
            ErrorCode::Estale
        );
        metadata.close().await.unwrap();
        blocks.close().await.unwrap();
        // Delete only this test's owned volume rows after provider shutdown.
        let (client, connection) = tokio_postgres::connect(&url, NoTls).await.unwrap();
        let task = tokio::spawn(async move {
            connection.await.unwrap();
        });
        for table in [
            "mount_rs_inode_guards",
            "mount_rs_metadata",
            "mount_rs_blocks",
            "mount_rs_block_authority",
        ] {
            client
                .execute_typed(
                    &format!("DELETE FROM {table} WHERE volume_key=$1"),
                    &[(&volume, Type::TEXT)],
                )
                .await
                .unwrap();
        }
        drop(client);
        task.await.unwrap();
    });
    proxy.worker.join().unwrap();
    let frames = proxy.trace.frames.lock().unwrap();
    let parses: Vec<_> = frames
        .iter()
        .filter(|(tag, _)| *tag == b'P')
        .map(|(_, types)| types)
        .collect();
    assert!(!parses.is_empty());
    assert!(
        parses
            .iter()
            .all(|types| !types.is_empty() && types.iter().all(|oid| [20, 25].contains(oid))),
        "all inode parameters must use explicit TEXT/INT8 OIDs: {parses:?}"
    );
    assert_eq!(
        frames.iter().filter(|(tag, _)| *tag == b'C').count(),
        0,
        "typed inode operations must not close prepared statements"
    );
    assert_eq!(
        frames.iter().filter(|(tag, _)| *tag == b'S').count(),
        parses.len(),
        "each typed statement needs one Sync"
    );
}

mod batch_fixture {
    use mount_rs_core::chunking::{Chunker, FixedSizeChunker};
    use mount_rs_core::storage::{DirectoryEntry, FileLayout, Namespace, NodeData, NodeMetadata};
    use mount_rs_core::{S_IFDIR, S_IFREG, Stats};
    use std::collections::BTreeMap;
    const NODES: usize = 131;
    pub(super) fn batch_namespace() -> Namespace {
        let chunker = FixedSizeChunker::new(4096).unwrap().config();
        let root_stats = Stats {
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
        let mut nodes = BTreeMap::new();
        let mut entries = Vec::new();
        for inode in 2..=NODES as u64 {
            let mut stats = root_stats.clone();
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
                stats: root_stats,
                data: NodeData::Directory { entries },
            },
        );
        Namespace {
            format_version: 1,
            root: 1,
            next_inode: NODES as u64 + 1,
            default_uid: 0,
            default_gid: 0,
            umask: 0o022,
            default_chunker: chunker,
            nodes,
        }
    }
}

#[test]
#[ignore = "requires actual PGlite and PGLITE_DATABASE_URL"]
fn structural_guard_values_statement_count() {
    let url = std::env::var("PGLITE_DATABASE_URL").unwrap();
    let proxy = Proxy::new(&url);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let windows = runtime.block_on(async {
        let volume = format!("guard-count-{}-é🗃", uuid::Uuid::new_v4());
        let metadata = PgliteMetadataStore::connect_with_key(&proxy.url, &volume)
            .await
            .unwrap();
        let direct = PgliteMetadataStore::connect_with_key(&url, &volume)
            .await
            .unwrap();
        let blocks = PgliteBlockStore::connect_with_key(&url, &volume)
            .await
            .unwrap();
        let backing = blocks.prepare_concurrent_backing().await.unwrap();
        metadata
            .prepare_bound_concurrent_mode(backing)
            .await
            .unwrap();
        let bytes: Vec<u8> = (2_u64..=131)
            .flat_map(|inode| (0_u64..4).flat_map(move |part| (inode * 4 + part).to_le_bytes()))
            .collect();
        let block = blocks.put(&bytes).await.unwrap();
        let mut expected = batch_fixture::batch_namespace();
        for (&inode, node) in expected.nodes.iter_mut().filter(|(id, _)| **id != 1) {
            node.stats.size = 32;
            node.stats.blocks = 1;
            let mount_rs_core::storage::NodeData::File(layout) = &mut node.data else {
                unreachable!()
            };
            layout.extents.push(mount_rs_core::storage::BlockExtent {
                file_offset: 0,
                block: block.clone(),
                block_offset: (inode - 2) * 32,
                length: 32,
            });
        }
        metadata
            .publish_bound_if_revision(backing, 0, expected.clone())
            .await
            .unwrap();
        proxy.trace.armed.store(true, Ordering::SeqCst);
        metadata.prepare_inode_mode(backing, 1).await.unwrap();
        proxy.trace.armed.store(false, Ordering::SeqCst);
        let mut windows = vec![(
            "enrollment",
            std::mem::take(&mut *proxy.trace.inserts.lock().unwrap()),
        )];
        for generation in [2, 3] {
            let old = direct.load_inode_snapshot(backing).await.unwrap();
            expected.nodes.get_mut(&2).unwrap().stats.mtime_ms = generation as i64;
            proxy.trace.armed.store(true, Ordering::SeqCst);
            metadata
                .publish_structure_if_versions(
                    backing,
                    generation,
                    &old.inode_revisions,
                    expected.clone(),
                )
                .await
                .unwrap();
            proxy.trace.armed.store(false, Ordering::SeqCst);
            windows.push((
                if generation == 2 {
                    "structure-first"
                } else {
                    "structure-repeat"
                },
                std::mem::take(&mut *proxy.trace.inserts.lock().unwrap()),
            ));
        }
        let actual = direct.load_inode_snapshot(backing).await.unwrap();
        assert_eq!(actual.structural_generation, 4);
        assert_eq!(actual.inode_revisions.len(), 131);
        assert!(actual.inode_revisions.values().all(|v| *v == 0));
        assert_eq!(
            serde_json::to_vec(&actual.namespace).unwrap(),
            serde_json::to_vec(&expected).unwrap()
        );
        let read = blocks.get(&block).await.unwrap();
        assert_eq!(read, bytes);
        for inode in 2..=131 {
            let start = (inode - 2) * 32;
            assert_eq!(&read[start..start + 32], &bytes[start..start + 32]);
        }
        println!(
            "GUARD_ORACLE_PASS provider=pglite nodes=131 files=130 all_file_bytes=4160 generation=4"
        );
        metadata.close().await.unwrap();
        direct.close().await.unwrap();
        blocks.close().await.unwrap();
        let (client, connection) = tokio_postgres::connect(&url, NoTls).await.unwrap();
        let task = tokio::spawn(connection);
        for table in [
            "mount_rs_inode_guards",
            "mount_rs_metadata",
            "mount_rs_blocks",
            "mount_rs_block_authority",
        ] {
            client
                .execute_typed(
                    &format!("DELETE FROM {table} WHERE volume_key=$1"),
                    &[(&volume, Type::TEXT)],
                )
                .await
                .unwrap();
        }
        drop(client);
        task.await.unwrap().unwrap();
        windows
    });
    proxy.worker.join().unwrap();
    for (phase, inserts) in &windows {
        println!(
            "GUARD_WIRE provider=pglite phase={phase} executes={} rows={:?} frontend_bytes={}",
            inserts.len(),
            inserts.iter().map(|i| i.rows).collect::<Vec<_>>(),
            inserts.iter().map(|i| i.bytes).sum::<usize>()
        );
        for insert in inserts {
            let bound = super::inode_batch::encoded_bound(
                insert.rows,
                insert.volume_bytes,
                insert.node_bytes,
            )
            .unwrap();
            assert!(insert.bytes <= bound);
            assert!(bound <= 256 * 1024);
            assert_eq!(insert.tags, b"PBDES");
            assert_eq!(insert.types, [25, 20, 20, 25].repeat(insert.rows));
        }
    }
    assert!(
        !proxy
            .trace
            .frames
            .lock()
            .unwrap()
            .iter()
            .any(|(tag, _)| *tag == b'C')
    );
    for (_, inserts) in windows {
        assert_eq!(
            inserts.iter().map(|i| i.rows).collect::<Vec<_>>(),
            [64, 64, 3]
        );
    }
}

#[test]
#[ignore = "requires actual PGlite and PGLITE_DATABASE_URL"]
fn actual_singleton_typed_duplicate_rollback_protocol_control() {
    let url = std::env::var("PGLITE_DATABASE_URL").unwrap();
    let proxy = Proxy::new(&url);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
 let key=format!("singleton-error-control-{}",uuid::Uuid::new_v4());
 let store=PgliteMetadataStore::connect_with_key(&proxy.url,&key).await.unwrap();
 let backing=ConcurrentBackingId::from_bytes([9;16]).unwrap();store.prepare_bound_concurrent_mode(backing).await.unwrap();
 let ns=batch_fixture::batch_namespace();store.publish_bound_if_revision(backing,0,ns.clone()).await.unwrap();store.prepare_inode_mode(backing,1).await.unwrap();
 {
 let mut client=store.0.lock_client().await.unwrap();let client=client.as_mut().unwrap();
 let tx=client.transaction().await.unwrap();
 proxy.trace.armed.store(true,Ordering::SeqCst);
 let node=encode_inode_node(&ns.nodes[&1]).unwrap();
 let error=tx.execute_typed("INSERT INTO mount_rs_inode_guards(volume_key,inode,generation,revision,node) VALUES($1,$2,$3,0,$4)",&[(&key,Type::TEXT),(&1_i64,Type::INT8),(&2_i64,Type::INT8),(&node,Type::TEXT)]).await.unwrap_err();
 println!("PG_SINGLETON_SQL_ERROR code={:?}",error.code().map(|code|code.code()));
 drop(tx);
 let reused=client.query_typed_one("SELECT revision FROM mount_rs_metadata WHERE volume_key=$1",&[(&key,Type::TEXT)]).await;
 println!("PG_SINGLETON_AFTER_ROLLBACK typed_read_ok={}",reused.is_ok());
 }
 let fresh=PgliteMetadataStore::connect_with_key(&url,&key).await.unwrap();
 let snapshot=fresh.load_inode_snapshot(backing).await.unwrap();assert_eq!(snapshot.structural_generation,2);
 assert_eq!(serde_json::to_vec(&snapshot.namespace).unwrap(),serde_json::to_vec(&ns).unwrap());
 println!("PG_SINGLETON_FRESH_ORACLE_PASS generation=2 nodes=131");
 fresh.close().await.unwrap();store.close().await.unwrap();
 let (client,connection)=tokio_postgres::connect(&url,NoTls).await.unwrap();let task=tokio::spawn(connection);
 for table in ["mount_rs_inode_guards","mount_rs_metadata"]{client.execute_typed(&format!("DELETE FROM {table} WHERE volume_key=$1"),&[(&key,Type::TEXT)]).await.unwrap();}
 drop(client);task.await.unwrap().unwrap();
 });
    proxy.worker.join().unwrap();
    println!(
        "PG_SINGLETON_WIRE_ORDER {:?}",
        proxy.trace.order.lock().unwrap()
    );
}

#[test]
#[ignore = "requires actual PGlite and PGLITE_DATABASE_URL"]
fn actual_pglite_multibatch_structural_commit_ack_is_not_replayed() {
    let url = std::env::var("PGLITE_DATABASE_URL").unwrap();
    let proxy = Proxy::new(&url);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
 let key=format!("guard-ack-{}",uuid::Uuid::new_v4());let store=PgliteMetadataStore::connect_with_key(&proxy.url,&key).await.unwrap();let direct=PgliteMetadataStore::connect_with_key(&url,&key).await.unwrap();let backing=ConcurrentBackingId::from_bytes([6;16]).unwrap();store.prepare_bound_concurrent_mode(backing).await.unwrap();let mut expected=batch_fixture::batch_namespace();store.publish_bound_if_revision(backing,0,expected.clone()).await.unwrap();store.prepare_inode_mode(backing,1).await.unwrap();let before=direct.load_inode_snapshot(backing).await.unwrap();expected.nodes.get_mut(&2).unwrap().stats.mtime_ms=777;
 proxy.trace.armed.store(true,Ordering::SeqCst);proxy.trace.drop_commit_ack.store(true,Ordering::SeqCst);
 let error=tokio::time::timeout(std::time::Duration::from_secs(10),store.publish_structure_if_versions(backing,2,&before.inode_revisions,expected.clone())).await.unwrap().unwrap_err();assert_eq!(error.code,ErrorCode::Eio);
 proxy.trace.armed.store(false,Ordering::SeqCst);
 assert_eq!(proxy.trace.inserts.lock().unwrap().iter().map(|i|i.rows).collect::<Vec<_>>(),[64,64,3]);assert_eq!(proxy.trace.commits.load(Ordering::SeqCst),1);assert_eq!(proxy.trace.acks_dropped.load(Ordering::SeqCst),1);
 let after=direct.load_inode_snapshot(backing).await.unwrap();assert_eq!(after.structural_generation,3);assert_eq!(after.inode_revisions.len(),131);assert!(after.inode_revisions.values().all(|r|*r==0));assert_eq!(serde_json::to_vec(&after.namespace).unwrap(),serde_json::to_vec(&expected).unwrap());
 store.close().await.unwrap();direct.close().await.unwrap();let(client,connection)=tokio_postgres::connect(&url,NoTls).await.unwrap();let task=tokio::spawn(connection);for table in ["mount_rs_inode_guards","mount_rs_metadata"] {client.execute_typed(&format!("DELETE FROM {table} WHERE volume_key=$1"),&[(&key,Type::TEXT)]).await.unwrap();}drop(client);task.await.unwrap().unwrap();
 println!("GUARD_LOST_ACK_PASS provider=pglite inserts=[64,64,3] commit_sends=1 committed_ready_dropped=1 fresh_generation=3 exact_namespace=true replay=false");
 });
    proxy.worker.join().unwrap();
}
