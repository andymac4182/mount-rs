# Remote drive I/O amplification investigation

## Measurement domains

The common service workload has 100 clients, ten server coordinators in one process, 100 files with 32 distinct 4 KiB blocks each, Q100, three seconds of warmup and fifteen seconds measured plus drain. Audit logging remains enabled. Instrumentation is opt-in (`MOUNT_RS_PROFILE_IO=1`, service `io-profiling` feature). Spans count attempted calls and inclusive wall time, including waits; overlapping durations must not be summed into CPU percentages. Backend and host observers run outside the workload timer. Pooled catalog pager samples describe connection activity and can include initialization or the previous diagnostic `PRAGMA page_size`; they are not exact per-request physical I/O.

Logical drive operations, SQL statements, KV requests, SQLite pager pages, and host storage-driver operations are different units. Pager misses may hit the OS cache; pager writes exclude WAL/checkpoint and filesystem traffic. Host counters include all applications and Docker. None of these establish NAND operations or the laptop's advertised 100,000 IOPS limit.

## SQLite baseline before optimizations

All 100 fresh handles verified; both stages had zero errors and cleanup succeeded.

| Per completed drive operation | Read | Write |
|---|---:|---:|
| Drive operations/sec | 4,181 | 274 |
| Datastore SQL statements | 3.000 | 20.612 |
| Conditional metadata checks | 2.000 | 2.693 |
| Block gets | 1.000 | 1.693 |
| Block puts | 0 | 1.693 |
| Publication attempts | 0 | 1.693 |
| Publication conflicts | 0 | 0.693 |
| Namespace bytes serialized | 0 | 758,133 |
| Namespace bytes returned | 0 | 384,805 |
| Datastore pager misses | 0.000064 | 172.106 |
| Datastore pager writes | 0 | 11.367 |

A full 4 KiB overwrite reads the old block on every publication attempt even though every byte is replaced. A shared namespace CAS retries unrelated file writes, reserializing about 448 KiB per attempt. Failed attempts also put and flush immutable data again. These are distinct sources of amplification.

The read path refreshes metadata twice and clones all 101 namespace nodes once. Mean inclusive filesystem gate wait totals 13.97 ms per read. Actual block get averages 0.088 ms. Catalog loading averages 6.80 ms: opening/configuring 4.55 ms, querying 0.85 ms, decoding 0.01 ms, closing 1.29 ms. Catalog connections are recreated for every authorization check, producing three catalog pager misses per request in addition to the datastore counters above. Audit averages 2.11 ms per read. The wire performs two JSON encodes and decodes per operation; aggregate encode/decode bytes are about 14.5 KiB per 4 KiB operation because JSON represents the payload byte array as numbers. Encoding and decoding total 0.12 ms per read.

The `filesystem.write_fallback` read-stage counter in the original raw baseline was incorrectly placed in the handle read entry; exclude that counter. The recorder placement was corrected before subsequent runs. Other counters use separate events and remain valid.

### Direct datastore baseline

The fixed-count SQLite diagnostic dispatches 3,200 operations sequentially over ten round-robin connections; this is not the common concurrent service workload. Provider block gets reach 76,072/sec on the first ten-connection pass and 71,610/sec on repetition, one SQL statement each. Provider puts reach 1,753/sec, four SQL statements and 4.54 pager writes each. Unchanged revision checks reach 76,164/sec. Full namespace load/JSON decode reaches only 155/sec with zero pager misses; same-document publication under the legacy writer lease reaches 80/sec (a different protocol from the concurrent MRC2 service). Thus warm block retrieval is much faster than the mounted path, while full-document publication and decoding remain expensive even without physical reads.

### Actual counter rates

At 4,181 completed reads/sec the datastore executes 12,544 SQL statements/sec and almost no pager reads. At 274 completed writes/sec it executes 5,656 SQL statements/sec, about 47,228 pager misses/sec and 3,119 pager writes/sec. These counters expose substantial amplification below the logical drive API.

