# Independent inode publication

The opt-in `inode_updates` storage setting publishes file content metadata with
an independent revision for each inode. Ordinary writes, existing-file
replacement, and handle/path/open truncation leave the structural namespace
generation unchanged. Directory creation, deletion and rename retain
transactional publication against the complete inode revision map.

## Storage and safety

MRC4 enrollment materializes a complete record for every namespace inode.
File CAS validates physical backing authority, structural generation, the
selected revision, identity, permissions and links; immutable blocks flush
before publication. A proven conflict reloads and reconstructs the candidate.
Ambiguous publication errors poison the coordinator instead of replaying.
Structural publication checks all current records before folding them into the
namespace and atomically advances generation and resets inode revisions.
Malformed or missing records never fall back to the stale namespace base.
Open handles retain the last observed effective contents after unlink.

The setting defaults to false. Explicit enrollment fences older whole-namespace
clients, including generic metadata reads. Enrollment atomically advances the
root revision and stores the base namespace in a strict MRC4 envelope. Older
mode-blind conditional readers must then invalidate their cached revision, and
their plain namespace decoders reject that envelope. Every structural update
retains it. Regressions emulate the exact historical queries/decoders; they do
not execute an independently compiled older binary. It cannot be combined with delegated
publication or writeback. Existing local mounting interfaces are unchanged.
The drive catalog persists this setting inside its existing driver JSON.

```json
{
  "version": 1,
  "driver": {
    "kind": "splitstore",
    "storage": {
      "inode_updates": true,
      "metadata": {"kind": "sqlite", "path": "metadata.db"},
      "blocks": {"kind": "sqlite", "path": "blocks.db"}
    }
  }
}
```

Conditional reads check fresh authority and exact version tokens twice along
the existing read path. Matching tokens omit inode JSON. As with the earlier
namespace conditional API, this cannot audit out-of-band payload modifications
that do not advance the token. Unconditional reads and structural transactions
validate the actual records.

## Measurement

Measurements use release builds with allocation instrumentation, 100 QUIC
clients and 10 service instances in one process, one outstanding request per
client, 100 files of 32 blocks, 4 KiB operations, 3 seconds warmup and 15 seconds
measurement including drain. Audit logging and correctness checking remain
enabled. These warm-cache local runs do not establish disk saturation,
cross-host performance, power-loss durability or confidence intervals.
The previous SQLite comparison is the committed MRC2 resource profile at
`df63611a`. The final inode mode verifies all 100 files with no workload,
verification or cleanup errors.

| SQLite stage | MRC2 operations/s | MRC4 operations/s | MRC2 allocations/operation | MRC4 allocations/operation | MRC2 requested bytes/operation | MRC4 requested bytes/operation |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Read | 6,354 | 5,411 | 185.14 | 198.78 | 765,076 | 765,731 |
| Write | 279 | 681 | 20,394.42 | 372.28 | 5,429,652 | 817,830 |

Writes are **2.44 times faster**, with **98.17% fewer allocations** and
84.94% less requested allocation byte volume. Serialized metadata falls from
766,317 namespace bytes to **4,431 inode bytes per successful write** (about
173 times smaller). The write stage performs 10,428 inode CAS attempts for
10,329 successful operations and 99 proven non-commit retries. Those generic
EAGAIN counts do not distinguish database busy from revision conflicts. It
performs no namespace encoding, snapshot cloning or structural publication.

Whole-process CPU per write falls from 2,233.92 us user + 2,097.01 us system
to 321.02 + 1,150.83 us, a 66.02% reduction. User/system CPU per read is
278.67/1,289.51 us. Reads remain **14.84% slower** than the earlier MRC2 run.
The new inode conditional path checks physical identity and complete authority
fields that the old matching-revision namespace fast path did not check.
Caching full filesystem qualification removes most of the first-run regression
without dropping fresh identity/link checks. The default remains MRC2; this is
a measured write/read tradeoff, not a universal throughput improvement.

