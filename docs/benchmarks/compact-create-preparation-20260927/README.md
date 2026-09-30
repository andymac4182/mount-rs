# Compact new-file preparation

The stage metrics found a redundant Full metadata scan when preparing a missing
file. Compact preparation now captures the guarded path and parent. The existing
Full batch snapshot, rebase and structural publication checks still run before
acknowledgment. An occupied path captures its current Full file body.

## Controlled result

The test creates 128 sibling files, reopens the stores, and pauses a new write at
its first immutable block PUT. Preparation and publication are measured
separately, before the complete-byte and fresh-reopen checks.

| Observation | Before | Final after |
| --- | ---: | ---: |
| Preparation Full guard rows | 129 | 0 |
| Preparation selected guard rows | 1 | 3 |
| Preparation snapshot nodes | 129 | 0 |
| Preparation guard bytes decoded | 97,978 | 13,878 |
| Inclusive create-capture time | 2.775 ms | 0.559 ms |
| Publication Full guard rows | 258 | 258 |
| Guard bytes decoded through acknowledgment | 284,682 | 200,582 |
| Authority query calls through acknowledgment | 4 | 5 |
| Read transaction begin calls through acknowledgment | 3 | 4 |

These are one before and one final after observation on local macOS SQLite.
They establish less scan/decode work for this missing-file case. They do not
establish fewer SQL statements, sustained throughput, exclusive CPU time,
allocation-free metadata or physical device IOPS. The nested elapsed spans
overlap. Publication still scans and validates all guards.

The baseline passed complete bytes, exact sizes, EOF, distinct inode identities
and fresh-reopen checks for all 129 files, then failed the intended
zero-preparation-Full-scan assertion. The final implementation passes those
checks and the amplification assertions.

## Safety and tradeoffs

Race controls cover peer inode allocation, symlink retargeting, an occupied
path that requires conflict/replay, and a removed parent that must refuse the
write. An additional occupied-path control installs a stale namespace leaf,
then a newer peer selected body, and verifies Full current-body capture.
Cancellation and preparation/publication overlap controls remain in the suite.

Actual SQLite guard corruption refuses acknowledgment, poisons the live owner,
preserves the raw damaged metadata and causes a fresh owner to refuse opening.
Traversed corruption refuses before any immutable PUT. Unrelated corruption
can be detected by the Full batch after blocks have been prepared: this test
observed two PUTs and no publication. Unreferenced blocks can remain; concurrent
compact SQLite mode does not promise automatic reconciliation or reclamation.

Thirteen local gates passed: 328 Rust test executions, 55 modeled parent
controls, formatting and strict chunked/SQLite Clippy. All twelve owned Rust
gates bind the same 504 source inputs, settled child lifecycles and complete
bounded logs. Warmed recorder controls still observe zero added allocations for
13 selected storage rows and 89 selected core rows. That assertion covers the
recorders, not the whole operation. Hosted CI and merge remain separate gates.

## Reproduce

Use a dedicated writable `CARGO_TARGET_DIR` and the existing shared Cargo
wrapper. The fixed parent selectors are:

```sh
python3 -B scripts/test-remote-failures.py createprep
python3 -B scripts/test-remote-failures.py createpath
python3 -B scripts/test-remote-failures.py createguard
python3 -B scripts/test-remote-failures.py createunit
```

The [report](report.json) retains fixed stage measurements, source/receipt/log
digests, qualification limits, and two earlier fixture/verifier failures.
Neither earlier failure is treated as a production semantic regression.
