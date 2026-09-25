# Drive metadata read amplification and provider comparison

## Workload and boundaries

The common workload uses 100 QUIC clients and ten independently opened server
coordinators sharing one Drive namespace. Both metadata and 4 KiB immutable
blocks use the selected provider. All ten service instances run in one process.
The public SDK creates the same fixed 4 KiB, concurrent-write filesystem for
every provider. The comparison explicitly uses MRC2 revision CAS, with
writeback disabled and no MRC3 directory checkout. The dataset contains 100 files with 32 distinct blocks each
(12.5 MiB). The smaller dataset stays below FoundationDB's default 512 KiB
serialized namespace limit; the original TiDB qualification used 64 blocks.

Read-only stages sweep total queue depths 100, 200, 400, 800, and 1,600. A second
invocation measures separate read and write stages at depths 100 and 200.
Stages have a 3-second warmup and nominal 15-second measurement. Throughput
uses actual elapsed time including drain. Only complete, validated operations
count. Errors invalidate a run; uncertain writes are never replayed. Successful
runs reopen a fresh coordinator and verify all 100 names and full file contents.
Latency percentiles are logarithmic histogram upper bounds.

Measurements are cache-warm logical Drive operations, including QUIC, JSON,
authorization and audit output, provider calls, namespace snapshots, and
publication. They are not a direct benchmark of SSD IOPS or database engines.
Runs are serial, with other benchmark builds and workloads stopped. SQLite and
PGlite clients run natively on macOS. TiDB runs in Docker with the service on
macOS. FoundationDB and its native service client run in Linux containers.
These environments are deployment tradeoffs, not a controlled engine-only
ranking. Local single-node clusters do not establish replicated durability.

## Read amplification fix

SQLite, PGlite, and TiDB now implement `MetadataStore::load_if_changed` instead
of always fetching and decoding namespace JSON. A fresh single SQL statement
returns the revision and uses `CASE` to omit namespace content only on an exact
nonzero revision match. Revision and payload belong to one statement snapshot;
both filesystem consistency checks remain. FoundationDB already reads a small
manifest and avoids namespace shards when its revision matches.

Zero/uninitialized revisions cannot return unchanged. Missing and invalid
stored revisions fail closed. Changed payloads retain the existing validation
requirements. Revisions must increase on every publication and must never be
reused. Direct edits that change content without incrementing the revision
violate that contract; unconditional `load` remains available.

The unchanged SQL fast path removes full namespace bytes from the SQL response
and avoids JSON decoding. SQLite additionally avoids materializing the payload
column. This does not prove that TiDB/PGlite perform zero internal storage reads
for that column. Database row layout, caches, and expression evaluation still
matter.

### TiDB before/after, original 25 MiB dataset

The original namespace was 866,917 bytes: two full reloads per 4 KiB read
transferred about 1.65 MiB of namespace text, or 423 times the payload size.
After the fix, unchanged reads return revision plus SQL NULL, avoiding those
namespace transfers. The same release workload produced:

| Total queue depth | Before read IOPS | After read IOPS | After p99 upper bound |
| --- | ---: | ---: | ---: |
| 100 | 89.37 | 1,393.08 | 0.131 s |
| 200 | 69.70 | 1,415.24 | 0.262 s |
| 400 | 30.79 | 1,347.20 | 0.524 s |
| 800 | 117.44 | 1,474.88 | 1.049 s |
| 1,600 | 114.90 | 1,360.91 | 2.097 s |

Both read-only runs verified all 100 files. Peak throughput increased about
12.6 times. This is a short-run observation; the before curve was variable.
The after-fix read/write probe at depth 200 encountered one `EAGAIN` metadata
publication conflict and skipped final verification. Its completed-write rate
is diagnostic, not a qualified successful write result.

The 25 MiB before/after measurements preceded the requested rebase. The
common four-provider comparison runs on `origin/main` at `e9eb233f` plus the
remote Drive branch and conditional-load changes.

