//! A writer publishing while every TiDB component is killed at once.
//!
//! `scripts/test-tidb.sh` (with `MOUNT_RS_TIDB_CRASH=1`) runs the two tests
//! here around a crash it causes itself:
//!
//! 1. `actual_tidb_crash_writer_publishes_until_the_cluster_is_killed` takes
//!    the writer lease and publishes one new file per publication, each file
//!    pointing at an immutable block, as fast as it can. It records every
//!    publication it starts and every one TiDB acknowledges in a journal, and
//!    creates a `ready` file after the first few. The harness then sends
//!    SIGKILL to every PD, TiKV and TiDB container in one `docker kill`. The
//!    writer stops at the first error, without retrying, and records it.
//! 2. The harness starts every component again, and
//!    `actual_tidb_crash_survivors_match_the_acknowledged_publications`
//!    checks the restarted cluster against the journal: every acknowledged
//!    publication is there, the one in flight is there completely or not at
//!    all, every block the metadata names can be read, and the dead writer's
//!    lease is fenced once a new writer has taken over.
//!
//! This is a crash of every process, not a power loss. The containers' page
//! cache and the host's disks survive a SIGKILL, so nothing here shows what
//! happens to data a component had written but not yet fsynced.

use mount_rs_core::chunking::{Chunker, FixedSizeChunker};
use mount_rs_core::storage::{
    BlockExtent, BlockId, BlockStore, DirectoryEntry, FileLayout, MetadataStore, Namespace,
    NodeData, NodeMetadata, WriterLease,
};
use mount_rs_core::{ErrorCode, FsDriver, FsError, S_IFDIR, S_IFMT, S_IFREG};
use mount_rs_memfs::MemoryFs;
use mount_rs_tidb::{TidbBlockStore, TidbMetadataStore, TidbStorageOptions};
use mysql_async::Pool;
use mysql_async::prelude::Queryable;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const BLOCK_BYTES: usize = 4096;
const DISTINCT_BLOCKS: u64 = 8;
/// Publications acknowledged before the harness is told to kill the cluster.
const READY_AFTER: u64 = 20;
const LEASE_TTL: Duration = Duration::from_secs(30);
/// The writer gives up if the cluster is still alive this long after it took
/// the lease, well before the lease could expire under it.
const KILL_DEADLINE: Duration = Duration::from_secs(20);
/// Bounds every statement, so a connection to a killed server cannot hang.
const STATEMENT_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the restarted cluster gets to answer the first queries.
const RECOVERY_DEADLINE: Duration = Duration::from_secs(180);
const WRITER_OWNER: &str = "tidb-crash-writer";
const FIXED_TIME_MS: i64 = 1_790_000_000_000;

fn required_env(name: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| panic!("{name} must be set; scripts/test-tidb.sh supplies it"))
}

fn crash_dir() -> PathBuf {
    let dir = PathBuf::from(required_env("MOUNT_RS_TIDB_CRASH_DIR"));
    assert!(dir.is_dir(), "{} must be a directory", dir.display());
    dir
}

fn options(volume_key: &str) -> TidbStorageOptions {
    TidbStorageOptions::new(volume_key)
        .with_durable(true)
        .with_statement_timeout(STATEMENT_TIMEOUT)
        .with_max_block_bytes(4 * 1024 * 1024)
        .with_max_namespace_bytes(16 * 1024 * 1024)
}

async fn assert_actual_tidb(url: &str) {
    let pool = Pool::from_url(url).expect("the TiDB identity URL must be parseable");
    let mut connection = pool
        .get_conn()
        .await
        .expect("the TiDB identity connection must be reachable");
    let identity: Option<(String, String)> = connection
        .exec_first("SELECT tidb_version(), VERSION()", ())
        .await
        .expect("the TiDB identity query must succeed");
    let (tidb_version, version) = identity.expect("the TiDB identity query must return a row");
    assert!(
        format!("{tidb_version} {version}")
            .to_ascii_lowercase()
            .contains("tidb"),
        "server identity did not identify TiDB: {version:?}"
    );
    drop(connection);
    pool.disconnect()
        .await
        .expect("the TiDB identity pool must close");
}