SQLite stage counters are reset at begin, so end snapshots represent the stage
**directly**, rather than an end-minus-begin lifetime delta. The final read
stage records exactly three provider SELECTs per read and zero pager
read bytes/read, with no page writes. Writes record about 12.077 statements,
44,779 pager read bytes and 31,221 pager write bytes per success. Page estimates
exclude WAL/checkpoints, syncs and the physical SSD. Catalog SQL is measured
separately. Stable connection IDs support cumulative snapshots and expose
connection churn; closed connections cannot provide final pager attribution.

## Qualified workload summary

| Metadata implementation | Reads/s | Writes/s | Allocations/write | Serialized inode bytes/write |
| --- | ---: | ---: | ---: | ---: |
| SQLite | 5,411 | 681 | 372.28 | 4,431 |
| TiDB, one PD/TiKV/TiDB | 2,398 | 816 | 674.62 | 4,463 |
| Native FoundationDB, Linux VM | 4,669 | 2,061 | 474.73 | 4,655 |
| PgLite | Not ranked | Not ranked | Not measured | Real-server contract fixture passes |

All three timed implementations verify 100 files with zero stage errors and no
whole-namespace serialization during file writes. This is a comparison of the
measured complete configurations, not a controlled backend ranking: native FDB
uses a different client OS. TiDB reads improve 56.02% against its preceding
1,537/s resource run, though allocations/read rise from 290.03 to 430.70 because
the joined inode path performs additional authority/record handling.

## Native FoundationDB results

The feature-on native lane passes strict Clippy for all provider targets, five
ordinary contract tests, the delegated contract, the new inode transaction
fixture and terminal network shutdown. Every load artifact freshly verifies
all 100 files, with no workload, verification or cleanup errors.

| Stage | Operations/s | Allocations/operation | Requested bytes/operation | User + system CPU us/operation |
| --- | ---: | ---: | ---: | ---: |
| Read, separate tokens and parallel checks | 4,669 | 255.83 | 780,217 | 648.98 |
| Write, independent inode CAS | 2,061 | 474.73 | 852,188 | 863.62 |

The write stage encodes about 4,655 bytes of inode JSON per successful
operation, with no whole-namespace serialization. Native FDB runs inside
Linux Docker; SQLite/TiDB client processes run on macOS, so these rows do not
isolate backend differences from OS/runtime differences.

Read tokens remove full-record fetches: exported read traffic falls from
13,622 bytes/operation in the initial diagnostic run to about 4,365 in the
final second read stage. Concurrent
authority reads improve reads from 3,028/s to 4,669/s. The prior MRC2 native
resource run reached 6,787/s; MRC4 is still 31.21% slower for this read workload.
It performs two fresh conditional checks, each with six authority GETs and one
inode-token GET, plus the block GET. Exported counters measure about 15.30
read requests and 3.06 transactions per logical read in that second stage; **three transactions is
not three KV reads**. This remaining authority lookup amplification is a real
read cost, despite its small returned byte volume. The independent first read
stage reaches 4,669/s; the second read reaches 4,566/s. The first exported status
window carries setup/export lag (19.29 requests and 7,462 returned bytes/read),
so the smaller second-stage counters should not be presented as first-stage
measurements. The earlier whole-namespace
write artifact had errors and is not a qualified throughput comparison.

Status exports have lag/background work; CPU and RSS gauges are boundary samples,
not measured average utilization. Device counters describe the local Linux VM,
not physical Mac SSD IOPS. Foreign C allocator memory is unavailable to the
Rust allocation wrapper. The token/CAS fixture also tests missing and malformed
tokens, token/body mismatches and complete membership during snapshots.

## Backend tradeoffs

- SQLite removes unrelated inode revision conflicts but still serializes
  physical writers. Compact authority reads need a covering index to avoid
  touching the namespace overflow pages. Local filesystem qualification is
  cached only after its full check and a final physical identity check. Each
  hot read still checks both path identities and link count before and after
  the SQL probe; writes and full inspection retain full qualification.
- TiDB file transactions lock only the selected inode and use fresh compact
  authority reads under READ COMMITTED. Structural transactions lock the
  namespace root and the complete inode set. Its covering authority index
  prevents large namespace fetches on the file path.