## Measured performance on the rebased branch

Raw artifacts are retained in
[the benchmark evidence directory](benchmarks/remote-provider-comparison-20260924/).
These are short single invocations, not statistical confidence intervals. The
read-only sweep and separate read/write invocation use different fresh namespaces.
Read and write phases below run separately; they are not a simultaneous mixed workload.

### Read throughput and latency as concurrency increases

| Total queue depth | SQLite read IOPS / p99 upper | FoundationDB read IOPS / p99 upper | TiDB read IOPS / p99 upper |
| --- | ---: | ---: | ---: |
| 100 | 4,587 / 65.5 ms | 4,881 / 32.8 ms | 1,708 / 131 ms |
| 200 | 4,378 / 131 ms | 4,667 / 65.5 ms | 1,726 / 262 ms |
| 400 | 4,491 / 262 ms | 4,705 / 131 ms | 1,736 / 524 ms |
| 800 | 4,487 / 524 ms | 4,561 / 262 ms | 1,775 / 1,049 ms |
| 1,600 | 4,513 / 1,049 ms | 4,467 / 524 ms | 1,782 / 2,097 ms |

All three sweeps completed without errors and verified all 100 files through a fresh
coordinator. SQLite's throughput was already at its observed plateau at depth
100. FoundationDB lost 8.5% throughput over the sweep. TiDB gained only 4.3%
throughput from 16 times as many outstanding requests.
The extra concurrency chiefly created queues: all three p99 bounds rose 16 times.
Use depth 100 as the initial latency-conscious operating point for this workload;
these runs do not establish a universal admission limit for other datasets or hardware.
SQLite was 2.5–2.7 times faster than TiDB in these deployment configurations. Payload
throughput peaked at 17.9 MiB/s for SQLite, 19.1 MiB/s for FoundationDB, and
7.0 MiB/s for TiDB. FoundationDB and SQLite read rates were close; this short
run and the native Linux/macOS difference do not establish a decisive engine
throughput winner. FoundationDB had lower observed read latency bounds.

### Committed writes and contention

| Total queue depth | SQLite write IOPS | SQLite p50 / p99 upper bounds | TiDB write IOPS | TiDB p50 / p99 upper bounds |
| --- | ---: | ---: | ---: | ---: |
| 100 | 198.15 | 262 ms / 4.19 s | 58.39 | 262 ms / 16.78 s |
| 200 | 156.88 | 1.05 s / 8.39 s | 45.17 | 4.19 s / 16.78 s |

Both complete read/write invocations verified all 100 files and returned no
operation or cleanup errors. Writes count only successful committed operation
responses, with writeback disabled. SQLite was approximately 3.4 times faster
in these phases. Doubling queue depth reduced write throughput by 21% for
SQLite and 23% for TiDB, and worsened tail latency or median latency. Increasing
outstanding writes is therefore a loss here. Long drain times are included
in throughput: TiDB's depth-200 write stage lasted 18.64 seconds despite a
15-second nominal window.

The read/write gap is large: depth-100 read-only throughput is 23 times SQLite's
committed-write throughput and 29 times TiDB's. This is consistent with the
shared publication unit and retry work described below. The runs do not include
phase-wide CPU attribution, physical storage I/O counters, or per-operation provider timing, so they
cannot assign a percentage of the limit to SQL, namespace cloning, fsync,
QUIC, or JSON serialization.

An additional SQLite invocation at depths 100–800 also passed, with writes
ranging from 298 to 104 IOPS. Its raw artifact is retained as
`sqlite-extra-depths.json`. It demonstrates run variability and the decline
at higher write concurrency; the matched two-depth invocation above is used
for the comparison rather than selecting that invocation's best write rate.

### Shared service profile: catalog connection churn

