//! Actual provider protocol regression: record message tags and Parse type OIDs
//! only. SQL, credentials, parameter values and file bytes are never retained.
use super::*;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Default)]
struct Trace {
    armed: AtomicBool,
    frames: StdMutex<Vec<(u8, Vec<u32>)>>,
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
                    move || std::io::copy(&mut server, &mut client).unwrap()
                });
                let mut size = [0; 4];
                client.read_exact(&mut size).unwrap();
                let mut startup = vec![0; u32::from_be_bytes(size) as usize];
                startup[..4].copy_from_slice(&size);
                client.read_exact(&mut startup[4..]).unwrap();
                server.write_all(&startup).unwrap();
                let mut tag = [0];
                while client.read_exact(&mut tag).is_ok() {
                    client.read_exact(&mut size).unwrap();
                    let length = u32::from_be_bytes(size) as usize;
                    assert!((4..=1024 * 1024).contains(&length));
                    let mut body = vec![0; length - 4];
                    client.read_exact(&mut body).unwrap();
                    if trace.armed.load(Ordering::SeqCst) {
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
                        }
                        trace.frames.lock().unwrap().push((tag[0], types));
                    }
                    server.write_all(&tag).unwrap();
                    server.write_all(&size).unwrap();
                    server.write_all(&body).unwrap();
                }
                server.shutdown(Shutdown::Write).unwrap();
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
