# TiDB packet classification comparison — 2026-09-30

## Result

The packet header eligibility guard removes allocation growth with directory
size from the measured borrowed-root read cycle. At 1,000 files, allocation
requests fall **83.30%**, from approximately 5,090 to 850. The complete cycle
still allocates and fails the original 256-call limit.

**Latency is worse in this observation.** Median latency rises about 3% at both
sizes, and p95 rises 12.29% / 33.43%. This establishes an allocation improvement;
it does not establish a speed or throughput improvement.

| Files | Metric | Guard disabled A | Guard enabled B | Change |
| --- | --- | ---: | ---: | ---: |
| 128 | Mean allocation requests/cycle | 1,603 | 851 | −46.91% |
| 128 | Mean requested bytes/cycle | 136,184 | 117,948 | −13.39% |
| 128 | Median cycle latency | 11.032 ms | 11.415 ms | +3.48% |
| 128 | Mean cycle latency | 11.891 ms | 12.436 ms | +4.58% |
| 128 | Nearest-rank p95 latency | 15.664 ms | 17.590 ms | +12.29% |
| 1,000 | Mean allocation requests/cycle | 5,090.19 | 850.19 | −83.30% |
| 1,000 | Mean requested bytes/cycle | 218,961 | 117,016 | −46.56% |
| 1,000 | Median cycle latency | 15.123 ms | 15.589 ms | +3.08% |
| 1,000 | Mean cycle latency | 17.451 ms | 19.592 ms | +12.27% |
| 1,000 | Nearest-rank p95 latency | 32.468 ms | 43.321 ms | +33.43% |

Statistics pool 128 timed samples per role and directory size, across four
independent process trials per role. These are descriptive observations on a
shared local laptop. No confidence intervals or attribution of the latency
regression are claimed; per-trial statistics and all samples are retained.

## Cause and isolated regression

The pinned MySQL driver attempted OK/EOF parsing and then ERR parsing for
every ordinary binary row. Both parsers rejected its header, constructing
discarded heap-backed errors. Two complete root streams therefore contributed
four allocations per file even after owned Row/Value decoding was removed.

The new guard checks the context's pinned OK/EOF header and ERR header before
those parsers. Matching headers and empty packets retain the original parser
paths. An actual failing regression recorded **32 calls / 768 requested bytes
for 16 valid binary rows**; the corrected classifier records **zero / zero**.
Its allocating owned-decoder control still records 16 calls / 384 bytes.
The classifier test excludes sockets, executor activity and surrounding driver
work. It cannot establish zero allocations for the full provider.

## Workload and comparison

Actual durable local **TiDB 8.5.7 supplies both metadata and blocks**. One
ChunkedFs instance performs Open → handle Stat → positional Read → Close on
indexed MRC5 metadata, with compact inode updates and reversed root order.
The selected file contains the 13-byte `selected-read` payload; siblings share
its immutable block. This is a provider benchmark, not an OS mount, RustFS
workload, QUIC transport or server cluster.

Both roles use borrowed root reads. The sole changed source input is
`vendor/mount-rs-mysql-async/src/conn/mod.rs`: A disables only the new eligibility
guard with `None::<&u8>`, and B enables it. Both copied release binaries have
locked builds and 630-input source manifests, including all fork Rust sources
and both lockfiles. Candidate source is restored after the control build and
matches after every settled trial and final qualification. An initial
control-only compilation used an unsupported Rust 2024 let-chain in the Rust
2021 fork; it was corrected before any runtime trial and its receipts retained.

Eight fresh process trials run in **ABBABAAB** order, with four untimed warmups
and 32 timed samples per size per process: 512 cycles total. Setup, warmup,
diagnostic snapshots/deltas, sample storage, oracles, cleanup and reporting
are outside measured windows. The meter includes current-thread Rust
driver/runtime allocation requests, excluding foreign threads and TiDB;
requested bytes are not retained heap or RSS. Latency includes instrumentation.

## SQL amplification and remaining allocations

All 512 samples retain exactly the same successful provider-observed counts
in both roles, with zero errors, cancellations or in-flight operations:

| Operation/cycle | 128 files | 1,000 files |
| --- | ---: | ---: |
| Inode read statements / rows | 7 / 262 | 7 / 2,006 |
| Metadata read statements / rows | 1 / 1 | 1 / 1 |
| Block read statements / rows | 3 / 3 | 3 / 3 |
| Rollbacks | 1 | 1 |
| Pool checkouts | 8 | 8 |
| Session configurations | 0 | 0 |

Complete-root validation still enumerates membership and directory entries:
inode rows remain `2F + 6`. These counters do not cover every MySQL protocol
statement and are not TiKV/RocksDB physical IOPS. No SQL amplification was
removed by this packet classification change.

At 1,000 files, Stat drops from **4,454 to 352 allocation requests**, and from
152,069 to 53,520 requested bytes. Candidate Stat averages 352.44 calls at 128
files. The shared classifier also reduces Open from about 299 to 237 calls and
Read from about 336 to 260 calls; Close remains one call. Full-cycle allocation
is approximately constant across the two measured directory sizes.

The unchanged original one-cycle controls are separate from the pooled
statistics. They still fail the **256-call ceiling** after fresh oracles and
cleanup: A records 1,604 / 5,094 calls, B records **852 / 854**, for 128 / 1,000
files. The limit and workload were not weakened.

## Correctness and qualification

All 16 paired cases and four original-control cases complete fresh independent
metadata audits, compare anchor/root guard/root entry order against the seed,
and check the selected inode and bytes. These oracles do not check EOF, every
file guard or every file's full payload. Actors close before exact generated-key
cleanup, with zero rows verified in all seven owned metadata/block row families.

Fresh qualification passes 160 core units, 50 enabled TiDB units, 56 actual
TiDB compact controls, 24 focused driver controls and four borrow-lifetime/Send
doctests. Classifier controls include 86,016 differential cases against the
retained upstream classifier plus valid OK, both EOF modes, handshake/completed
errors and MariaDB progress. Core/provider and fork strict Clippy and format
checks pass. The nine separately gated database unit tests remain ignored.

Subsequent delivery qualification reproduces the measured candidate binary
byte for byte. Its only Rust source-manifest difference is a NAPI test assertion:
the borrowed path adds one rollback/SQL site, so current coverage mirrors now
expect 11/80 rather than 10/79. The existing NAPI failure was reproduced and
corrected; 44 JavaScript diagnostic controls, the storage benchmark unit suite,
NAPI strict Clippy and final root formatting pass. Historical coverage rejection
and the original 1,000-operations/s throughput gate remain unchanged.

Every benchmark/build/check receipt preserves the 64 GiB host reserve,
300-second child deadline, 64 MiB per-capture limit, direct-child reaping and
absent process groups. The original eight backend containers remain running;
their lifecycle and storage configuration were not changed.

Six new bounded Kani harnesses are registered alongside all 62 existing named
invocations. Their source and 98 direct covers are not executed proof evidence.
Full formal execution, CI, native TiDB/RustFS performance and 10,000-client
capacity remain outstanding.

See [summary.json](summary.json), [paired-results.json.gz](paired-results.json.gz)
and [qualification.json](qualification.json) for statistics, observations,
execution receipts and source provenance. The earlier
[borrowed-root comparison](../tidb-borrowed-root-20260930/README.md) measures
a different change; its values are not this experiment's control.
