# SQLite reliability and fault injection

Required acceptance work; this document is not a claim of completed coverage.
Test both SQLite as a mount-rs backing provider and SQLite databases hosted
through native mounts or the mount-free VFS. These are distinct boundaries.

## Configuration matrix

| Axis | Required cases |
| --- | --- |
| Journal | DELETE, TRUNCATE, PERSIST, WAL; MEMORY and OFF as explicitly unsafe/volatile cases |
| Synchronization | OFF, NORMAL, FULL, EXTRA; query and record actual values |
| Locking | NORMAL and EXCLUSIVE; single connection, multiple connections, separate processes, competing mount/service instances |
| WAL | automatic checkpoints disabled/enabled; PASSIVE, FULL, RESTART, TRUNCATE checkpoints; long readers, busy checkpoints, WAL/SHM lifecycle |
| Transactions | autocommit, DEFERRED/IMMEDIATE/EXCLUSIVE, commit/rollback, savepoints, schema changes, large binary values, interrupted transactions |
| Layout | representative small/default/large SQLite pages; aligned and misaligned chunk boundaries; growth, truncate, VACUUM, mmap enabled/disabled where supported |
| Access | read-only/read-write, reopen/fresh process, backup/restore, concurrent readers/writers, busy handling, handle cleanup |
| Storage | memory as volatile baseline; every supported persistent metadata/block pair, including split remote stores |
| Interface/platform | native mounts and SQLite VFS; Rust and Node entry points; macOS, Linux and Windows |

Assert the effective journal mode after requesting it. WAL must never pass by
silently falling back to a rollback journal. Unsupported combinations must
reject explicitly with a documented reason and remain acceptance gaps where
the user requires support. Record SQLite version, compile options, platform,
transport, backend versions, topology, chunk size and actual PRAGMA values.

Use bounded representative coverage per PR and a broader scheduled Cartesian
matrix. Store the complete list of cells and their passed/failed/unsupported/
untested status; selecting representative coverage does not erase other cells.

## Failure model and reusable feature

Keep injection in a separate optional integration crate. Normal production
configuration must not inject faults. A plan selects a named boundary, operation,
before/after phase, occurrence or deterministic seed, action and bounded budget.
Emit the selected event and outcome without file contents or credentials. Replay
uses the same operation trace; concurrent ordering must be recorded or controlled
explicitly rather than assumed deterministic from a random seed alone.

Required hooks include metadata load/publication, writer acquire/renew/release,
block put/get/delete, flush/sync, VFS read/write/truncate/lock/unlock, transport
requests/responses and service lifecycle. Cover:

- IO, no-space, permission and read-only failures; short writes/reads where the
  interface permits them; missing blocks and deliberate corruption/truncation.
- Failed barriers, bounded delays, timeout/cancellation, disconnects, retryable
  service failures and failure after commit but before acknowledgment.
- Lease expiry/renewal failures, stale writers, CAS conflicts, concurrent
  publication, interruption during journal writes and WAL checkpointing.
- SQLite-client termination, filesystem-service termination, provider restart,
  partitions and crash/power-loss simulation as separate fault classes.

Immutable block APIs do not return short writes: simulate partial storage at
the appropriate adapter/VFS seam, not by inventing a successful block result.
An after-publication error may have committed; test reconciliation without
duplicate effects or falsely reporting rollback. Never inject into user data.
Use exact test-owned paths/buckets/prefixes, bounded jobs and verified cleanup.

## Recovery assertions

For durability-qualified settings, retain an independent acknowledged-commit
ledger. After each injected failure and fresh reopen, verify `integrity_check`,
foreign-key consistency when used, exact row/blob hashes and transaction history,
absence of partial transactions, and survival of acknowledged durable commits.
Integrity alone is insufficient: a database can be structurally valid but have
lost data. Unknown commit outcomes must reconcile to an allowed whole-transaction
state. Ensure a stale writer cannot publish after replacement takes ownership.

OFF, MEMORY and reduced-sync configurations require separate documented expected
loss/corruption boundaries; do not promise durable recovery where SQLite does
not. SIGKILL testing is not power-loss evidence. Keep real-provider and simulated
fault results separate, retaining the seed, exact plan, revision and failing trace.

## Sources

- [SQLite PRAGMAs](https://www.sqlite.org/pragma.html): journal, synchronization,
  locking and checkpoint configuration and their safety constraints.
- [SQLite testing](https://www.sqlite.org/testing.html): inspiration for systematic
  fault campaigns and recovery checks, not evidence for mount-rs.
- [SQLite corruption hazards](https://www.sqlite.org/howtocorrupt.html): storage,
  locking and synchronization assumptions to verify at each supported boundary.
