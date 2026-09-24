# Mount ownership implementation plan

**Goal:** Explicit exclusive/shared ownership with safe exclusive writeback, tests and measured benchmarks.
**Architecture:** Extend existing ChunkedFs exclusive lease and shared bound-CAS paths. Keep legacy write-through defaults and enable exclusive writeback explicitly.
**Constraints:** Preserve unrelated work; scripts/cargo-shared for Rust; blocks durable before metadata; stale writers fail closed; no distributed SQLite support claim.

- [x] Task 1: Core OwnershipMode enum, ChunkedOptions writeback/builder, deferred publication, barriers and lifecycle safety tests in filesystems/mount-rs-chunked. Test-first: pending writes visible locally but no metadata publication until sync; sync/shutdown reopen; reject shared writeback; expiry/fencing/cancellation/GC/concurrent mutation tests. Run cargo-shared test -p mount-rs-chunked --locked.
- [x] Task 2: SDK SplitOptions writeback and with_ownership_mode builder, exports and propagation; CLI ownership_mode parsing with legacy compatibility and contradiction rejection. Update internal explicit option literals. Test parser/runtime/SDK behavior with focused Rust tests.
- [x] Task 3: Repeatable benchmark comparing write-through and exclusive writeback with operation-counting metadata/block wrappers, random page writes and namespace operations; memory and SQLite-backed runs, sync per transaction and amortized batches; persist raw results and distributions.
- [x] Task 4: Native Linux exclusive FUSE SQLite rollback/WAL and multi-process qualification, investigate available Linux runtime, fix failures within scope. Record exact qualification bounds.
- [x] Task 5: Independent spec/code review and corrections; workspace formatting/Clippy/tests; user documentation plus measured performance and limitations. Complete goal only after required work is done.

## Execution ledger

- Core: 48 unit and 44 integration checks passed; independent review approved generation separation, durable drain, expiry/fencing, cancellation and GC. Fluent builder versus declarative config distinction clarified.
- APIs: 73 CLI and 16 SDK tests passed; N-API ownership integration/type checks and Rust tests/Clippy passed. Independent API review approved end-to-end propagation and default compatibility.
- Benchmark: 16-case matrix with 7 samples plus warmup passed reopen checks. Repeating primary run after known concurrent compile load clears; retain noisy run separately.
- Native: actual Ubuntu arm64 FUSE SQLite DELETE/WAL process lock/recovery/remount test passed. Final-state rerun plus explicit writeback mount-service SIGKILL/restart qualification underway.
- Broad: workspace tests and strict Clippy running, formatting check passed. NFS FILE_SYNC per WRITE documented as a performance boundary.

- Final native verification strengthened to exact IDs/payload bytes after review, passed 15.58s. Explicit writeback mount-service SIGKILL/restart passed36.96s; live lease rejects fresh owner until expiry. Baseline native lane passed69.74s (different workload, not performance comparison).
- Final Rust workspace:1121passed,87ignored,0failures; formatting/strict workspaceClippy green. NAPI final binary rebuilt/focused JS checks green. Final broad review approved after exact-payload and NFS test-handshake fixes.
- Shared-mode scope alignment pending: existing CAS mode is implemented; optional user clarification asks whether to include directorycheckout/checkin in this goal. No claim that these contracts are interchangeable.

- Final benchmark with no known concurrent builds/tests: SQLite sync16 page median125.992→76.098ms (1.66x), namespace156.187→5.501ms (28.39x). Sync1 pages slower; block puts unchanged.112measured+16warmup trials verified reopen.
- Exclusive-first implementation complete. Continuing shared directory authority under docs/superpowers/plans/2026-09-24-directory-ownership.md; existing shared CAS remains separately documented. Goal stays active for this remaining approved proposal scope.

Shared-directory continuation completed under2026-09-24-directory-ownership.md. Finalgoal evidence:1156workspacechecks; nativeExclusive+SharedLinuxFUSESQLite; all four provider contracts; fullNAPIrelease checks;22-casefinalbenchmarks. Allrequired plan work is complete within documentedqualification bounds.