A separate SQLite depth-100 read/write invocation was sampled twice with macOS
`sample` (3 seconds per phase, 10 ms sampling interval). Both samples succeeded;
the invocation had zero operation errors and freshly verified 100 files. The
profiler perturbs execution, so its throughput is excluded from the matched
comparison. Stack excerpts and its diagnostic artifact are retained with the
other evidence. All stacks combine clients and services in one process.

The read sample contains substantial catalog connection and WAL work on the
Tokio blocking pool, including `SqliteCatalog::connect`, SQLite open/close,
WAL header/shared-memory locking, and mutex waits. Namespace snapshot cloning,
JSON serialization/deserialization, and socket operations are also visible.
The write sample shows SQLite commit/fsync work, busy sleeps, and serialization.
Wall-clock stack samples include idle and waiting threads; the totals are not
exclusive CPU percentages and should not be summed as a CPU cost breakdown.

This catalog path is common to all four provider workloads. Every operation
loads current grants through `Dispatcher` -> `SqliteCatalog::load_current` ->
`connect`. `connect` opens a new connection and applies `journal_mode=WAL` and
`synchronous=FULL`, then `load_current` fetches/decodes the document and drops
the connection. The SQLite sample recorded 119 thread entries, including the blocking
pool, despite the benchmark specifying 16 async runtime workers. This is not
a count of simultaneously active blocking workers.

Consequently the read plateau is a service deployment result, not an isolated
backend capacity measurement. Before replacing a data backend, test a bounded
catalog connection pool or dedicated catalog worker. Reuse connections and
prepare settings once, while retaining a fresh autocommit revision/grant read
for every operation. Conditional catalog decoding can avoid parsing an unchanged
document. Preserve immediate revocation, fail-closed errors, catalog replacement
behavior, and coherent revision/document snapshots; a TTL grant cache would
change the security contract. This experiment may remove shared overhead for
every provider, but its throughput benefit has not been measured here.

### FoundationDB: read success, write conflict failure

The actual FoundationDB 7.4.7 cluster identity was verified as `ssd-2`, single
redundancy, with an owned persistent Docker data volume. The full read-only
invocation verified 100 files without errors. In the separate read/write
invocation the depth-100 write stage failed with 27 `EAGAIN` publication
conflicts, zero timeouts, and 3,367 completed writes. Cleanup succeeded, but
fresh file verification was skipped after the failure. Its 217.26 completed
writes/second is diagnostic and is not a qualified rate to rank against the
successful SQLite/TiDB write runs. The depth-200 write stage was not reached.

The core caps competing namespace publication attempts at 128. This workload
has ten independent coordinators competing on the same manifest revision.
The observed conflicts expose a limit in the current implementation at the
requested concurrency; they do not establish a general FoundationDB database
write-throughput limit. Preserve known conflicts and ambiguous commit outcomes
as distinct cases when designing backoff or admission control.

One read-stage Docker snapshot recorded approximately 623% CPU and 552 MiB
for the combined workload/service client container, versus 40% CPU and 309 MiB
for the database server. This suggests investigating service/client CPU before
assuming the database or SSD was saturated. It is one diagnostic snapshot,
not phase-average CPU attribution. Container network and block I/O counters
are cumulative and include setup; they are not physical host SSD IOPS.

### PGlite: correctness failure before measurement

The pinned PGlite 0.5.8 / socket 0.2.11 implementation failed during concurrent
file setup, before any timed stages. The first run produced 30 setup `EIO`
failures; a traced repeat reproduced the failure. Observed backend errors include
`26000: unnamed prepared statement does not exist` and `08P01: bind message
supplies 7 parameters, but prepared statement "" requires 2` (also 3).

The socket adapter shares one database's unnamed extended-query state and
queues individual protocol messages. Concurrent clients can interleave Parse
and Bind messages, including while large messages arrive in fragments. One
client's Parse then replaces the unnamed statement required by another
client's Bind. The affected block and publication queries predate the new
conditional metadata query. The provider's isolated real-database regression
suite passed, but this ten-coordinator workload exposed a separate concurrency
failure in the pinned wire adapter.

