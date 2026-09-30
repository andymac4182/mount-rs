# TiDB directory ownership: allocations and latency

This measures the current working-tree refactor against `1a805764` using actual
durable TiDB 8.5.7. SQL directory names move into the validated logical directory;
structural write planning retains their physical ordinals. Thin borrowed String
keys reduce lookup-set memory. Complete fresh validation, authority serialization,
guard locks, packet checks and COMMIT-before-acknowledgment remain intact.

## Final paired result

Both variants use the existing exact-range membership proof. At 1,000 existing
files, each acknowledged create produced these observations:

| Metric | Copy names | Move names | Observed change |
|---|---:|---:|---:|
| Rust allocation calls | 8,111 | 7,111 | −12.33% |
| Requested allocation bytes | 804,603 | 738,267 | −8.24% |
| Median publication latency | 15.427 ms | 16.078 ms | +4.22% |
| Mean publication latency | 16.423 ms | 17.115 ms | +4.21% |
| p95 publication latency | 22.415 ms | 21.498 ms | −4.09% |
| Inclusive inode SQL mean | 4.875 ms | 5.165 ms | +5.96% |
| Returned inode SQL rows | 1,002 | 1,002 | unchanged |

Allocation reductions are consistent across every sample. Latency remains mixed:
mean and median increased, while p95 decreased. These results establish an
allocation improvement without establishing a latency or throughput improvement.
SQL still performs complete parent work; this change removes neither statements
nor returned rows. Allocation requests remain substantial.

At four files, calls fell 459→455, mean requested bytes increased
77,656.969→77,720.969, and median latency increased 11.297→12.136 ms. The ordinal
vector and planner bookkeeping have a small fixed cost.

## Method and scope

Two release binaries share identical benchmark, allocator and storage sources;
their production differences are exactly `compact.rs` and `compact/indexed.rs`.
The control compiles those two files from `1a805764`. The final candidate uses
the recorded working-tree hashes. Its control binary is reused byte-for-byte
from the preceding source-bound build; reuse and original receipts are recorded.
This is an explicitly pinned working-tree experiment, not a committed release.

Eight trials run in predeclared `A B B A B A A B` order. A is copy names and B is
move names. Each trial runs four warmups and 32 timed creates at each of four and
1,000 files, yielding 128 non-warmup samples per variant/size. Statistics pool
only those samples; p95 uses nearest rank. Trial medians are retained separately.

Only direct provider publication and the identical timeout wrapper are measured.
Seed, capture, pure evaluation, SQL-proxy probe, complete audits, Full resets and
cleanup are outside the windows. Every created intermediate state is compared
with its pure reference before reset. There are 592 intermediate checks, 16
independent final full snapshots after writer retirement, and 16 verified
five-table metadata-key cleanups. All eight children exited successfully and
were reaped. Every sample retained three successful inode SQL read observations
and F+2 returned rows. Unknown writes are never replayed.

The Rust meter counts current-thread allocation/reallocation requests, including
driver and runtime work; server and foreign-thread allocations are excluded.
Requested bytes measure allocation traffic, not live heap or RSS. Instrumentation
affects timing. Inclusive asynchronous SQL spans overlap other spans. Counter
endpoints and returned rows are not physical datastore IOPS.

This publication experiment is metadata-only. It does not exercise RustFS blobs,
QUIC, OS mounts, sustained cluster throughput or the production client count.
Output caps are checked after execution; process receipts prove direct-child
reaping, without an independent process-group absence check.

## Open/Stat/Read/Close diagnosis

A separate existing allocation control includes all four operations and uses
TiDB for metadata **and blocks**. Nonallocating phase snapshots retain the same
overall window, workload, warmup, assertions and 256-call ceiling. Each case is
one observation; these are not latency distribution measurements.

| At 1,000 files | Copy names calls | Move names calls |
|---|---:|---:|
| Open | 300 | 300 |
| Stat | 9,847 | 8,846 |
| Read | 338 | 338 |
| Close | 1 | 1 |
| Complete cycle | 10,486 | 9,485 |

The candidate cycle requested 851,269 bytes versus 892,278. At 128 files, cycle
calls were 2,330→2,201. Both variants failed the unchanged 256-call ceiling in
both cases. Before those assertions, fresh independent providers checked the
anchor, complete root body/listing order, selected inode and target bytes; all
seven owned table families were cleaned and verified empty after retirement.
The allocation target remains unmet.

Source tracing explains the sibling-scaled Stat work: it verifies backing
storage, fetches complete membership and root dentries, reconstructs the root,
then serializes the reconstructed anchor/node for canonical comparison. This
complete-root audit is an existing fstat contract. Stable Open and byte Read use
point receipts; they still incur driver/runtime allocations. A subsequent
optimization can remove reconstructed JSON encoding/reparsing while retaining
the complete audit. Fresh SQL row decoding and validation collections remain
additional allocation sources; exact attribution needs further measurements.

The native cluster byte cycles omit handle Stat, so these cycle counts do not
directly describe native steady read/write performance.

## Qualification and retained evidence

Final-source TiDB/NAPI enabled tests passed **107**, with 102 explicit opt-ins
ignored. All **56 actual TiDB compact controls** passed, including corruption,
fresh lock-wait proofs, packet boundaries, rollback and unknown COMMIT cases.
Strict all-target release Clippy, formatting and whitespace checks passed.
Independent reviews checked ownership/ordinal/name-key semantics and reproduced
the final statistics, all 20 raw output hashes and source/binary bindings.
The separate allocation-ceiling controls remain failed; overall release
qualification is false.

- `final-thin-keys.json.gz` retains every final sample and operation diagnosis.
- `summary.json` provides reproducible statistics and trial medians.
- `qualification.json` binds source, build, fixture and local gate receipts,
  while recording incomplete gates and measurement limits.
- `exploratory-wide-keys.json.gz` preserves the preceding candidate. It removed
  the same 1,000 calls but saved only 0.73% requested bytes. Its results are
  separate and excluded from the final pooling.

Hosted TiDB/RustFS and Windows Node jobs passed for baseline head `1a805764`;
those jobs do not qualify these working-tree changes. Other CI failures remain.
The full formal run ended after a runner shutdown signal: five proofs completed,
the sixth was interrupted, and no counterexample was logged. Full formal
qualification remains incomplete.

The [previous native TiDB/RustFS report](../tidb-rustfs-structural-lock-scope-20260930/README.md)
remains the latest qualified cluster measurement, at `6a60518d`. A fresh native
result for these changes and the ten-server/10,000-client production target
remain outstanding. This code and report remain uncommitted while the existing
allocation gate is investigated; PR 34 is open and unmerged.
