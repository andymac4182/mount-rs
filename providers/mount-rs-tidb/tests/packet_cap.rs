//! Prerequisite evidence for bounded structural INSERT batching, not a bulk test.
//! Actual TiDB only. No GLOBAL settings are changed; every stored row has an
//! owned unique key. Connection strings and parameter data are never logged.
use mount_rs_core::chunking::{Chunker, FixedSizeChunker};
use mount_rs_core::storage::{
    BlockExtent, BlockStore, DirectoryEntry, FileLayout, MetadataStore, Namespace, NodeData,
    NodeMetadata, encode_inode_namespace,
};
use mount_rs_core::{S_IFDIR, S_IFREG, Stats};
use mount_rs_tidb::{TidbBlockStore, TidbMetadataStore, TidbPoolContext, TidbStorageOptions};
use mysql_async::prelude::Queryable;
use mysql_async::{Conn, Error, IoError, Opts, OptsBuilder, Pool, PoolConstraints};
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const CLIENT_CAP: usize = 64 * 1024;
const NODES: usize = 131;

// Deliberately do not format driver/provider errors: they may include a URL.
fn checked<T, E>(result: Result<T, E>, operation: &str) -> T {
    result.unwrap_or_else(|_| panic!("{operation} failed (details suppressed)"))
}

fn url() -> String {
    std::env::var("MOUNT_RS_TIDB_URL").expect("MOUNT_RS_TIDB_URL must be set")
}

fn opts(url: &str, cap: Option<usize>) -> OptsBuilder {
    let base = checked(Opts::from_url(url), "parse fixture URL");
    let pool = base
        .pool_opts()
        .clone()
        .with_reset_connection(false)
        .with_constraints(PoolConstraints::new(1, 1).unwrap());
    OptsBuilder::from_opts(base)
        .pool_opts(pool)
        .max_allowed_packet(cap)
}

#[derive(Debug, PartialEq, Eq)]
struct CapObservation {
    connection_id: u64,
    session: u64,
    configured: Option<usize>,
}

async fn observe(connection: &mut Conn) -> CapObservation {
    let (connection_id, session): (u64, u64) = checked(
        connection
            .query_first("SELECT CONNECTION_ID(), @@SESSION.max_allowed_packet")
            .await,
        "read session cap",
    )
    .expect("session cap row");
    assert!(session > 0);
    CapObservation {
        connection_id,
        session,
        configured: connection.opts().max_allowed_packet(),
    }
}

async fn identity(connection: &mut Conn) {
    let version: String = checked(
        connection.query_first("SELECT VERSION()").await,
        "read version",
    )
    .expect("version row");
    assert!(version.to_ascii_lowercase().contains("tidb"));
    // VERSION is a server identity, never an endpoint/credential.
    println!("PACKET_CAP_IDENTITY version={version}");
}

async fn assert_read_only(connection: &mut Conn, before: &CapObservation) {
    // Attempt only the already-current value: even an unexpected success
    // cannot lower/raise the session cap or mutate any GLOBAL setting.
    let result = connection
        .query_drop(format!("SET SESSION max_allowed_packet={}", before.session))
        .await;
    let Err(Error::Server(error)) = result else {
        panic!("same-value SESSION assignment must return a read-only server error");
    };
    let message = error.message.to_ascii_lowercase();
    assert!(message.contains("read only") || message.contains("read-only"));
    println!("PACKET_CAP_READ_ONLY server_error_code={}", error.code);
    assert_eq!(&observe(connection).await, before);
}