fn block_bytes(index: u64) -> Vec<u8> {
    let pattern = format!("mount-rs crash block {index};");
    pattern
        .as_bytes()
        .iter()
        .copied()
        .cycle()
        .take(BLOCK_BYTES)
        .collect()
}

fn block_index(seq: u64) -> u64 {
    seq % DISTINCT_BLOCKS
}

fn file_name(seq: u64) -> String {
    format!("published-{seq:06}")
}

/// The namespace before any publication. Every timestamp is fixed so the
/// verifying process can rebuild exactly what the writer published.
async fn base_namespace() -> Namespace {
    let mut stats = MemoryFs::empty().stat("/").await.expect("root stat");
    assert_eq!(stats.mode & S_IFMT, S_IFDIR);
    stats.atime_ms = FIXED_TIME_MS;
    stats.mtime_ms = FIXED_TIME_MS;
    stats.ctime_ms = FIXED_TIME_MS;
    stats.birthtime_ms = FIXED_TIME_MS;
    let root = stats.ino;
    Namespace {
        format_version: 1,
        root,
        next_inode: root + 1,
        default_uid: 0,
        default_gid: 0,
        umask: 0o022,
        default_chunker: chunker_config(),
        nodes: BTreeMap::from([(
            root,
            NodeMetadata {
                stats,
                data: NodeData::Directory { entries: vec![] },
            },
        )])
        .into(),
    }
}

fn chunker_config() -> mount_rs_core::chunking::ChunkerConfig {
    FixedSizeChunker::new(BLOCK_BYTES)
        .expect("fixed chunker")
        .config()
}

/// Publication `seq` adds one file to the namespace left by `seq - 1`.
fn add_publication(namespace: &mut Namespace, seq: u64, blocks: &[BlockId]) {
    let inode = namespace.next_inode;
    let root = namespace.root;
    let mut stats = namespace.nodes[&root].stats.clone();
    stats.ino = inode;
    stats.mode = S_IFREG | 0o644;
    stats.nlink = 1;
    stats.size = BLOCK_BYTES as u64;
    stats.blocks = BLOCK_BYTES.div_ceil(512) as u64;
    stats.mtime_ms = FIXED_TIME_MS + seq as i64;
    stats.ctime_ms = stats.mtime_ms;
    let mut nodes: BTreeMap<_, _> = namespace
        .nodes
        .iter()
        .map(|(inode, node)| (*inode, node.clone()))
        .collect();
    nodes.insert(
        inode,
        NodeMetadata {
            stats,
            data: NodeData::File(FileLayout {
                chunker: chunker_config(),
                extents: vec![BlockExtent {
                    file_offset: 0,
                    block: blocks[block_index(seq) as usize].clone(),
                    block_offset: 0,
                    length: BLOCK_BYTES as u64,
                }],
            }),
        },
    );
    let root_node = nodes.get_mut(&root).expect("root node");
    let NodeData::Directory { entries } = &mut root_node.data else {
        panic!("root is a directory");
    };
    entries.push(DirectoryEntry {
        name: file_name(seq),
        inode,
    });
    namespace.nodes = nodes.into();
    namespace.next_inode = inode + 1;
}

async fn namespace_after(seq: u64, blocks: &[BlockId]) -> Namespace {
    let mut namespace = base_namespace().await;
    for publication in 1..=seq {
        add_publication(&mut namespace, publication, blocks);
    }
    namespace
}

async fn put_blocks(store: &TidbBlockStore) -> Vec<BlockId> {
    let mut ids = Vec::new();
    for index in 0..DISTINCT_BLOCKS {
        ids.push(
            store
                .put(&block_bytes(index))
                .await
                .expect("store a crash-test block"),
        );
    }
    store
        .flush()
        .await
        .expect("acknowledge the crash-test blocks");
    ids
}

struct Journal {
    file: fs::File,
}

impl Journal {
    fn create(path: &Path) -> Self {
        let file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(path)
            .unwrap_or_else(|error| panic!("create journal {}: {error}", path.display()));
        Self { file }
    }

