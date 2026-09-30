//! Paired actual-TiDB publication microbenchmark; no blob or sustained I/O load.
//! Run this ignored test serially in both source/binary-qualified variants.
use super::*;
use mount_rs_core::diagnostics::storage;
use std::time::{Duration, Instant};
use tokio::time::timeout;

#[path = "member_performance_alloc.rs"]
mod allocations;

const OP_BOUND: Duration = Duration::from_secs(30);
const WORK_BOUND: Duration = Duration::from_secs(180);
const WARMUPS: usize = 4;
const SAMPLES: usize = 32;
const ENUMERATE: &str =
    "SELECT inode FROM mount_rs_tidb_compact_members WHERE volume_key=? ORDER BY inode";
const AGGREGATE: &str = "SELECT COUNT(*),COUNT(CASE WHEN inode BETWEEN ? AND ? THEN 1 END) FROM mount_rs_tidb_compact_members WHERE volume_key=?";

type Checked<T> = std::result::Result<T, &'static str>;

fn proposal(
    base: &CompactSnapshot,
    name: &str,
) -> Checked<(CompactStructuralDelta, CompactSnapshot, u64)> {
    let mut namespace = base.namespace().map_err(|_| "invalid reference snapshot")?;
    let inode = add_file(&mut namespace, name);
    let delta = CompactStructuralDelta::capture(base, &namespace, StructuralScope::FileCreate)
        .map_err(|_| "invalid create proposal")?;
    let expected = delta
        .evaluate(base)
        .map_err(|_| "invalid create reference")?;
    Ok((delta, expected, inode))
}

async fn audit_created(f: &Fixture, expected: &CompactSnapshot) -> Checked<()> {
    let actual = timeout(OP_BOUND, f.store.load_compact_snapshot(f.backing))
        .await
        .map_err(|_| "intermediate fresh audit deadline; key retained")?
        .map_err(|_| "intermediate fresh audit failed; key retained")?;
    if actual != *expected {
        return Err("intermediate fresh full snapshot differs from pure reference; key retained");
    }
    Ok(())
}

async fn reset(
    store: &TidbMetadataStore,
    created: &CompactSnapshot,
    inode: u64,
) -> Checked<CompactSnapshot> {
    let mut namespace = created.namespace().map_err(|_| "invalid reset source")?;
    namespace.nodes.remove(&inode).ok_or("reset inode absent")?;
    let NodeData::Directory { entries } = &mut namespace
        .nodes
        .get_mut(&namespace.root)
        .ok_or("reset root absent")?
        .data
    else {
        return Err("reset root not directory");
    };
    let before = entries.len();
    entries.retain(|entry| entry.inode != inode);
    if entries.len() + 1 != before {
        return Err("reset dentry not unique");
    }
    let delta = CompactStructuralDelta::capture(created, &namespace, StructuralScope::Full)
        .map_err(|_| "invalid full reset")?;
    let expected = delta
        .evaluate(created)
        .map_err(|_| "invalid full reset reference")?;
    let publication = timeout(OP_BOUND, store.publish_compact_structure(&delta))
        .await
        .map_err(|_| "reset deadline: outcome unknown; not replayed")?
        .map_err(|_| "reset failed; not replayed")?;
    if publication.anchor != expected.anchor {
        return Err("reset anchor differs from pure reference");
    }
    Ok(expected)
}

fn classify(queries: &[String], guard_rows: usize) -> Checked<serde_json::Value> {
    let members: Vec<_> = queries
        .iter()
        .filter(|sql| {
            sql.starts_with("SELECT ") && sql.contains(" FROM mount_rs_tidb_compact_members ")
        })
        .collect();
    if members.len() != 1 || guard_rows != 1 {
        return Err("probe lacks one membership statement and selected parent row");
    }
    let mode = if members[0] == ENUMERATE {
        "enumerating"
    } else if members[0] == AGGREGATE {
        "exact_range"
    } else {
        return Err("unrecognized membership statement");
    };
    let starts = queries
        .iter()
        .filter(|sql| sql.starts_with("START TRANSACTION"))
        .count();
    let commits = queries
        .iter()
        .filter(|sql| sql.eq_ignore_ascii_case("COMMIT"))
        .count();
    if starts != 1 || commits != 1 {
        return Err("probe lacks one transaction and acknowledged COMMIT");
    }
    Ok(serde_json::json!({
        "mode": mode, "member_statements": members.len(), "selected_guard_rows": guard_rows,
        "start_transactions": starts, "commits": commits, "total_statements": queries.len(),
        "timed": false
    }))
}

