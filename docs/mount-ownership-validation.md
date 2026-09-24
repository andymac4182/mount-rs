# Mount ownership validation

## Phase 1: exclusive mode (historical completed runs)

Validated 2026-09-24 against the working tree; unrelated formal-verification plan changes were preserved. Changes are local and uncommitted. No deployment or remote CI qualification is implied.

## Rust workspace

```sh
./scripts/cargo-shared fmt --all -- --check
./scripts/cargo-shared test --workspace --all-targets --locked
./scripts/cargo-shared clippy --workspace --all-targets --locked -- -D warnings
```

Phase1 final runs passed: 1,121 tests across 134 test binaries, zero failures, 87 ignored opt-in cases. The ignored cases include native mounts and external provider services/credentials. Relevant native Linux cases were executed separately below. Strict Clippy, formatting and diff checks passed. Logs for this local run are `/tmp/mount-ownership-workspace-tests-final.log` and `/tmp/mount-ownership-workspace-clippy-final.log`.

Chunked focused final validation passed 49 unit and 44 integration tests. New coverage includes local visibility without immediate publication, file/fs synchronization and shutdown reopen, overlapping same-inode random writes and appends, truncate, atomic rename/unlink, expiry recovery, takeover fencing, block/metadata barrier errors, cancellation, reconciliation roots, monotonic local generations, shared-writeback rejection, and one authority validation per writeback sync.

A broad rerun exposed a preexisting NFS concurrency test handshake race: it announced backend entry before registering its release notification. An isolated replay passed; the test-only fix registers/enables its waiter before readiness notification. The complete final workspace rerun passed without increasing deadlines or changing NFS production behavior.

## Consumer boundaries

CLI/SDK tests cover explicit modes, legacy defaults, simultaneous declarative contradictions, supported shared provider pairing, and writeback propagation. Independent core/API/final reviews approved the implementation; the final review's payload-verification gap was corrected and native tests rerun.

The final N-API addon was rebuilt using `node scripts/build-native.mjs` and `node postbuild.mjs` with shared Cargo environment. `node test/chunked.mjs`, `node test/typecheck.mjs`, and `node test/node-cli.mjs` passed against it. Addon SHA-256: `b2fcc991b205b85d3cc54597fa13e4821ab353f2cf41d016fb696a278a0529d9`. Thirty N-API Rust unit tests and strict Clippy passed. All broader N-API suite stages passed; an initially missing pinned oracle unstorage dependency was installed under `/private/tmp` and affected/remaining stages rerun. External service/feature opt-ins remained skipped.

## Native SQLite and performance

See [native qualification](native-exclusive-sqlite-qualification.md) for exact Ubuntu arm64 Linux FUSE execution: multiprocess DELETE/WAL locks, killed SQLite writer recovery, exact byte-content reopen checks, and mount-service SIGKILL/restart with live lease fencing. The Linux VM was returned to its initially stopped state and original Docker context restored.

See [benchmark methodology](../benchmarks/ownership/README.md) and [measured results](../benchmarks/ownership/RESULTS.md) for release comparisons, raw trials, counters, source fingerprints and local-provider limitations.

These phase1 runs qualify the observed exclusive Linux FUSE lane and tested direct-driver contracts. NFS FILE_SYNC per WRITE preserves its existing cost. Shared directory checkout/checkin was implemented in phase2 below; phase1 artifacts retain their historical source fingerprints.

## Phase 2: shared directory ownership (completed)

Final exact-source workspace execution passed **1,156 tests across136 test binaries, zero failures,89 ignored opt-in cases**. Whole-workspace strictClippy, formatting and diff checks passed. Logs: `/tmp/mount-ownership-phase2-workspace-tests-complete.log` and `/tmp/mount-ownership-phase2-workspace-clippy-complete.log`.

Core authority:35 tests. Chunked engine:51 unit,44 legacy-CAS integration,9 delegated integration and1 benchmark example test. SQLite provider:69 tests including claim/checkin COMMIT faults. SDK:17 tests including real disjoint scopes, handoff, expected-fence recovery and stale-owner refusal through erased adapters. CLI:79 unit tests plus portable integration; real SQLite command dispatch verifies enrollment revision protection, JSON status and recovery fencing.