    fn record(&mut self, line: &str) {
        writeln!(self.file, "{line}").expect("append to the crash journal");
        self.file.flush().expect("flush the crash journal");
    }
}

fn error_class(error: &FsError) -> &'static str {
    if error.to_string().contains("commit outcome is unknown") {
        "unknown-outcome"
    } else {
        "not-sent"
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "run by scripts/test-tidb.sh with MOUNT_RS_TIDB_CRASH=1, which kills the cluster under it"]
async fn actual_tidb_crash_writer_publishes_until_the_cluster_is_killed() {
    let url = required_env("MOUNT_RS_TIDB_URL");
    let volume_key = required_env("MOUNT_RS_TIDB_CRASH_VOLUME_KEY");
    let dir = crash_dir();
    assert_actual_tidb(&url).await;

    let metadata = TidbMetadataStore::connect_with_options(&url, options(&volume_key))
        .await
        .expect("connect crash-test metadata");
    let blocks = TidbBlockStore::connect_with_options(&url, options(&volume_key))
        .await
        .expect("connect crash-test blocks");
    let block_ids = put_blocks(&blocks).await;
    let initial = metadata.load().await.expect("load crash-test metadata");
    assert_eq!(initial.revision, 0, "the crash-test volume must be new");

    let mut journal = Journal::create(&dir.join("journal"));
    for (index, id) in block_ids.iter().enumerate() {
        journal.record(&format!("block {index} {}", id.0));
    }
    let lease = metadata
        .acquire_writer(WRITER_OWNER, LEASE_TTL)
        .await
        .expect("acquire the crash-test writer");
    let leased_at = Instant::now();
    journal.record(&format!(
        "lease {} {} {}",
        lease.owner, lease.fence, lease.expires_at_ms
    ));

    let mut namespace = base_namespace().await;
    let mut revision = 0_u64;
    let mut seq = 0_u64;
    let failure = loop {
        assert!(
            leased_at.elapsed() < KILL_DEADLINE,
            "the cluster was still answering {KILL_DEADLINE:?} after the writer started \
             ({seq} publications acknowledged)"
        );
        seq += 1;
        add_publication(&mut namespace, seq, &block_ids);
        journal.record(&format!("start {seq} {revision}"));
        match metadata.publish(revision, &lease, namespace.clone()).await {
            Ok(published) => {
                assert_eq!(
                    published,
                    revision + 1,
                    "publication {seq} skipped a revision"
                );
                revision = published;
                journal.record(&format!("ack {seq} {revision}"));
                if seq == READY_AFTER {
                    fs::write(dir.join("ready"), b"ready\n").expect("signal the harness");
                }
            }
            Err(error) => break error,
        }
    };
    let class = error_class(&failure);
    journal.record(&format!("error {seq} {class}"));
    println!(
        "TIDB_CRASH_WRITER acknowledged={} in_flight={seq} error_class={class} error={failure}",
        seq - 1
    );
    assert!(
        seq > READY_AFTER,
        "publication {seq} failed before the harness was told to kill the cluster: {failure}"
    );
    // A retryable conflict would invite the caller to replay a publication
    // whose outcome nobody knows.
    assert!(
        !failure.is(ErrorCode::Eagain),
        "a publication interrupted by a crash must not be reported as retryable: {failure}"
    );
    if class == "unknown-outcome" {
        assert!(failure.is(ErrorCode::Eio), "{failure}");
    }
    // The pools point at dead servers. Closing them is bounded and its result
    // says nothing about the test.
    let _ = tokio::time::timeout(Duration::from_secs(10), metadata.close()).await;
    let _ = tokio::time::timeout(Duration::from_secs(10), blocks.close()).await;
}

struct JournalState {
    blocks: Vec<BlockId>,
    lease: WriterLease,
    acknowledged: BTreeMap<u64, u64>,
    in_flight: u64,
    in_flight_base: u64,
    error_class: String,
}