async fn probe(
    f: &Fixture,
    base: &CompactSnapshot,
) -> Checked<(serde_json::Value, CompactSnapshot)> {
    let (delta, expected, inode) = proposal(base, "member-performance-probe")?;
    let proxy = timeout(OP_BOUND, compact_proxy::Proxy::new(&f.url))
        .await
        .map_err(|_| "proxy creation deadline")?;
    let connection = timeout(
        OP_BOUND,
        TidbMetadataStore::connect_with_key(&proxy.url, &f.key),
    )
    .await;
    let writer = match connection {
        Ok(Ok(writer)) => writer,
        _ => {
            timeout(OP_BOUND, proxy.shutdown())
                .await
                .map_err(|_| "proxy retirement deadline")?;
            return Err("probe connection failed");
        }
    };
    proxy.begin();
    let result = timeout(OP_BOUND, writer.publish_compact_structure(&delta)).await;
    let (queries, rows) = proxy.end();
    let closed = timeout(OP_BOUND, writer.close()).await;
    let drained = timeout(OP_BOUND, proxy.shutdown()).await;
    if !matches!(closed, Ok(Ok(()))) || drained.is_err() {
        return Err("probe pool or relay retirement unproven");
    }
    let publication = result
        .map_err(|_| "probe deadline: outcome unknown; not replayed")?
        .map_err(|_| "probe failed; not replayed")?;
    if publication.anchor != expected.anchor {
        return Err("probe anchor differs from pure reference");
    }
    let description = classify(&queries, rows)?;
    audit_created(f, &expected).await?;
    Ok((description, reset(&f.store, &expected, inode).await?))
}

async fn measure(f: &Fixture, files: usize) -> Checked<(serde_json::Value, CompactSnapshot)> {
    let empty = timeout(OP_BOUND, f.store.load_compact_snapshot(f.backing))
        .await
        .map_err(|_| "seed read deadline")?
        .map_err(|_| "seed read failed")?;
    let mut namespace = empty.namespace().map_err(|_| "invalid seed source")?;
    for index in 0..files {
        add_file(
            &mut namespace,
            &format!("member-performance-seed-{index:04}"),
        );
    }
    let seed = CompactStructuralDelta::capture(&empty, &namespace, StructuralScope::Full)
        .map_err(|_| "invalid seed proposal")?;
    let expected = seed
        .evaluate(&empty)
        .map_err(|_| "invalid seed reference")?;
    let seeded = timeout(OP_BOUND, f.store.publish_compact_structure(&seed))
        .await
        .map_err(|_| "seed deadline: outcome unknown; not replayed")?
        .map_err(|_| "seed failed; not replayed")?;
    if seeded.anchor != expected.anchor {
        return Err("seed anchor differs from pure reference");
    }
    let base = timeout(OP_BOUND, f.store.load_compact_snapshot(f.backing))
        .await
        .map_err(|_| "seed audit deadline")?
        .map_err(|_| "seed audit failed")?;
    if base != expected {
        return Err("seed fresh full snapshot differs from reference");
    }
    let initial_members = base.anchor.members.clone();
    let (probe, mut base) = probe(f, &base).await?;
    let mut samples = Vec::with_capacity(WARMUPS + SAMPLES);
    for index in 0..WARMUPS + SAMPLES {
        if base.anchor.members != initial_members {
            return Err("fixed corpus member identities changed");
        }
        let (delta, expected, inode) =
            proposal(&base, &format!("member-performance-create-{index:02}"))?;
        // Snapshot construction and reference evaluation are outside both windows.
        let before = storage::snapshot();
        let window = allocations::Window::begin();
        let started = Instant::now();
        let publication = timeout(OP_BOUND, f.store.publish_compact_structure(&delta)).await;
        let elapsed_ns =
            u64::try_from(started.elapsed().as_nanos()).map_err(|_| "latency overflow")?;
        let allocated = window.finish();
        let after = storage::snapshot();
        let publication = publication
            .map_err(|_| "publish deadline: outcome unknown; not replayed")?
            .map_err(|_| "publish failed; not replayed")?;
        if publication.anchor != expected.anchor {
            return Err("publication anchor differs from pure reference");
        }
        let observed = after.delta(&before).map_err(|_| "storage counters reset")?;
        let reads = observed
            .entries
            .iter()
            .find(|entry| entry.name == "tidb.sql.inode_read")
            .ok_or("inode read metric absent")?;
        if reads.calls == 0
            || reads.returned_row_observations == 0
            || reads.in_flight != 0
            || reads.error != 0
            || reads.cancelled != 0
        {
            return Err("inode read metric coverage is unavailable or incomplete");
        }
        samples.push(serde_json::json!({
            "index": index, "warmup": index < WARMUPS, "latency_ns": elapsed_ns,
            "base_members": base.anchor.members.len(),
            "allocation_calls": allocated.calls, "allocation_requested_bytes": allocated.requested_bytes,
            "tidb_sql_inode_read": {
                "calls": reads.calls, "success": reads.success, "returned_rows": reads.returned_rows,
                "returned_row_observations": reads.returned_row_observations, "elapsed_ns": reads.elapsed_ns
            }
        }));
        // Validate the stored new body before reset can remove it. Every audit
        // starts a fresh complete provider read, outside the measured windows.
        audit_created(f, &expected).await?;
        // Remove precisely this created inode/dentry through Full publication,
        // advancing the pure snapshot but retaining the original member IDs.
        base = reset(&f.store, &expected, inode).await?;
    }
    let report = serde_json::json!({
        "schema": "compact-member-publication-performance-v1", "files": files,
        "mode": probe["mode"], "probe": probe, "warmups": WARMUPS, "timed_samples": SAMPLES,
        "fixed_base_members": files + 1, "samples": samples,
        "fresh_intermediate_full_oracles": 1 + WARMUPS + SAMPLES,
        "latency_scope": "direct provider publish await plus identical timeout wrapper; capture, evaluate, probe, Full resets, snapshots, oracle and cleanup excluded",
        "allocation_scope": "current-thread Rust allocation requests during direct publish; driver/runtime included; foreign threads and server excluded; not RSS",
        "storage_scope": "process-local non-atomic counter endpoints around direct publish; run this test serially with MOUNT_RS_PROFILE_IO=1",
        "throughput_or_physical_iops_claim": false
    });
    Ok((report, base))
}