Independent contract, provider, engine, CLI and N-API reviews approved the final implementation. Corrected findings include sparse allocator exhaustion, malformed Legacy enrollment fences, orphan cleanup replay and checkin retries, ordinary unauthorized mutation poisoning, namespace/authority revision coherence, transport re-probe, and native retirement tracking. Cancellation tests interrupt actual futures during flush, release and native creation. Ambiguous checkout retains its nonce unless verified authority proves it inactive.

### Consumers

Final N-API32 Rust tests and strictClippy passed. The final **release** addon includes engine source SHA-256 `2045a6315fbfbb0478578265f0fa3ad2a7ce81d5595ad7f88eb5e86dd52defec`; binary SHA-256 `56ce92bb55b785abe04efc9b8634296ef45a64acb01e8b48adf752e9e9522212`.

Focused chunked, type and Node CLI checks and the full aggregate `pnpm test` passed with unchanged default network deadlines and the pinned oracle dependency. An earlier contended/debug non-chunked SQLite WebDAV PUT test hit its10s deadline; the final quiet release suite passed that stage in192ms without a deadline override. Expected external/native feature opt-ins were skipped. Actual N-API host mounts were not attempted; native Linux qualification uses the Rust FUSE lane below.

Generic N-API delegated mounts require Linux FUSE and a grant, reject protocol adapters, and block checkout/checkin/shutdown until confirmed native retirement. Failed/canceled creation retains an uncertainty marker requiring kernel retirement and offline recovery before a fresh coordinator. SDK direct-driver consumers must uphold the documented native unmount/remount contract.

### Native SQLite

See [shared native qualification](native-shared-sqlite-qualification.md): final coherent source passed three consecutive strengthened Linux FUSE runs in14.69/18.82/20.66s with unchanged hashes and zero invalid-orphan-provenance traces. Tests run concurrent DELETE/WAL workloads through two real mount processes in disjoint directories, deny overlapping and cross-scope access, perform clean unmount/checkin/remount handoff, and recover a SIGKILL owner using its exact fence while preserving the other grant and exact data bytes.

Final-source Exclusive regression passed6.66s; rebuilt Exclusive mount-service SIGKILL/restart passed16.55s. Strict LinuxClippy covered all three native targets and the service example. These timings are test execution evidence, not performance comparisons. No remaining FUSE mounts or test children; Colima returned to **Stopped** and Docker context to **desktop-linux**.

### Provider servers

PGlite29 library tests passed, including actual isolated Node-backed server cases. TiDB14 unit tests and portable suites passed; actual TiDB8.5.7 single PD/TiKV topology passed delegation plus direct/concurrent/405-mutation load/ambiguous-commit suites (`/private/tmp/mount-rs-delegation-tidb-service-final-20260924.log`). FoundationDB36 unit tests and actual7.4.7 arm64 single `ssd-2` delegation case passed with exit0 (`/private/tmp/mount-rs-delegation-fdb-service-budget-20260924.log`). Feature-enabled strictClippy passed. Owned test services stopped/cleaned.

These service smoke lanes establish observed transaction behavior; they do not qualify replicated failure tolerance or SQLite hosting on remote services. FoundationDB authority state also has its100KB value limit, tighter than the core4096-identity bound in some workloads.

### Benchmarks and support boundaries

Final combined release run:22 cases,154 measured trials plus22 warmups, all full-byte/file-size reopen checks and896 overlapping-checkout denials passed. Parent independently matched all25 recorded source/manifest/script hashes to the final working tree. Known task builds/tests and qualification VM were stopped during measurement; the development host remained unisolated.

SQLite batch16 page writes: median108.669→64.643ms (**1.68×**); namespace operations141.257→5.405ms (**26.13×**), including sync. Page publication128→8 and flush136→8; immutable puts128 unchanged. Sync-per-page median147.525→150.445ms showed no gain.

Shared disjoint writes: MRC2 median297.233ms, MRC3 **436.936ms** for256 writes, reflecting authority/delta checks and retries. Direct-driver delegated handoff+receiving full read median **2.989ms**, p95 **3.671ms**. Legacy CAS has no exclusive handoff, so its timing is not safety-equivalent. See [exclusive results](../benchmarks/ownership/RESULTS.md), [delegation results](../benchmarks/ownership/RESULTS-delegated.md) and raw provenance.

This completes the implemented exclusive and shared-directory contract lanes. macOS FSKit, live mounted cache revocation, independent-host native SQLite, network performance, replicated provider durability and device/VM power loss remain outside qualification. Shared GC and receipt compaction are not implemented. Existing grants remain fenced when provider authority capacity is exhausted.
