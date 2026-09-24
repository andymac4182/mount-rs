# Exclusive ownership benchmark

[Exclusive results](RESULTS.md), [delegated results](RESULTS-delegated.md),
and [raw primary trials](results-macos-arm64.json)
are retained alongside this harness. The [concurrent-build run](results-concurrent-build-macos-arm64.json)
is retained separately because other workspace builds were active during it.

Run from this checkout with `benchmarks/ownership/run.sh [output.json]`.
The script uses `scripts/cargo-shared` with a release build and never assumes
a checkout-local target directory. Override `BENCH_SAMPLES` (default 7) and
`BENCH_OPERATIONS` (default 128) to extend the run. Each case first discards one
warmup trial, then creates fresh stores for every measured trial.

The 16 cases compare exclusive ownership with legacy write-through behavior
(the unchanged default constructor, `writeback=false`) against explicit exclusive writeback. Identical seeded
random 4 KiB writes target a preallocated 256 KiB file. Namespace transactions
create, close, rename and unlink one empty file. Both workloads call `syncfs`
after either every operation or every 16 operations; the final partial batch
also syncs. Timings include all workload operations and these barriers. Setup,
close of the page handle, shutdown and verified reopen are excluded. Namespace
handle close is part of its workload. Every trial checks the entire page file
and absence of temporary namespace entries after shutdown and reopen.

Raw JSON records host, toolchain and relevant source SHA-256 fingerprints and
contains every total duration, every transaction duration, nearest-rank
p50/p95/p99 distributions and operation counts for metadata load, publish,
renew and flush, and block put, get and flush. Counters reset after setup and
are captured at the timed boundary, before verification. Provider durability
and publish-includes-flush assertions are forwarded unchanged by the wrappers.
Counts describe provider API calls, not underlying SQL statement or device I/O
counts. Memory results are volatile; SQLite uses file-backed metadata and block
providers with their existing durability configuration. Seven trials make high
percentiles equal to the slowest sample; use more samples for tail analysis.

There is no injected latency. Results measure this host and these local
providers. Reduced publication calls are evidence for a narrower metadata
operation budget; they do not establish actual network speedups, power-loss
durability, native FUSE SQLite behavior, or independent-mount shared SQLite.

## Directory delegation suite

Use `BENCH_SUITE=delegated benchmarks/ownership/run.sh output.json` for the six
SQLite cases, or `BENCH_SUITE=all` for both suites. Legacy MRC2 uses
`with_concurrent_writes(true)`; MRC3 uses explicit Shared ownership. Memory
MRC3 is unsupported and is deliberately excluded.

Each coordinator opens independent SQLite metadata and block connections;
verification opens a fresh provider pair after shutdown. The disjoint-client
workload starts two OS threads at a barrier. Each client
owns a separate directory in MRC3 and writes 128 deterministic 4 KiB pages by default,
syncing every 16 writes. Both modes publish immediately; publication counters
include attempted CAS operations and therefore can exceed successful writes
when the two clients conflict on the whole-namespace revision. Successful
publication counts are separate and asserted to equal the combined page writes.
Counts include
conditional metadata loads, delegation reads, checkout/checkin and backing
verification, alongside the original provider counters.

Handoff alternates access to `/a`: the current client writes and closes a page
handle, then MRC3 checks in and the receiving client checks out the directory.
Legacy MRC2 has no ownership handoff: its comparison performs a sender sync.
Both receiving clients then open, stat, read and compare the complete 256 KiB file. Raw
`operation_ms` includes checkin/checkout or sender sync plus that receiving
open/stat/read/close; total trial time also includes the preceding writes.

Same-directory admission measures an overlapping checkout denial in MRC3
versus an accepted open/stat/full read/close in legacy MRC2. These outcomes have
different semantics; a timing difference does not imply equivalent safety.
The raw data records every admission outcome. All workloads compare complete
contents of both files after shutdown and fresh driver reopen. Driver handoff
is measured directly; this is not native mount cache or transport handoff
qualification. No artificial latency or network performance claim is made.

Historical phase1 [raw trials](results-phase1-macos-arm64.json) and
[report](RESULTS-phase1.md) retain their original source provenance.

The [preliminary delegation run](results-delegation-preliminary-macos-arm64.json)
verifies full-size workloads while task builds or VM activity may be present;
it is retained separately from final performance evidence.