async fn cleanup_owned(f: &Fixture) -> Checked<()> {
    let pool = Pool::from_url(&f.url).map_err(|_| "cleanup pool creation failed")?;
    let work = timeout(OP_BOUND, async {
        let mut conn = pool
            .get_conn()
            .await
            .map_err(|_| "cleanup connection failed")?;
        for table in [
            "mount_rs_tidb_compact_dentries",
            "mount_rs_tidb_compact_guards",
            "mount_rs_tidb_compact_members",
            "mount_rs_tidb_inodes",
            "mount_rs_tidb_metadata",
        ] {
            conn.exec_drop(format!("DELETE FROM {table} WHERE volume_key=?"), (&f.key,))
                .await
                .map_err(|_| "owned key cleanup failed")?;
            let remaining: Option<u64> = conn
                .exec_first(
                    format!("SELECT COUNT(*) FROM {table} WHERE volume_key=?"),
                    (&f.key,),
                )
                .await
                .map_err(|_| "owned key cleanup verification failed")?;
            if remaining != Some(0) {
                return Err("owned key rows remain");
            }
        }
        Ok(())
    })
    .await;
    let retired = timeout(OP_BOUND, pool.disconnect()).await;
    if !matches!(retired, Ok(Ok(()))) {
        return Err("cleanup pool retirement unproven");
    }
    work.map_err(|_| "cleanup deadline")?
}

async fn run_case(files: usize) -> Checked<serde_json::Value> {
    let f = timeout(OP_BOUND, fixture(true))
        .await
        .map_err(|_| "fixture deadline")?;
    let measured = timeout(WORK_BOUND, measure(&f, files)).await;
    let closed = timeout(OP_BOUND, f.store.close()).await;
    if !matches!(closed, Ok(Ok(()))) {
        return Err("direct writer retirement unproven; diagnostic key retained");
    }
    let (mut report, expected) =
        measured.map_err(|_| "case deadline: outcome unknown; key retained")??;
    // Only settled successful publications permit an independent new connection.
    let observer = timeout(
        OP_BOUND,
        TidbMetadataStore::connect_with_key(&f.url, &f.key),
    )
    .await
    .map_err(|_| "fresh observer connection deadline")?
    .map_err(|_| "fresh observer connection failed")?;
    let actual = timeout(OP_BOUND, observer.load_compact_snapshot(f.backing)).await;
    let retired = timeout(OP_BOUND, observer.close()).await;
    if !matches!(retired, Ok(Ok(()))) {
        return Err("fresh observer retirement unproven; key retained");
    }
    let actual = actual
        .map_err(|_| "fresh oracle deadline; key retained")?
        .map_err(|_| "fresh oracle failed; key retained")?;
    if actual != expected {
        return Err("fresh complete snapshot differs from pure reference; key retained");
    }
    cleanup_owned(&f).await?;
    report["fresh_full_snapshot_parity"] = serde_json::json!(true);
    report["writer_and_probe_retired"] = serde_json::json!(true);
    report["owned_key_cleanup_verified"] = serde_json::json!(true);
    Ok(report)
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires actual owned TiDB, MOUNT_RS_TIDB_URL and MOUNT_RS_PROFILE_IO=1; run serial"]
async fn actual_compact_member_publication_performance() {
    for files in [4, 1_000] {
        let result = run_case(files).await;
        // All normal-path pools/relay and exact owned rows have settled first.
        let report = result.expect("bounded member publication benchmark refused");
        eprintln!("{report}");
    }
}