- Native FoundationDB file transactions conflict on authority reads and the
  selected record, while writing only that record. Structural transactions
  protect the complete prefix scan, including membership changes. Small version
  tokens are stored separately from full records and updated atomically with
  them, so exact conditional reads avoid fetching inode JSON. Independent
  authority and token reads are issued together in one transaction. Native value
  and transaction limits continue to apply.
- PgLite uses the same independent record contract. Its bounded real-server
  fixture qualifies this path; the existing 100-client pgwire protocol failure
  prevents a valid comparative throughput ranking.

A file record still contains its entire extent layout, so its encoding and
allocation costs grow with that file. Structural operations still scan, fold
and publish the whole namespace. The refactor does not change the JSON RPC
byte-array network expansion or per-request catalog checks.

## Verification

The TiDB real transaction fixture passes, including independent file CAS,
same-inode conflicts, structural folding, old-client fencing, missing/corrupt
guards, and an EXPLAIN assertion that compact authority reads are covered.
The focused chunked runtime integration suite passes all five tests, including
replacements, path/handle/open truncation, same-file append and unlinked handles.
Independent source review found three race/corruption issues, which were fixed;
the final review reports no further material findings. The bounded PgLite real-server fixture also passes. Kani 0.68.0 / CBMC
6.11.0 proves the actual production attribute eligibility helper with arbitrary
full-range Stats pairs: 0/197 failed checks and 4/4 covers. It excludes layouts,
errors, provider transactions, concurrency and crash durability. The earlier
full-validator attempt was stopped without a result due to heap/error paths.
Final gates on the completed source pass:

- `cargo-shared fmt --all -- --check`.
- `cargo-shared clippy --workspace --all-targets --locked --offline --features allocation-profiling -- -D warnings`.
- `cargo-shared test --workspace --all-targets --locked --offline --features allocation-profiling`: 1,256 passed, zero failed, 103 ignored across 155 targets.
- Native FoundationDB feature-enabled all-target strict Clippy, baseline,
  delegated, inode and terminal fixtures in the Linux client container.
- Actual TiDB transaction/terminal fixtures and final 100-client workload;
  actual PgLite focused inode fixture; final SQLite 100-client workload.
- A fresh Kani run of `inode_publication_preserves_structural_attributes`:
  verification successful, 0/197 failed checks, 4/4 covers.
- Shell syntax, `git diff --check`, and parsing all 14 JSON evidence artifacts.

Commands use the isolated Cargo target directories. Ignored external-system
workspace tests are not claimed as passing; the named backend fixtures were
run explicitly. The new Kani harness is bounded and does not constitute formal
verification of the distributed storage protocol.

## TiDB datastore profile

The TiDB v8.5.7 fixture uses one PD, TiKV and TiDB instance in the local Docker
Linux VM. It is a single-node smoke result, not replicated acceptance.

| Stage | Operations/s | SQL statements/operation | TiDB CPU seconds/operation | TiKV CPU seconds/operation | TiKV VM block writes/operation | TiKV VM write bytes/operation |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Read | 2,398 | 3 | 0.001160 | 0.000527 | 0.00105 | 3.18 |
| Write | 816 | 10 | 0.002904 | 0.001785 | 3.0255 | 28,215 |

The datastore windows are 15.583 and 15.551 seconds, slightly wider than the
logical stages, and include background traffic. Warm stages issue zero VM
block reads; these counts measure neither the physical Mac SSD nor its claimed
100,000 IOPS. Read-end RSS: PD 152 MB, TiDB 664 MB and TiKV 2.99 GB.
Root namespace JSON is absent from the hot file-write path, but transaction
protocol, SQL execution, block insertion, WAL and compaction still cost work.

Evidence: `benchmarks/remote-inode-publication-20260924/tidb.json` and
`tidb-{read,write}-datastore.json`. SQLite diagnostic artifacts retain the
initial read regression and intermediate covering-index/filesystem-cache fixes.
The initial native FoundationDB artifact records its full-guard read regression.
SQLite snapshots now identify connections; compute deltas by matching IDs,
when using cumulative samples. This harness resets at begin, so use its end
sample directly. Closed connections have no final pager
sample and must be reported as incomplete attribution rather than subtracting
aggregate lifetime totals.