fn read_journal(path: &Path) -> JournalState {
    let text = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("read journal {}: {error}", path.display()));
    let mut blocks = Vec::new();
    let mut lease = None;
    let mut acknowledged = BTreeMap::new();
    let mut started = BTreeMap::new();
    let mut error = None;
    for line in text.lines() {
        let fields: Vec<&str> = line.split(' ').collect();
        let number = |index: usize| -> u64 {
            fields
                .get(index)
                .and_then(|value| value.parse().ok())
                .unwrap_or_else(|| panic!("malformed journal line {line:?}"))
        };
        match fields.first().copied() {
            Some("block") => {
                assert_eq!(number(1), blocks.len() as u64, "block lines are in order");
                blocks.push(BlockId(fields[2].to_owned()));
            }
            Some("lease") => {
                lease = Some(WriterLease {
                    owner: fields[1].to_owned(),
                    fence: number(2),
                    expires_at_ms: number(3),
                })
            }
            Some("start") => {
                started.insert(number(1), number(2));
            }
            Some("ack") => {
                acknowledged.insert(number(1), number(2));
            }
            Some("error") => error = Some((number(1), fields[2].to_owned())),
            _ => panic!("unknown journal line {line:?}"),
        }
    }
    let (in_flight, error_class) = error.expect("the writer recorded the error that stopped it");
    assert!(
        !acknowledged.contains_key(&in_flight),
        "the failed publication {in_flight} was also acknowledged"
    );
    let in_flight_base = *started
        .get(&in_flight)
        .expect("the failed publication was started");
    let expected: Vec<u64> = (1..in_flight).collect();
    assert_eq!(
        acknowledged.keys().copied().collect::<Vec<_>>(),
        expected,
        "every publication before the failed one was acknowledged, in order"
    );
    assert_eq!(
        blocks.len() as u64,
        DISTINCT_BLOCKS,
        "every block id was recorded"
    );
    JournalState {
        blocks,
        lease: lease.expect("the writer recorded its lease"),
        acknowledged,
        in_flight,
        in_flight_base,
        error_class,
    }
}