There is no qualified PGlite IOPS result. Its low deployment footprint is useful,
but this adapter needs correct session isolation before it can qualify this
multi-client service configuration. A single-client or globally serialized
benchmark would measure a different concurrency contract and would not resolve
this failure. This result does not rank the PGlite database engine's throughput.

## Provider tradeoffs

| Provider | Unchanged metadata read | Changed metadata / write behavior | Deployment and durability boundary |
| --- | --- | --- | --- |
| SQLite | Local SQL revision check, no namespace JSON materialization | Full namespace JSON and serialized conditional commits; file locking and synchronous I/O can block runtime workers | Simple local durable file, `synchronous=FULL`; concurrent same-file coordinators need one reliable local filesystem. This does not qualify shared NFS database files or multiple hosts. |
| PGlite | SQL round trip, revision plus NULL | Full JSON refresh/publication; one PGlite database instance processes queries, with bounded wire connections | PostgreSQL in WASM behind a Node socket server. Persistent data directory is exercised, but provider `durable=true` is a caller assertion and does not prove crash-safe host fsync semantics. |
| FoundationDB | Native transaction reads the manifest only | Changed revisions fetch all 8 KiB metadata shards; publication rewrites full namespace and contends on one manifest revision | Distributed transactional KV option; native client and cluster lifecycle add setup cost. Default namespace limit 512 KiB, block limit 64 KiB. Local single-node test is not replicated acceptance. |
| TiDB | Fresh SQL round trip, revision plus NULL | Changed revisions fetch full JSON; publication replaces one shared row through revision CAS | Distributed SQL backed by PD/TiKV; more VM and operational overhead locally. Default namespace limit 4 MiB. Local one-PD/one-TiKV/one-TiDB test is not replicated acceptance. |

Every provider still stores the whole Drive namespace as one publication unit.
Continuous writes invalidate all coordinators' namespace caches, even when
clients update disjoint files. Conditional reads therefore help stable read
workloads most. `ChunkedFs::snapshot` also clones the whole namespace in memory
on reads; this remains CPU/allocation work after provider transfer amplification
is removed. The per-coordinator gate, SQL round trips, shared publication CAS,
JSON framing, and audit output remain potential throughput limits.

Latest main also adds exclusive deferred writeback and MRC3 fenced directory
ownership. Those offer different publication and cache ownership tradeoffs.
Deferred write acknowledgement is not the committed-write measurement used
here: durable synchronization must be measured separately. MRC3 checkouts
require disjoint owned subtrees and retain backing/fence authorization; they
do not automatically make global namespace publication independent per inode.
See [mount ownership](mount-ownership.md) for the contracts.

The next structural improvement is to separate inode/layout metadata and load
only the records needed by an operation, with transactionally consistent
publication and cross-server cache invalidation. That requires a storage format
and consistency design beyond this conditional-read fix.

## Wins, losses, and the next performance experiments

| Implementation | Wins in this workload | Losses and constraints | Initial use case supported by this evidence |
| --- | --- | --- | --- |
| SQLite | Highest successful native committed-write rate; competitive reads; lowest setup cost; no SQL network round trip | Serialized local commits, synchronous connection locks, long write tails; no multi-host shared-file qualification | One host using a reliable local database file, with bounded outstanding writes |
| PGlite | Small Node/WASM deployment and PostgreSQL query interface; conditional query avoids unchanged JSON transfer | Pinned socket adapter fails concurrent setup; durability assertion is not a host crash test | Isolated provider tests pass; this concurrent remote service is not qualified |
| FoundationDB | Comparable best read rate and lowest observed read latency; small unchanged manifest read; transactional KV/shard consistency | Write probe exhausted publication retries; native runtime/deployment requirements, sequential namespace shard loads on change, smaller default namespace limit | Read-heavy workload passed; current sustained competing-write workload failed |
| TiDB | Qualified remote SQL-backed workload, fresh conditional reads, single fenced publication statement | Lower local rates than SQLite; SQL/VM/distributed commit overhead; full namespace row replacement and write contention | SQL-backed server deployment when operational/distributed requirements justify the observed local cost |