#[tokio::test]
#[ignore = "requires actual TiDB and MOUNT_RS_TIDB_URL"]
async fn actual_tidb_packet_cap_session_is_read_only_across_reuse_and_reconnect() {
    checked(
        tokio::time::timeout(Duration::from_secs(60), async {
            for configured in [None, Some(CLIENT_CAP)] {
                let pool = Pool::new(opts(&url(), configured));
                let mut connection = checked(pool.get_conn().await, "open fresh cap session");
                identity(&mut connection).await;
                let fresh = observe(&mut connection).await;
                assert_eq!(fresh.configured, configured);
                assert!(fresh.session >= CLIENT_CAP as u64, "fixture cap too small");
                assert_read_only(&mut connection, &fresh).await;
                checked(
                    connection.query_drop("SET @mount_rs_packet_cap_probe=731").await,
                    "set owned session marker",
                );
                drop(connection);
                // With one retained connection, get_conn waits for recycling;
                // ID and marker prove reuse without relying on a sleep.
                let mut connection = checked(pool.get_conn().await, "reuse cap session");
                let reused = observe(&mut connection).await;
                assert_eq!(reused, fresh);
                let marker: Option<u64> = checked(
                    connection.query_first("SELECT @mount_rs_packet_cap_probe").await,
                    "read retained session marker",
                ).flatten();
                assert_eq!(marker, Some(731));
                checked(connection.disconnect().await, "disconnect owned cap session");
                let mut connection = checked(pool.get_conn().await, "reconnect cap session");
                let reconnected = observe(&mut connection).await;
                assert_ne!(reconnected.connection_id, fresh.connection_id);
                assert_eq!(reconnected.configured, configured);
                assert_eq!(reconnected.session, fresh.session);
                let marker: Option<u64> = checked(
                    connection.query_first("SELECT @mount_rs_packet_cap_probe").await,
                    "read new session marker",
                ).flatten();
                assert_eq!(marker, None);
                assert_read_only(&mut connection, &reconnected).await;
                println!("PACKET_CAP_SESSIONS fresh={fresh:?} reused={reused:?} reconnected={reconnected:?}");
                checked(connection.disconnect().await, "close reconnected session");
                // A read-only prepared parameter probe distinguishes the
                // actual driver cap from merely observing an options field.
                let mut connection = checked(pool.get_conn().await, "open packet bound probe");
                let probe: Result<Option<u64>, Error> = connection
                    .exec_first("SELECT OCTET_LENGTH(?)", (vec![b'x'; CLIENT_CAP + 1],))
                    .await;
                let category = match &probe {
                    Ok(_) => "accepted",
                    Err(Error::Driver(_)) => "driver",
                    Err(Error::Io(_)) => "io",
                    Err(Error::Server(_)) => "server",
                    Err(Error::Other(_)) => "other",
                    Err(Error::Url(_)) => "url",
                };
                println!("PACKET_CAP_PARAMETER_PROBE configured={configured:?} result_category={category}");
                if configured.is_some() {
                    // Codec errors are wrapped as Io(Other), not the similarly
                    // named DriverError variant, in pinned mysql_async 0.37.1.
                    assert!(matches!(probe, Err(Error::Io(IoError::Io(ref error)))
                        if error.kind() == std::io::ErrorKind::Other
                            && error.to_string() == "Packet is larger than max_allowed_packet"));
                    // Fatal packet rejection discards this connection; no
                    // statement or mutation is replayed on another session.
                    drop(connection);
                    println!("PACKET_CAP_BOUND explicit_client_cap={CLIENT_CAP} oversized_parameter_rejected=true");
                } else {
                    assert_eq!(checked(probe, "uncapped parameter control"), Some(CLIENT_CAP as u64 + 1));
                    checked(connection.disconnect().await, "close packet control session");
                    println!("PACKET_CAP_BOUND configured_cap=none same_parameter_accepted=true");
                }
                checked(pool.disconnect().await, "close cap pool");
            }
        }).await,
        "session cap timeout",
    );
}