/// Retry while the restarted cluster elects leaders and reloads its schema.
async fn until_recovered<T, F, Fut>(what: &str, mut operation: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = mount_rs_core::Result<T>>,
{
    let deadline = Instant::now() + RECOVERY_DEADLINE;
    loop {
        match operation().await {
            Ok(value) => return value,
            Err(error) if Instant::now() < deadline => {
                eprintln!("waiting for the restarted cluster to {what}: {error}");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Err(error) => panic!("the restarted cluster could not {what}: {error}"),
        }
    }
}

fn file_names(namespace: &Namespace) -> Vec<String> {
    let NodeData::Directory { entries } = &namespace.nodes[&namespace.root].data else {
        panic!("root is a directory");
    };
    entries.iter().map(|entry| entry.name.clone()).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "run by scripts/test-tidb.sh with MOUNT_RS_TIDB_CRASH=1 after it restarts the killed cluster"]
async fn actual_tidb_crash_survivors_match_the_acknowledged_publications() {
    let url = required_env("MOUNT_RS_TIDB_URL");
    let volume_key = required_env("MOUNT_RS_TIDB_CRASH_VOLUME_KEY");
    let dir = crash_dir();
    let journal = read_journal(&dir.join("journal"));
    let last_acknowledged = journal.in_flight - 1;
    let acknowledged_revision = journal
        .acknowledged
        .get(&last_acknowledged)
        .copied()
        .expect("at least one acknowledged publication");
    assert_eq!(journal.in_flight_base, acknowledged_revision);

    let metadata = until_recovered("open the metadata store", || {
        TidbMetadataStore::connect_with_options(&url, options(&volume_key))
    })
    .await;
    let blocks = until_recovered("open the block store", || {
        TidbBlockStore::connect_with_options(&url, options(&volume_key))
    })
    .await;
    let loaded = until_recovered("load the metadata", || metadata.load()).await;
    loaded
        .validate()
        .expect("the metadata that survived the crash is a valid namespace");
    let survived = loaded.namespace.expect("the volume was initialized");

    let block_ids = journal.blocks.clone();

    let in_flight_present = if loaded.revision == acknowledged_revision {
        false
    } else if loaded.revision == acknowledged_revision + 1 {
        true
    } else {
        panic!(
            "revision {} after the crash; the last acknowledged publication made revision \
             {acknowledged_revision} and only one more was in flight",
            loaded.revision
        );
    };
    if journal.error_class != "unknown-outcome" {
        assert!(
            !in_flight_present,
            "publication {} failed before it reached TiDB but is present",
            journal.in_flight
        );
    }
    let survived_seq = if in_flight_present {
        journal.in_flight
    } else {
        last_acknowledged
    };
    // Not just the names: every node, every directory entry and every block
    // reference must be exactly what that publication wrote, so a partial
    // publication cannot pass.
    let expected = namespace_after(survived_seq, &block_ids).await;
    assert_eq!(
        file_names(&survived),
        file_names(&expected),
        "the surviving files are not those of publication {survived_seq}"
    );
    assert!(
        survived.nodes == expected.nodes
            && survived.next_inode == expected.next_inode
            && survived.root == expected.root,
        "the surviving namespace differs from publication {survived_seq}"
    );

    let mut referenced = BTreeSet::new();
    for (_, node) in &survived.nodes {
        if let NodeData::File(layout) = &node.data {
            for extent in &layout.extents {
                referenced.insert(extent.block.clone());
            }
        }
    }
    for (index, id) in block_ids.iter().enumerate() {
        if !referenced.contains(id) {
            continue;
        }
        let bytes = until_recovered("read a referenced block", || blocks.get(id)).await;
        assert_eq!(
            bytes,
            block_bytes(index as u64),
            "block {id:?} named by the surviving metadata changed"
        );
    }

    // The dead writer's lease still fences it: once a new writer holds the
    // volume, the old lease can neither publish nor renew.
    let stale = journal.lease.clone();
    let acquire_deadline = Instant::now() + LEASE_TTL + RECOVERY_DEADLINE;
    let mut refused = 0_u32;
    let current = loop {
        match metadata
            .acquire_writer("tidb-crash-successor", LEASE_TTL)
            .await
        {
            Ok(lease) => break lease,
            Err(error) if error.is(ErrorCode::Eagain) && Instant::now() < acquire_deadline => {
                // The dead writer's lease has not expired yet.
                refused += 1;
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Err(error) if Instant::now() < acquire_deadline => {
                eprintln!("waiting to take over the crash-test volume: {error}");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Err(error) => panic!("no new writer could take over the crashed volume: {error}"),
        }
    };
    assert!(
        current.fence > stale.fence,
        "the new lease has a newer fence"
    );
    let mut next = survived.clone();
    add_publication(&mut next, survived_seq + 1, &block_ids);
    let stale_publish = metadata
        .publish(loaded.revision, &stale, next.clone())
        .await
        .expect_err("the crashed writer's lease must not publish after a takeover");
    assert!(stale_publish.is(ErrorCode::Estale), "{stale_publish}");
    let stale_renew = metadata
        .renew_writer(&stale, LEASE_TTL)
        .await
        .expect_err("the crashed writer's lease must not renew after a takeover");
    assert!(stale_renew.is(ErrorCode::Estale), "{stale_renew}");
    let revision = metadata
        .publish(loaded.revision, &current, next)
        .await
        .expect("the new writer publishes after the crash");
    assert_eq!(revision, loaded.revision + 1);
    metadata
        .release_writer(&current)
        .await
        .expect("release the new writer");
    metadata.close().await.expect("close crash-test metadata");
    blocks.close().await.expect("close crash-test blocks");

    println!(
        "TIDB_CRASH_VERIFIED acknowledged={last_acknowledged} acknowledged_present={last_acknowledged} \
         in_flight={} in_flight_error={} in_flight_outcome={} revision={} blocks_read={} \
         successor_refused_while_lease_live={refused} stale_lease=fenced",
        journal.in_flight,
        journal.error_class,
        if in_flight_present {
            "committed"
        } else {
            "absent"
        },
        loaded.revision,
        referenced.len(),
    );
}
