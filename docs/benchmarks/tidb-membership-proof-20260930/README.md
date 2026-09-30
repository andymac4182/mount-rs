# TiDB membership proof: latency, allocations and returned rows

This adds a metadata experiment to the earlier [TiDB + RustFS cluster
benchmark](../tidb-rustfs-structural-lock-scope-20260930/README.md). It measures
actual serial MRC5 FileCreate publication against the owned durable TiDB 8.5.7
fixture. It does not exercise blobs, QUIC, OS mounts or concurrent clients.

## Paired method

Two release binaries differ at the structural member-proof step: the control
uses actual enumeration; the candidate uses fresh exact equality inside TiDB.
Both contain identical benchmark and allocation instrumentation. Each experiment
runs eight trials in predeclared order `A B B A B A A B`, four per variant.
`A` is enumeration and `B` is the candidate for that experiment. Every
trial has 4 warmups and 32 timed creates at each of 4 and 1,000 existing files:
128 measured samples per variant/size. The fixed member identities remain
unchanged through untimed Full resets. Generation, inode allocation and physical
dentry ordinals advance in the same schedule.

Only direct provider publication plus the identical timeout wrapper is timed.
Seed, capture, pure evaluation, SQL-proxy probe, complete audits, Full resets
and cleanup are excluded. Every intermediate created state is compared with
its pure reference through a fresh complete transaction read before deletion.
The final complete snapshot is read through a new context after the writer
closes. Each experiment completed 592 intermediate and 16 independent final
oracles, and verified cleanup of all 16 owned metadata keys across five tables.
No unknown operation was retried.

Allocation requests are counted on the current test thread during publication.
They include Rust driver/runtime work and exclude foreign threads/server
allocation. Requested bytes count allocation/reallocation traffic, not live
heap, RSS or peak memory. Instrumentation affects latency. SQL observations are
process-local counter endpoints; returned rows are not datastore reads or IOPS.

## Constant range proof

The fast path requires a strictly contiguous expected set of positive inode
IDs. Create-only workloads and retained root-file tombstones can preserve that
layout; Full removals and later allocation can introduce gaps. Eligibility is
checked on every publication. For N eligible IDs, one three-parameter
statement returns total rows and rows within its closed interval. With validated
primary-key uniqueness, both counts equal N exactly when the complete actual set
matches. Only a fresh authority match plus that proof permits borrowing the
sealed base. Sparse sets, mismatches and packet refusals use enumeration. Full
scope/snapshots and complete parent-entry validation remain unchanged.

| Metric at 1,000 files | Enumeration | Range proof | Observed change |
|---|---:|---:|---:|
| Returned inode SQL rows/create |2002 |1002 |−49.95% |
| Rust allocation calls/create |11120 |8111 |−27.06% |
| Requested allocation bytes/create |892620 |804573 |−9.86% |
| Median publication latency |16.122ms |15.933ms |−1.17% |
| Mean publication latency |16.554ms |16.849ms |+1.79% |
| p95 publication latency |20.199ms |24.189ms |+19.75% |
| Inclusive inode-read SQL mean |5.610ms |4.998ms |−10.91% |

The returned-row/allocation reductions are consistent. Whole-publication latency
is mixed: three candidate trial medians were below the pooled control median
(16.122 ms) and one was above it; its pooled mean and p95 were worse.
This does not establish an isolated latency win,
a causal tail regression, sustained capacity or a cluster throughput gain.
At 4 files, median latency rose 11.120→11.446ms, allocation calls fell 473→459,
and requested bytes fell 77705.969→77626.969. Small-set benefits are limited.
TiDB still scans all members, and publication still returns the complete parent
dentry set. The remaining 8,111 allocation requests demonstrate that metadata
publication is far from allocation-free.

## Rejected IN-list candidate

The first candidate sent all expected IDs in a padded IN predicate. It passed
correctness and returned the same 1002 inode rows at 1,000 files, but median
publication increased 16.238→17.683ms (+8.90%) and mean increased 16.582→18.296ms.
Allocation calls fell 11120→8119.969, while requested bytes fell only 1.23%.
Inclusive inode-read SQL mean increased 5.565→7.235ms. That implementation was
replaced by the constant range proof; its evidence remains available.

## Qualification and evidence

`range.json.gz` and `rejected-in.json.gz` retain all samples, trial receipts,
source pins, exact binary digests, raw output hashes and terminal outcomes.
`summary.json` contains independently reproducible statistics and observed
changes. p95 uses nearest rank over the 128 non-warmup samples; trial medians
are retained separately. These are short local measurements with different
volume keys and backing-store history, without a statistical capacity claim.

The final implementation passed all 56 actual TiDB compact controls, including
dense and sparse equal-count substitutions, fresh reads after lock waits,
rollback, unknown COMMIT and packet bounds. Final-source TiDB/NAPI enabled tests
passed 104, with 102 explicit opt-ins ignored. Strict all-target release Clippy,
formatting, JavaScript storage controls, all 44 FoundationDB diagnostic consumer
controls, syntax and whitespace checks passed. The independent review reproduced
the statistics and checked eligibility/fallback semantics. `qualification.json`
binds these local gates to output hashes and records their scope; hosted CI and
full formal results are still required before merge.

The control is a verified source build of the same working tree with only the
member-proof call replaced by its previous enumeration path. Candidate source
pins and distinct binary hashes are in the raw evidence; the tree's base commit
is `f924e8dd`. This is not a claim that every working-tree file was compiled from
that Git commit. Later test-only fixes do not change the measured production
helper or benchmark sources.

The [earlier cluster report](../tidb-rustfs-structural-lock-scope-20260930/README.md)
remains the qualified 10-server/10-client result: roughly 628–656 read cycles/s,
228–236 overwrite cycles/s and 55 churn cycles/s. It used the preceding structural
lock change, not this range proof. The subsequent 100-client attempt failed the
host disk reserve during population; neither it nor this metadata experiment
qualifies the 10,000-client production target.
