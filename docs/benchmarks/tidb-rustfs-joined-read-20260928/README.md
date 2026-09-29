# TiDB joined-read control

The joined-read implementation at `083c82919bdeac7c7182563bbbdaa0e91810b200`
passed the owned TiDB correctness suite and a complete matched remote control.
Its measured throughput was lower than the earlier baseline. This result is
retained as a regression requiring investigation.

| Fleet workload | Baseline ordinary RPC/s | Joined ordinary RPC/s |
| --- | ---: | ---: |
| Random read | 386.99 | 297.95 |
| Random overwrite | 174.91 | 120.90 |
| Mixed | 236.69 | 213.77 |

Each control used ten server processes, ten clients, ten Drives, five
Partitions and 100 files per Drive on the same owned TiDB/RustFS fixture.
Both completed all sixteen pattern/mode stages and two fresh backend-context
byte, membership, EOF and reopen checks. Partition, sibling-Drive and revoked
grant denials passed. All workers exited successfully and cleanup completed.
The catalog used SQLite; filesystem metadata used TiDB and blocks used RustFS.

The all-active random-read pattern still performed five compact metadata
loads per open/read/close cycle. The joined query reduced SELECT calls from
ten to five per cycle, while retaining five explicit read transactions.
Summed inclusive begin and rollback spans increased from about 11.02 ms to
14.93 ms per cycle; the joined SELECT spans were also slower. These overlapping
async spans are observations, and cannot be summed as exclusive CPU costs.
Pool checkout and local filesystem gate waits remained small in that cell.

There is one baseline and one candidate run, with short diagnostic windows.
These measurements do not isolate the change from host or database variation.
They motivate query-plan inspection, a contemporary alternating control and
evaluation of a single-statement autocommit read. No new performance win is
claimed before those changes are implemented and measured.

[Comparison tables](comparison.md) retain all sixteen cells and operation
definitions. [Comparison evidence](comparison.json) retains source and binary
hashes, normalized calls and spans, metric provenance and correctness oracles.
The baseline source is `73afa304cd6a3939cd8695960a8db4e93d9b1c80`.
The candidate has exactly four disclosed source-file differences: the three
joined-read provider/test files and a Windows-only fixture path correction.
The provider source is bound through the accepted release build manifest to
the binary used by the workers.

This does not qualify 10,000 clients, cross-host capacity, sustained throughput,
crash recovery, physical disk IOPS or whole-operation allocation counts.
Current-head CI and PR acceptance remain separate requirements.
The measured clients call service RPCs directly; performance through an
operating-system-mounted CLI remains unqualified.

## Subsequent implementation

The next change uses the same joined SELECT as one autocommit statement, with
a local server-status check requiring autocommit and no active transaction.
Full snapshots and publication retain their explicit transactions. All 19
live TiDB compact controls passed, including the warmed one-SELECT/no-control-
SQL test and cap-one connection reuse after tracked transaction drop and
selected-read cancellation. The cancellation test covers its specific proxy
boundary, rather than every possible cancellation point.

The ordinary TiDB suite passed 45 tests with 53 environment cases ignored.
SDK construction tests passed seven cases; CLI construction and remote tests
passed three and thirteen cases respectively. Default and profiled strict
Clippy and formatting passed locally. Native Windows execution and hosted CI
remain outstanding. This implementation has not yet received a matched
performance run, so the table above continues to describe the earlier
explicit-transaction joined-query control.