fn namespace() -> Namespace {
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

async fn publication_case(url: &str, key: &str, shared: bool) {
    let options = TidbStorageOptions::new(key);
    let context = shared.then(|| checked(TidbPoolContext::new(url, 1), "create provider context"));
    let (metadata, blocks) = if let Some(context) = &context {
        (
            checked(
                context.metadata(options.clone()).await,
                "open shared metadata",
            ),
            checked(context.blocks(options).await, "open shared blocks"),
        )
    } else {
        (
            checked(
                TidbMetadataStore::connect_with_options(url, options.clone()).await,
                "open private metadata",
            ),
            checked(
                TidbBlockStore::connect_with_options(url, options).await,
                "open private blocks",
            ),
        )
    };
    let backing = checked(
        blocks.prepare_concurrent_backing().await,
        "prepare actual backing",
    );
    checked(
        metadata.prepare_bound_concurrent_mode(backing).await,
        "prepare metadata binding",
    );
    let payload: Vec<u8> = (0_u32..1024).flat_map(u32::to_le_bytes).collect();
    let block = checked(blocks.put(&payload).await, "put exact oracle block");
    let mut expected = namespace();
    let file = expected.nodes.get_mut(&2).unwrap();
    file.stats.size = payload.len() as u64;
    file.stats.blocks = 8;
    let NodeData::File(layout) = &mut file.data else {
        unreachable!()
    };
    layout.extents.push(BlockExtent {
        file_offset: 0,
        block: block.clone(),
        block_offset: 0,
        length: payload.len() as u64,
    });
    checked(expected.validate(), "validate baseline namespace");
    let root_bytes = checked(encode_inode_namespace(&expected), "encode baseline root").len();
    // Generous overhead allowance covers the existing bound root UPDATE's
    // key, generation, JSON length prefix, type/null metadata and SQL prepare.
    let root_packet_bound = root_bytes + key.len() + 1024;
    assert!(
        root_packet_bound < CLIENT_CAP,
        "old root UPDATE must fit client cap"
    );
    assert!(
        key.len() * NODES > CLIENT_CAP,
        "repeated keys alone exceed hypothetical all-row packet"
    );
    checked(
        metadata
            .publish_bound_if_revision(backing, 0, expected.clone())
            .await,
        "publish MRC2 singleton baseline",
    );
    checked(
        metadata.prepare_inode_mode(backing, 1).await,
        "enroll 131 singleton guards",
    );
    let initial = checked(
        metadata.load_inode_snapshot(backing).await,
        "read enrolled singleton guards",
    );
    assert_eq!(initial.structural_generation, 2);
    assert!(
        initial
            .inode_revisions
            .values()
            .all(|revision| *revision == 0)
    );
    assert_eq!(initial.inode_revisions.len(), NODES);
    // Change one node and a directory name without changing cardinality.
    expected.nodes.get_mut(&2).unwrap().stats.mode = S_IFREG | 0o600;
    expected.nodes.get_mut(&2).unwrap().stats.mtime_ms = 77;
    let NodeData::Directory { entries } = &mut expected.nodes.get_mut(&1).unwrap().data else {
        unreachable!()
    };
    entries[0].name = "renamed".into();
    assert!(
        checked(encode_inode_namespace(&expected), "encode changed root").len() + key.len() + 1024
            < CLIENT_CAP
    );
    assert_eq!(
        checked(
            metadata
                .publish_structure_if_versions(
                    backing,
                    2,
                    &initial.inode_revisions,
                    expected.clone()
                )
                .await,
            "publish structural singleton replacement"
        ),
        3
    );
    checked(metadata.close().await, "close metadata");
    checked(blocks.close().await, "close blocks");
    if let Some(context) = context {
        checked(context.close().await, "close provider context");
    }

    // Fresh public-provider connections verify persisted state, not old handles.
    let metadata = checked(
        TidbMetadataStore::connect_with_options(url, TidbStorageOptions::new(key)).await,
        "reopen metadata",
    );
    let blocks = checked(
        TidbBlockStore::connect_with_options(url, TidbStorageOptions::new(key)).await,
        "reopen blocks",
    );
    let actual = checked(
        metadata.load_inode_snapshot(backing).await,
        "fresh namespace oracle",
    );
    assert_eq!(actual.structural_generation, 3);
    assert_eq!(actual.inode_revisions.len(), NODES);
    assert!(
        actual
            .inode_revisions
            .values()
            .all(|revision| *revision == 0)
    );
    assert_eq!(
        checked(serde_json::to_vec(&actual.namespace), "encode actual"),
        checked(serde_json::to_vec(&expected), "encode expected")
    );
    assert_eq!(
        checked(blocks.get(&block).await, "fresh full block oracle"),
        payload
    );
    checked(metadata.close().await, "close oracle metadata");
    checked(blocks.close().await, "close oracle blocks");
    println!(
        "PACKET_CAP_SINGLETON_PASS shared={shared} nodes={NODES} volume_chars={} volume_bytes={} client_cap={CLIENT_CAP} root_bytes={root_bytes} root_packet_bound={root_packet_bound} repeated_volume_bytes={} byte_oracle=4096 generation=3",
        key.chars().count(),
        key.len(),
        key.len() * NODES
    );
}

#[tokio::test]
#[ignore = "requires actual TiDB and MOUNT_RS_TIDB_URL"]
async fn actual_tidb_low_client_cap_accepts_singleton_utf8_inode_publication() {
    let mut endpoint = checked(url::Url::parse(&url()), "parse low-cap URL");
    let query: Vec<(String, String)> = endpoint
        .query_pairs()
        .filter(|(name, _)| name != "max_allowed_packet")
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect();
    endpoint.set_query(None);
    endpoint
        .query_pairs_mut()
        .extend_pairs(query)
        .append_pair("max_allowed_packet", &CLIENT_CAP.to_string());
    let endpoint = endpoint.to_string();
    // Verify this URL actually passes the requested cap to the public provider.
    assert_eq!(
        checked(Opts::from_url(&endpoint), "parse provider cap option").max_allowed_packet(),
        Some(CLIENT_CAP)
    );
    let mut identity_connection = checked(
        Conn::new(opts(&endpoint, Some(CLIENT_CAP))).await,
        "open provider identity session",
    );
    identity(&mut identity_connection).await;
    checked(
        identity_connection.disconnect().await,
        "close provider identity session",
    );
    for shared in [false, true] {
        let prefix = format!(
            "cap-{}-{}-",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let key = format!("{prefix}{}", "🗃".repeat(255 - prefix.chars().count()));
        assert_eq!(key.chars().count(), 255);
        let task = tokio::spawn({
            let endpoint = endpoint.clone();
            let key = key.clone();
            async move {
                tokio::time::timeout(
                    Duration::from_secs(90),
                    publication_case(&endpoint, &key, shared),
                )
                .await
            }
        });
        let outcome = task.await;
        // Cleanup is outside the spawned assertion scope so it also runs on a
        // failed oracle. Only this test's exact key is affected.
        let pool = Pool::new(opts(&endpoint, Some(CLIENT_CAP)));
        let mut connection = checked(pool.get_conn().await, "open cleanup connection");
        identity(&mut connection).await;
        for table in [
            "mount_rs_tidb_inodes",
            "mount_rs_tidb_metadata",
            "mount_rs_tidb_blocks",
            "mount_rs_tidb_block_authority",
        ] {
            checked(
                connection
                    .exec_drop(format!("DELETE FROM {table} WHERE volume_key=?"), (&key,))
                    .await,
                "remove owned fixture rows",
            );
            let remaining: u64 = checked(
                connection
                    .exec_first(
                        format!("SELECT COUNT(*) FROM {table} WHERE volume_key=?"),
                        (&key,),
                    )
                    .await,
                "check owned fixture cleanup",
            )
            .expect("count row");
            assert_eq!(remaining, 0);
        }
        checked(connection.disconnect().await, "close cleanup connection");
        checked(pool.disconnect().await, "close cleanup pool");
        println!("PACKET_CAP_CLEANUP_PASS shared={shared} owned_tables=4");
        checked(
            checked(outcome, "singleton publication task"),
            "singleton publication timeout",
        );
    }
}