Source review explains costs but does not replace measured attribution:

- `ChunkedFs::read_at` takes the coordinator gate and checks metadata before and
  after block I/O. Its `snapshot` clones the full namespace, even for one file.
  Stable reads now avoid provider JSON transfer but retain that in-memory work.
- Writes validate and serialize the namespace and publish one shared revision.
  SQLite additionally acquires an immediate transaction and checks backing file
  identity. TiDB performs a fenced SQL update. FoundationDB reads/replaces
  manifest and shard records transactionally; changed payload reads iterate
  shards sequentially. These choices preserve the current consistency contract.
- The common TiDB namespace is 450,917 bytes: one full serialized publication is
  roughly 110 times a 4 KiB operation, before refreshes, retries, database
  replication, or logs. Conditional reads eliminate unchanged transfer; they do
  not eliminate the changed-revision or publication amplification.
- Disjoint files still share the Drive revision. Ten independently opened
  coordinators therefore compete even when the clients have separate files.

Priority experiments after these baseline runs:

1. Test catalog connection reuse with fresh authorization reads, then measure
   runtime CPU and provider timing at depth 100 separately for reads and
   writes. Measure gate wait, namespace clone/validation/serialization, block
   operations, authority checks, CAS attempts/conflicts, and SQL elapsed time.
   Keep logical service IOPS, backend requests, and physical disk IOPS distinct.
2. Hold 4 KiB operations and queue depth fixed while varying namespace size.
   This tests the remaining whole-namespace clone and publication costs.
3. Hold total queue depth fixed while varying coordinator count (1, 2, 10).
   This distinguishes local serialization from cross-coordinator contention.
4. Compare exclusive writeback and MRC3 directory ownership separately, using
   the actual permission model of the deployment. For writeback report both
   buffered acceptance and barrier-inclusive durable throughput. For MRC3
   include checkout/checkin, fencing, and disjoint owned directories.
5. Redesign per-inode metadata only after timing identifies its contribution.
   Preserve coherent reads, monotonically increasing revisions, authority
   validation, and ambiguous-commit handling through any smaller publication
   scheme. Increasing retries or accepting stale cached metadata can conceal
   contention without improving committed throughput or preserving correctness.

The measured successful read peaks reach about 4.6% (SQLite), 4.9%
(FoundationDB), and 1.8% (TiDB)
of the proposed 100,000 IOPS reference. That reference describes a possible
physical storage capability; these service measurements do not test or disprove
it. No SSD saturation conclusion follows from cache-warm operations at these
payload rates.

## Reproduce

Prepare the existing PGlite test server dependencies using its package manager.
The FoundationDB runner supplies its native client in an owned Docker lane;
it does not install the native client on the host.

```sh
export MOUNT_RS_REMOTE_COMPARISON_OUTPUT_DIR=/tmp/remote-provider-comparison
./scripts/bench-remote-providers.sh sqlite
./scripts/bench-remote-providers.sh pglite
MOUNT_RS_TIDB_TOPOLOGY=single ./scripts/bench-remote-providers.sh tidb
./scripts/bench-remote-providers.sh foundationdb
```

The runner fixes common dataset and timing settings. JSON artifacts identify
provider, topology, identity, completed operations, latency bounds, errors,
cleanup, and fresh verification. Unique sibling artifacts retain separate
invocations. PGlite logs remain in the output directory; its temporary data
folder and owned process are removed on exit. TiDB/FoundationDB owned clusters
are cleaned by their existing harnesses. FoundationDB build cache is retained
under the output directory for subsequent runs. Its prepared client image can
be supplied using `MOUNT_RS_FOUNDATIONDB_RUST_IMAGE`.