Phase-aligned host storage-driver counters measured 3,714 reads/sec and 4,116 writes/sec during the read stage, and 11,680 reads/sec and 6,759 writes/sec during the write stage. These are shared host operation rates: other applications, OS cache effects, Docker and audit output contribute. The read datastore itself reports zero pager writes, so the host write rate must not be attributed to drive data writes. An adjacent idle control is required before interpreting headroom. [Apple defines these cumulative counters as operations processed by the storage driver](https://developer.apple.com/documentation/iokit/kioblockstoragedriverstatisticswriteskey).

## Verified fixes and SQLite remeasurement

Full coverage of a resulting fixed-size chunk now skips the old data read, including short EOF chunks. Partial boundary chunks retain old reads. The regression failed first (one get instead of zero), then 52 chunked tests passed; the extended 12 KiB cross-chunk case preserves both 2 KiB edges and expects exactly two old gets.

The Unix catalog now reuses eight connections, queries fresh grant/policy data on every authorization, and checks backing-file identity before and after the operation. There is no permission TTL cache. New tests cover connection reuse, external revocation and replacement rejection. Non-Unix retains fresh connections. The SQLite trace registry also prunes expired weak entries when registering connections, preventing unsampled open/close cycles from accumulating entries.

The same Q100 SQLite workload after the overwrite and catalog fixes passed with 100 freshly verified files and zero errors:

| Measure | Before | After |
|---|---:|---:|
| Read operations/sec | 4,181 | 6,310 |
| Write operations/sec | 274 | 284 |
| Catalog mean inclusive milliseconds/read | 6.80 | 1.12 |
| Catalog pager misses/read | 3 | 0 |
| Old block gets/full write | 1.69 | 0 |
| Datastore SQL/write | 20.61 | 19.30 |
| Datastore pager misses/write | 172.11 | 167.61 |

Reads improve 51% in these samples. The small write-rate difference does not establish a material write win; publication and contention dominate. Namespace serialization still averages 775 KB per completed write. Adjacent no-workload controls show 17,459 and 17,496 host read operations/sec, versus 10,699 during the read stage and 17,873 during writes. Background load and prior writeback make exact subtraction inappropriate; host rates cannot be assigned to a provider.

A separate five-second sample of only the owned service process confirms worker stacks waiting inside stderr locking and filesystem `stat`, with additional codec/allocation work. Thread-stack samples include sleeping threads and are not exclusive CPU percentages; its throughput is diagnostic. The legacy audit `Value` formatter produced 66 writer fragments for the regression record while formatting under the stderr lock. Preparing the JSON string before emitting it reduces this to at most two fragments, retaining synchronous output and identical success/error fields. A final Q100 remeasurement after the audit fix passed all 100 fresh-file checks with zero errors: 6,454 reads/sec and 296 writes/sec. Inclusive audit time averaged 0.0147 ms/read and 0.0203 ms/write. Read gate wait fell to 0.135 ms/request in this sample; write gate wait remained 299 ms/request. The modest throughput increase from the preceding run is not a strong standalone audit throughput claim. Full writes still serialized 706 KB of namespace JSON per success despite zero old-block reads.

## TiDB: amplification inside the base store

An owned TiDB/TiKV/PD v8.5.7 single topology directly completed 3,200 operations per stage with zero failures: 10,746 block puts/sec, 18,792 block gets/sec, and 18,204 unchanged revision checks/sec on the small direct fixture. These short bursts use 100 tasks and bypass drive publication. Put operations produce 3,200 actual inserts plus 856 SELECT/SET/USE housekeeping statements, approximately 1.27 SQL statements/op, and about two TiDB-to-TiKV requests/op. Gets and revision checks produce approximately one TiDB-to-TiKV request/op. SQL executor statements and MySQL protocol commands (`Query`, `StmtPrepare`, `StmtExecute`, `Quit`) are counted separately.

Two common read stages passed at 2,215 and 2,238 operations/sec: exactly three datastore SQL statements/read and about three TiDB-to-TiKV requests/read. The write stage failed after 916 successes and one `EAGAIN`; 56.2 completed writes/sec is diagnostic, with fresh verification skipped. It generated 4,203 publication attempts, 3,287 conflicts, 28,505 SQL statements and 28,959 TiDB-to-TiKV requests. Attempts from the failed operation are included. It serialized 1.895 GB of namespace JSON and returned 1.525 GB across the stage. Raising retry limits would hide the failure without eliminating that traffic.

### Hidden read byte amplification confirmed

A separate successful read-only Q100 stage verified all 100 files and completed 21,923 reads. The observer measured **38.176 GB transmitted by TiKV and 38.188 GB received by TiDB**, approximately **1.742 MB per 4 KiB logical read**, or **425 times the payload**. Datastore SQL is still exactly three statements/read; the application receives no unchanged namespace JSON. TiKV cgroup block reads are zero, so this is cache-resident internal network traffic, not physical disk reads. Gross eth0 counters exclude loopback, include protocol/background/scrape traffic, and agree between sender and receiver.

The previous conditional `CASE WHEN revision=? THEN NULL ELSE namespace END` shrank the SQL response to the service but still read the large primary row value from TiKV. A regression first failed on actual TiDB with missing index error 1176. The provider now migrates an idempotent `(volume_key, revision)` secondary index and reads only `revision` through that index on the unchanged path. Its ignored real-TiDB plan test explains the exact production SQL constant and confirms `IndexReader` without `TableReader`. Changed revisions perform a second full, validated load; missing and invalid revisions retain their error behavior. The real provider contract and plan gate passed after the change.

A successful post-index Q100 read stage verified 33,775 reads at 2,247 operations/sec with zero errors. TiKV eth0 transmitted **160.857 MB**, or **4,762 bytes/read**, compared with 1,741,373 bytes/read before, a **366-fold reduction**. TiDB eth0 received 185.907 MB, or 5,504 bytes/read, a 316-fold reduction. Actual SQL remained exactly three statements/read and TiDB-to-TiKV requests stayed about three/read. TiKV cgroup reads were zero in both stages. Gross eth0 includes metric scrapes and background traffic, but the closely matched TiKV transmit/TiDB receive deltas and the exact plan support the covering-index explanation. The service binary also includes a concurrent audit-formatting change, so the observed 1,457-to-2,247 operations/sec difference is a combined-code result; the byte reduction is the specific index evidence.

The bounded write-cost diagnostic used the same disposable TiDB and binary for three 200-update stages, each replacing a 448,000-byte namespace row. Index DDL was outside stage timers. Unindexed A, indexed, and unindexed B reached 170.5, 152.5, and 158.0 updates/sec. Each issued exactly 200 actual SQL statements; TiDB-to-TiKV request counts were 412, 419, and 425. TiKV cgroup write bytes were 90.9, 97.7, and 92.5 MB. The indexed rate is about 7% below the mean of the two unindexed brackets, but this short serial UPDATE experiment does not establish a stable write penalty or qualify the failed Q100 drive-write stage.

## FoundationDB: native baseline and manifest contention

The owned native FoundationDB 7.4.7 cluster used verified `ssd-2` storage, single redundancy and a run-labeled Docker data volume. The direct 4 KiB block baseline used 100 tasks, ten native provider handles and Q100: **42,169 reads/sec** and **8,164 immutable puts/sec**, with zero failures. Every newly written block was read back and deleted. This baseline bypasses the SDK, filesystem, protocol and audit; immutable puts omit namespace pointer publication and are not durable drive commits.

The profiled common Q100 read completed 83,449 operations at **5,559 reads/sec**, p99 histogram upper bound 32.768 ms, with zero errors and all 100 files verified. A second read reached 5,516/sec. Exact process counters show one 4 KiB block GET and two conditional manifest checks per read, with zero namespace payload returns or serializations. The native exported snapshots measured approximately three KV reads and 4.1 KiB returned per completed read. Unlike a changed namespace load, an unchanged manifest check reads no namespace shards.

The read profile's largest inclusive delay was filesystem gate waiting, averaging 14.974 ms/request; metadata refresh averaged 1.462 ms across two checks, block GET 0.773 ms, and cloning 101 in-memory nodes 143 microseconds. Wire decode averaged 189 microseconds, authorization 109 microseconds and audit 18.47 microseconds. These nested wall times include waits, overlap across requests and must not be summed as exclusive CPU time. The validated Linux integration-test process issued 2.641 write syscalls/read after audit serialization was consolidated.

The Q100 write stage **failed** with 2,962 successes and 27 `EAGAIN` errors, zero timeouts and fresh content verification skipped. Its 190.6 completed writes/sec and 16.777-second p99 histogram upper bound are diagnostic, not qualified write throughput. It made 6,890 block puts, flushes and publication attempts, with 3,928 known manifest CAS conflicts. Full namespace payloads returned totaled 1.893 GB and serialization totaled 3.239 GB, approximately 1.73 MB of namespace materialization per successful write; failed attempts contribute to the numerator. No old-block GETs occurred for these complete chunk overwrites. Independent servers updating disjoint files still contend on the same namespace revision; bounded retries eventually report `EAGAIN`.

Native snapshot deltas reported roughly 16,550 KV reads/sec during the first service read. The failed write reported 18,764 KV reads/sec, 11,420 KV writes/sec and 381 committed transactions/sec. Native transaction conflicts are a different layer from manifest CAS rejection: only 14 were exported for the failed write. Status export lag is visible: direct read snapshots include 3,202 earlier seed commits and later snapshots include preceding-phase traffic, so native counts and per-operation ratios are approximate diagnostic intervals. Exact application/provider profile counts remain separate. [FoundationDB documents these logical counters](https://apple.github.io/foundationdb/mr-status.html) and [its internal latency probes](https://apple.github.io/foundationdb/administration.html), which are not application percentiles.

Linux VM device `vda` deltas measured approximately 14,103 block writes/sec during direct puts and 2,602 writes/sec during the failed service write. Warm service reads had zero device reads and approximately 137 writes/sec from background/log activity. These cover the shared Docker VM device, not one provider or the physical macOS SSD; `vda1` overlaps `vda` and must not be added. The direct harness and native provider, delegation and network shutdown gates passed with outer exit 0. The service harness exited 101 after the write failure and skipped its following shutdown gate; owned containers, networks and labeled data volumes were cleaned in both cases.

## PgLite direct baseline and qualification

A persisted direct-engine burst of 1,000 operations per stage reaches 3,674 reads/sec and 3,496 upserts/sec for 4 KiB payloads. Flushing backend statistics and clearing the cached snapshot outside the timer reconciles all seven stages to exactly 1,000 workload commits; an empty control reports exactly three observer commits and no target-table pages or WAL. [PostgreSQL's statistics implementation](https://github.com/postgres/postgres/blob/REL_18_STABLE/src/backend/utils/activity/pgstat.c) distinguishes flushing statistics from clearing the reader snapshot. The actual embedded identity is PostgreSQL 18.3 in PgLite 0.5.8.

Each warm read performs one heap and two index cache hits with zero backend page reads. Each upsert performs about 5.21 cache hits and one backend WAL write averaging 8,471 bytes, approximately 2.07 times the 4 KiB payload. Logical WAL records average 271 bytes for this compressible repeated-byte fixture. Backend WAL writes, logical WAL bytes and physical filesystem operations are different counters. There are no observed PostgreSQL fsync calls; persisted clean shutdown does not establish power-loss durability. This short direct-engine burst bypasses the filesystem/publication protocol and is not a sustained multi-client limit.

The common 100-client SDK workload fails during setup with prepared-statement/bind protocol errors. A ten-client probe passed, but the larger reproduction is decisive: 100 physical socket connections, fragmented 448 KiB binds and varying parameter counts produced 70 errors and exactly 70 error frames out of 200 unnamed-statement attempts. SQLSTATE `08P01` reports binds referencing a different parameter count in the shared unnamed statement. The same probe with unique statement names passed 200 attempts. This localizes a protocol/session-sharing failure; unique statement names alone do not prove independent transaction, portal or multi-process session isolation. No production adapter fix or service throughput ranking is claimed.

## Findings and remaining work

| Layer | Measured amplification / wait | Status |
|---|---|---|
| TiDB unchanged metadata | 1.74 MB internal traffic per 4 KiB read | Covering index reduces TiKV TX to 4.76 KiB/read; three SQL statements remain |
| Authorization catalog | Open/configure/close on every RPC, 6.80 ms inclusive/read | Unix connection reuse with fresh authorization and backing identity checks |
| Full fixed-chunk overwrite | Old data GET on every CAS attempt | Removed for complete chunks; partial edges retain reads |
| Audit | 66 formatter fragments under the stderr lock | Preformat before lock, at most two fragments; synchronous records preserved |
| Filesystem read | Clone all 101 namespace nodes, then the selected layout | Still present; measured 0.11–0.14 ms/read and grows with namespace size |
| Wire | Approximately 14.5 KiB aggregate encode/decode bytes per 4 KiB RPC | Still present; numeric JSON byte arrays expand payloads |
| Drive publication | Shared namespace revision, whole-document serialization and retry | Dominant remaining write bottleneck; TiDB/FDB service writes can hit `EAGAIN` |
| PgLite socket adapter | 70/200 unnamed prepared-statement errors with 100 connections | Reproduced; service performance remains unqualified |

The next write improvement needs a metadata format with a smaller transactional conflict domain, such as per-inode records and atomic directory updates, while preserving publication and delegation authority. Merely extending the CAS retry budget would retain both amplification and long-tail latency. Reusing immutable staged blocks across metadata rebases could remove repeated puts/flushes, but must retain backing/lease validation. These are architectural follow-ups, not implemented in this patch.

A smaller read follow-up is selecting the requested MRC2 node under the state lock instead of cloning the full namespace, retaining both fresh metadata checks and orphan/failure semantics. Binary payload encoding could also reduce measured wire expansion. Neither is needed to establish the datastore byte-amplification fix measured here.

## Reproduction and raw evidence

- [SQLite direct baseline](benchmarks/remote-io-amplification-20260924/sqlite-direct.json)
- [SQLite service before optimization](benchmarks/remote-io-amplification-20260924/sqlite-service-before.json)
- [SQLite after overwrite/catalog fixes](benchmarks/remote-io-amplification-20260924/sqlite-service-after.json)
- [SQLite final audit remeasurement](benchmarks/remote-io-amplification-20260924/sqlite-service-final.json)
- [SQLite final host counters](benchmarks/remote-io-amplification-20260924/sqlite-host-final.json)
- [Host counters and idle controls](benchmarks/remote-io-amplification-20260924/sqlite-host-after-and-controls.json)
- [TiDB direct baseline](benchmarks/remote-io-amplification-20260924/tidb-direct.json)
- [TiDB internal network proof](benchmarks/remote-io-amplification-20260924/tidb-net-before.json)
- [TiDB read after covering index](benchmarks/remote-io-amplification-20260924/tidb-net-after.json)
- [TiDB network counters before and after](benchmarks/remote-io-amplification-20260924/tidb-network-counters-before-after.json)
- [TiDB indexed and unindexed write-cost diagnostic](benchmarks/remote-io-amplification-20260924/tidb-index-write-cost.json)
- [Selected raw TiDB backend counters and deltas](benchmarks/remote-io-amplification-20260924/tidb-selected-counters.json)
- [FoundationDB direct block baseline](benchmarks/remote-io-amplification-20260924/foundationdb-direct-block.json)
- [FoundationDB verified Q100 read](benchmarks/remote-io-amplification-20260924/foundationdb-read.json)
- [FoundationDB Q100 write failure and profile](benchmarks/remote-io-amplification-20260924/foundationdb-read-write.json)
- [Selected raw FoundationDB snapshots and counter deltas](benchmarks/remote-io-amplification-20260924/foundationdb-selected-counters.json)
- [PgLite flushed direct baseline](benchmarks/remote-io-amplification-20260924/pglite-flush-direct.json)
- [PgLite 100-connection reproduction](benchmarks/remote-io-amplification-20260924/pglite-100client-probe.json)

Run the common comparison with `MOUNT_RS_PROFILE_IO=1 scripts/bench-remote-providers.sh <provider>` and a retained `MOUNT_RS_REMOTE_COMPARISON_OUTPUT_DIR`. Depths can be restricted with `MOUNT_RS_REMOTE_COMPARISON_READ_DEPTHS=1` and `MOUNT_RS_REMOTE_COMPARISON_READ_WRITE_DEPTHS=1`. TiDB and FoundationDB stage observers retain backend cumulative snapshots and phase deltas; a failed observer invalidates the measurement rather than supplying zero counters.

For the native FoundationDB reproduction, set a retained `MOUNT_RS_FOUNDATIONDB_BENCH_OUTPUT_DIR` and `MOUNT_RS_DATASTORE_STAGE_OBSERVER=/workspace/scripts/foundationdb-stage-observer.sh`, then run `scripts/bench-remote-foundationdb.sh` with `MOUNT_RS_REMOTE_TIDB_SATURATION_DEPTHS=1`. The harness enables `io-profiling` and `MOUNT_RS_PROFILE_IO=1`. These captures used the locally prepared `MOUNT_RS_FOUNDATIONDB_RUST_IMAGE=mount-rs-foundationdb-bench-client:rust1.95`; the default Rust 1.95 Bookworm client installs the same required clang/libclang dependencies. Run the direct baseline separately with `MOUNT_RS_FOUNDATIONDB_DIRECT_BASELINE=1`; preserve serial performance slots. Reduce a stage outside timing with `python3 scripts/foundationdb-counter-deltas.py <output>/counters <stage-id> --output <retained-delta.json>`.

## Validation

Fresh final gates passed: workspace formatting; strict workspace/all-target Clippy with `mount-rs-service/io-profiling`; 1,227 workspace tests across 153 targets, zero failures and 100 ignored; the opt-in SQLite trace reset/privacy/lifetime/weak-registry regression; four datastore observer tests; shell syntax and Node syntax checks; and all 21 canonical JSON artifacts. The first sandboxed workspace run stopped on Unix-socket `EPERM`; rerunning with owned local socket access passed. The final SQLite Q100 read/write gate passed, as did actual TiDB provider/index/read/write-cost diagnostics and the native FoundationDB direct/provider/delegation gates. Native FoundationDB and TiDB service write failures remain explicitly diagnostic. No new formal-verification or power-loss-durability result is claimed by these measurements.
