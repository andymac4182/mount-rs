# Filesystem Causal Metrics Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task by task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Attribute filesystem block-put amplification and operation-gate contention to initial work, fallback, replay, recovery and mutation batching.

**Architecture:** Append a closed causal label set to the existing opt-in core profile recorder. A private chunked helper module supplies stack observations for actual block puts, gate acquisition/ownership and batch boundaries. Keep the old rows and their recording sites intact; update the closed JavaScript consumer lists together.

**Tech Stack:** Rust 1.95, existing `async-trait`/`tokio::sync`, fixed atomic profile rows, existing Node test runner controls.

## Global Constraints

- Base: committed `909053a54dc4cf0a9a4f074b149dd4008f1e0f04`; branch `codex/storage-causal-metrics` in `/Users/amcclenaghan/github/andymac4182/mount-rs-causal-metrics`.
- Root accepted the genuine seven-case runtime RED and authorized scoped GREEN source implementation. Runtime execution, Cargo, Node, index and commits remain controlled by the root's separate leases.
- Leave the original checkout and its protected 15 paths untouched. Do not copy its dirty baseline into this checkout.
- Preserve the exact 47-row existing core `Event` prefix, every ordinal/name and recording site, including `filesystem.gate_wait` and `filesystem.mutation_batch_attempted_requests` semantics.
- The separate `diagnostics::storage::Operation` bank remains exactly 85 rows; these 61 additions belong only to `diagnostics::profile::Event`.
- No provider trait changes, new dependencies, keys, paths, tokens, arbitrary errors, dynamic labels or per-request registry.
- Recorders allocate once when warmed. Recording, cancellation and guard drop add no heap allocation. Disabled observations evaluate no clock. Snapshot/JSON publication allocation remains outside the recording claim.
- No change to retries, coalescing policy, fencing, outcomes, thresholds, flush ordering or cleanup.
- Wall spans are inclusive. Gate hold and nested phases may overlap; do not add them as exclusive time shares.
- Retained CI observations have an unverified source/build join. These changes do not establish that current code caused the retained 1.88/1.8425 ratios or qualify the 1,000 logical operations/sec floor.

## Contract and File Map

| File | Responsibility |
| --- | --- |
| `src/diagnostics/profile.rs` | Append the fixed names below; retain the old prefix and `name/calls/elapsed_ns/units` fields. |
| `filesystems/mount-rs-chunked/src/causal_metrics.rs` (new, GREEN stage) | Private `PutReason`, `PutObservation`, `GateKind`, `GatePhase`, and acquisition/batch terminal guards using the existing profile recorder. |
| `filesystems/mount-rs-chunked/src/lib.rs` | Pass reasons at the six `rewrite_layout` caller sites; observe its two actual put sites; classify every `operation_gate` caller; retain a hold span in `OperationGateGuard`; observe queue/coalescing/attempt/reply boundaries. |
| `filesystems/mount-rs-chunked/tests/filesystem_causal_metrics.rs` (new now) | Public filesystem operations with independent provider-call counts and deterministic CAS, pending-future and manual-clock controls. |
| `tests/filesystem_causal_profile_allocations.rs` (new, GREEN stage) | Warmed allocation gate for the actual new core `Event`/`Span`/`add` recording primitives, separate from functional filesystem/provider work. |
| `benchmarks/storage/owned-layout-metrics.mjs` and `.test.mjs` | Expand `CORE_NAMES` and acceptance/rejection controls. |
| `benchmarks/storage/owned-backing-pilot.mjs` and `.test.mjs` | Expand `profileNames` and independent `modelCommittedProfileNames` fixture, retaining prefix equality. |
| `scripts/verify-owned-backing-pilot.mjs` and `.test.mjs` | Expand exact `PROFILE_NAMES`/fixture contract; preserve unknown/duplicate/missing-row rejection. |
| `docs/bottleneck-metrics.md` | Define the causal rows, cancellation and byte semantics, old/new gate coverage difference, and inclusive phase limits. |

Generic core/NAPI/CLI/production-target exports already carry arbitrary matching profile rows and need no field/schema change. `scripts/summarize-remote-scaling.py` can receive a narrow display-list extension after the core contract is green; it is not a correctness prerequisite. No object-store bank change is needed: existing leaders/followers and raw creates already describe upload sharing.

The existing `owned-layout-metrics.mjs` projector allowlist contains 53 labels: the committed 47-row bank plus six previously accepted compact capture/COW labels. Preserve all six additional consumer labels, then append 61 (114 allowed projector labels). The actual Rust bank, pilot and exact verifier have 108 rows with the exact 47-row prefix. Projecting a partial input does not qualify a complete profile; do not normalize the projector's existing 53-label contract down to 47 or add those six labels to the Rust bank/independent exact fixture.

### Fixed appended label set

The order is the following group order, then enum order, then suffix order. Freeze that order in the independent consumer fixtures. There are **61 appended rows**.

1. Four `PutReason` values: `initial`, `fallback`, `retry_rewrite`, `chunker_reprepare`. For each append `filesystem.block_put.<reason>`, followed by `.success`, `.error`, `.cancelled` (16 rows). The unsuffixed span counts dispatched `BlockStore::put` calls and attempted chunk input bytes. Terminal rows partition calls; only success records the known input length of the successfully returned put, while error/cancelled units are zero. Attempted bytes remain separate from successfully completed input bytes, application payload bytes and wire/storage bytes. A `BlockId` return is not a returned byte body or proof of physical upload/durability. Drop while awaiting records cancellation once.
2. Eight `GateKind` values: `read`, `metadata`, `write_prepare`, `write_commit`, `write_fallback`, `whole_file_replay`, `mutation_batch`, `maintenance`. For each append `filesystem.gate_wait.<kind>`, `filesystem.gate_hold.<kind>`, `filesystem.gate_wait.<kind>.cancelled` (24 rows). Wait includes acquisition cancellation; hold starts only after acquisition and ends on guard drop. Units are zero. The new classified rows cover all acquisition sites; the unchanged old aggregate covers its existing caller-local sites, so their call sums need not equal.
3. Five guard-owned phase rows: `filesystem.gate_phase.refresh`, `.recovery`, `.block_rewrite`, `.publication`, `.cas_backoff` (5 rows). They may only be constructed through a borrowed acquired `OperationGateGuard`. Nested refresh/recovery/publication spans are inclusive. They do not measure work outside the gate.
4. Sixteen batch rows: `filesystem.mutation.enqueue_requests`, `.dequeue_requests`, `.queue_wait_requests`, `.coalescing_yields`, `.attempt_requests`, `.attempt.success`, `.attempt.conflict`, `.attempt.no_publication`, `.attempt.error`, `.attempt.cancelled`, `.request.committed`, `.request.conflict`, `.request.cancelled`, `.request.receiver_closed`, `.request.error`, `.request.reply_sent`. Count events use `units` for request/yield counts. Queue wait uses one span per queued request until dequeue or queue cancellation. Coalescing counts one span per adaptive window and its actual yield count. Attempt span starts inside the attempt loop, counts only requests whose replies remain open and whose candidate is considered, and receives one terminal outcome. Every provider-confirmed concurrent CAS/Eagain loss is `conflict`, including the final attempt when no retry remains; the returned request error may still be Eagain. `no_publication` identifies an iteration whose candidates produce no namespace change, including stale whole-file preparations; it must not count a CAS dispatch. Terminal committed/conflict/cancelled/error request observations occur once. Separately observe actual send attempts or closed-receiver skips as `reply_sent` (channel send returned Ok) or `receiver_closed`; a request dropped without a send/closed observation may have neither delivery row. A successful send is an enqueued reply and does not establish caller consumption or application acknowledgement. A lost receiver after known commit retains the committed observation and existing fail-closed behavior. Keep workload acknowledged operations as the independent denominator.

### Source boundary and units map

All pointers below refer to the base `filesystems/mount-rs-chunked/src/lib.rs`; append-only recorder names live in `src/diagnostics/profile.rs:18-66`.

| Observation | Exact base seam | Calls / units |
| --- | --- | --- |
| Put `initial` | Selected-inode preparation 2647/2963, optimistic partial write 4227, whole-file preparation 4469; actual dispatch 7540/7590 | One dispatch / actual chunk input bytes |
| Put `fallback` | `write_at_serial` 4300/4341, attempt zero | One dispatch / actual chunk input bytes |
| Put `retry_rewrite` | Selected-inode loops 2594/2890 and serial loop 4317/4341, attempt greater than zero | One dispatch / actual chunk input bytes |
| Put `chunker_reprepare` | Whole-file replay's changed-chunker branch 4611/4619 | One dispatch / actual chunk input bytes |
| Put terminal rows | Actual result or dropped await at 7540/7590 | One terminal call / successful known input bytes, otherwise zero |
| Classified wait/hold | Acquired guard seam 690 and owned guard/drop 253-265 | One wait or completed ownership interval / zero |
| Gate `read` | Read capture/validation 4009/4135; selected-inode read paths | One wait/hold / zero |
| Gate `metadata` | Generic mutate 3942, public stat/lstat 5798/5815, lookup/open/guarded namespace paths | One wait/hold / zero |
| Gate `write_prepare` / `write_commit` | Partial write 4194/4242; selected-inode 2604/2662, whole-file compact capture 4411 | One wait/hold / zero |
| Gate `write_fallback` / `whole_file_replay` / `mutation_batch` | 4309 / 4569 / 3806 | One wait/hold / zero |
| Gate `maintenance` | Shutdown, sync, close and checkin acquisitions | One wait/hold / zero |
| Gate `refresh` / `recovery` | Guarded lease/metadata validation 3185-3229; actual expired-lease reacquire 3240-3326 | One guard-owned phase / zero |
| Gate `block_rewrite` / `publication` / `cas_backoff` | Guarded rewrite 4341, publication 3368/3427, held backoff 3898/3984/4379/4677 | One guard-owned phase / zero |
| Enqueue/dequeue | Successful insertion 3737 and drain 3797 | One observed boundary / number of requests |
| Queue wait | Existing request creation 3668/3695 through drain/cancellation | One request interval / one request |
| Coalescing | Adaptive window 3751-3782, each actual yield 3762 | One window / actual yields |
| Batch attempt | Loop iteration 3814; only open replies considered 3831-3885 | One iteration / considered request count; never a CAS count |
| Attempt terminals | CAS acknowledgement/conflict/error 3888-3907, `changed == false`, dropped attempt | One outcome / same considered request count |
| Request result/delivery | Canceled skips 3836/3870, final result/reply 3909-3927 and `mutation_reply` | One terminal result / one request; separately one send/closed-receiver observation / one request |

The eight gate kinds are fixed source roles, not syscall/path dimensions. The base source has 31 acquisition sites plus the definition (32 textual occurrences); review every acquisition once while wiring. Selected-inode and previously unprofiled acquisitions receive classified rows without changing the old aggregate. Guard-owned phase intervals and batch spans remain nested inclusive observations.

## Task 1: Establish genuine RED from real filesystem work

**Files:** Create `filesystems/mount-rs-chunked/tests/filesystem_causal_metrics.rs`.

**Interfaces:** Consume existing `ChunkedFs::open`, `FsDriver::{write_file,open,stat,shutdown}`, `FileHandle::{write,read,close}`, `profile::{enabled,snapshot}`, `MemoryBlockStore`, `MemoryMetadataStore::with_clock`, `ManualClock`, `MetadataStore` and `BlockStore`. Read proposed counters by string; do not reference nonexistent production symbols.

- [x] Write independent boundary fixtures and seven tests in the new file.
- [x] Mark all seven tests `#[ignore = "requires MOUNT_RS_PROFILE_IO=1 and MOUNT_RS_TRACE_STORAGE=0"]`. Ordinary workspace/all-target runs leave them unexecuted; their ignored result does not qualify this gate.
- [x] Root obtained the integration-test Cargo lease after the clean `--lib` baseline settled.
- [x] Ran the exact RED command below: the seven public filesystem/provider prerequisites completed and each failed on a missing causal metric. Compilation errors, profiling-disabled assertions, fixture timeouts or failed payload checks are not an accepted RED.
- [x] Retained the actual exit, stdout/stderr hashes and first assertion for each case before production edits. Independent acceptance: `/private/tmp/mount-rs-causal-red-independent-review-20260927-lz_cl6_8/immutable-review.json`, SHA-256 `deae279e06e964ee14ec8b02681ffce8f2ccb5891f4bf5066c2dceb2c177c4a9`.
- [x] Dedicated RED reported `running 7 tests`, all seven named cases executed, zero filtered/ignored cases, and seven missing-metric failures. The four subsequent GREEN variant controls and compact-provider control are additions; the retained RED remains exactly the original seven cases.

```sh
MOUNT_RS_PROFILE_IO=1 MOUNT_RS_TRACE_REQUESTS=0 MOUNT_RS_TRACE_STORAGE=0 \
  ./scripts/cargo-shared test -p mount-rs-chunked --test filesystem_causal_metrics \
  --locked --offline -- --ignored --test-threads=1 --nocapture
```

Cases exercise: one real partial overwrite that loses its fast CAS and first fallback CAS, then rewrites in fallback retry; cancellation after the block provider receives a put; terminal block failure; two prepared writes coalesced into one batch with one controlled known CAS loss; canceled batch request before publication; a canceled metadata waiter while a publication owns the gate; and lease recovery driven by `ManualClock`.

## Task 2: Attribute actual block-put calls

**Files:** Modify `profile.rs`, create private `causal_metrics.rs`, modify `chunked/src/lib.rs`, extend the new test.

**Interfaces:** Produce private `#[derive(Clone, Copy)] enum PutReason { Initial, Fallback, RetryRewrite, ChunkerReprepare }` and `PutObservation::new(reason, bytes)`, `finish(&Result<T>)`. `Drop` records cancelled exactly once. The observation contains an existing `Span` and terminal flag; it does not box the future.

- [x] Append the 16 reason/outcome rows without changing the old prefix.
- [x] Pass `RewriteContext { path, reason }` to private `rewrite_layout`; the stack context keeps the existing argument count and carries only the existing borrowed path plus fixed enum. Put the observation immediately before each `.put` at current lines 7540 and 7590. Finish from its actual result before applying the existing error context.
- [x] Initial write/replace preparation uses `Initial`. `write_at_serial` uses `Fallback` for its first attempt and `RetryRewrite` for later attempts. Selected-inode write/replace uses `Initial` for attempt zero and `RetryRewrite` thereafter. Whole-file replay's changed-chunker branch uses `ChunkerReprepare`; unchanged layout replay records no put.
- [x] Run the partial overwrite, canceled put and failed put tests; require exact call and terminal-call partitions, attempted bytes and successful input bytes against the independent block fixture.
- [x] Add a forced second fallback conflict and changed-chunker replay control before claiming those reason variants wired. Verify empty and all-zero chunks add no actual put row.

Minimal call-site shape:

```rust
let mut observed = PutObservation::new(reason, chunk.len() as u64);
let result = blocks.put(&chunk).await;
observed.finish(&result);
let block = result.map_err(|error| with_context(error, "block-put", Some(path)))?;
```

## Task 3: Separate gate wait, ownership and held phases

**Files:** Private `causal_metrics.rs`, `chunked/src/lib.rs`, new integration test.

**Interfaces:** `operation_gate(kind: GateKind) -> OperationGateGuard<'_>`; private `OperationGateGuard::phase_permit(&self) -> GatePhasePermit<'_>`, followed by `GatePhasePermit::phase(GatePhase) -> GatePhaseObservation<'_>`. Put the hold-span field before the existing mutex-guard field so, after existing compact cleanup, its field drop records immediately before mutex release. An acquisition observation records cancelled if the future drops before it constructs that guard. The private permit is a lifetime-bound capability whose sole constructor borrows the acquired guard; its used `phase` method constructs `GatePhaseObservation<'gate>` containing the existing `Span`. Both carry `PhantomData<&'gate ()>` and no reference to the mutex guard, so carrying them across an await adds no `Sync` bound on that guard. The phase has explicit Drop, retaining the guard borrow until its span ends. They cannot outlive the guard borrow, including when a phase is returned from a permit. Do not add public bounds, an unused marker or an unrestricted permit constructor.

- [x] Append the 24 classified gate rows and five phase rows. Preserve all old caller-local `GateWait` spans.
- [x] Classify every call site. Read capture/validation is `Read`; lookup/namespace mutation/open is `Metadata`; optimistic captures/commits are `WritePrepare`/`WriteCommit`; fallback/replay/batch have their own kinds; sync/shutdown/checkin/maintenance use `Maintenance`. Selected-inode paths use the corresponding read/write class.
- [x] Start hold observation after lock acquisition; end it on guard drop, including errors and dropped futures. Preserve compact `pending_full` cleanup before mutex release.
- [x] Construct phase observations at the actual guarded refresh, rewrite, publication and backoff call sites, before awaiting their existing futures. Refresh includes `ensure_operation_lease`, `validate_lease` and selected-inode refresh. For the nested recovery branch only, propagate `Option<GatePhasePermit<'_>>` through those private renewal/reacquire helpers and use its `phase(Recovery)` method there. Optimistic preparation without a gate passes `None`. Keep the lease control flow and public provider traits unchanged. Check concrete guard `Send`/`Sync` behavior before any alternate borrowed-guard design; do not introduce a new trait bound to satisfy profiling.
- [x] Run the held-publication waiter-cancellation and manual-clock recovery tests. Add an acquired-then-canceled phase control and a normal read/metadata control. Require hold counts only for acquired gates and a canceled wait without a corresponding hold.
- [x] Ensure no new phase/counter changes when profiling is disabled; no phase clock is evaluated through an absent guard context.

## Task 4: Account for queue, coalescing, attempts and acknowledgements

**Files:** Private `causal_metrics.rs`, `chunked/src/lib.rs`, new integration test.

**Interfaces:** A stack queue-wait span is owned by each existing `MutationRequest`; it ends when removed from the queue or canceled. A stack attempt guard is created inside `for attempt in 0..attempts`, with terminal conflict/error/cancelled/success recording. Existing `MutationAcknowledgementGuard`/reply boundaries remain authoritative.

- [x] Append the 16 batch rows. Record enqueue after successful queue insertion, dequeue when draining, and actual yields inside the existing adaptive loop.
- [x] Move no existing aggregate span. Start a separate attempt observation inside the retry loop. Increment attempted request units while traversing open replies, preserving canceled skips.
- [x] Record known CAS loss before backoff; separately time backoff through the owned gate. Record terminal committed/conflict/error/cancelled once and channel delivery (`reply_sent`/`receiver_closed`) separately. Preserve the existing lost-postcommit-response fail-closed boundary.
- [x] Run the coalesced known-conflict test: two puts, one old batch invocation with two units, two actual batch attempts with four attempted-request units, one known CAS conflict, one no-publication iteration, two conflict results and two successful channel sends. The stale whole-file preparations then replay their existing layouts separately: three provider CAS dispatches total, two commits and two whole-file replay holds. The test separately observes both public futures return success. Run canceled-before-publication and add canceled-after-commit receipt controls before claiming complete terminal coverage. Do not change the filesystem's current whole-file conflict rule to fit a fixture.

## Task 5: Close consumers, observer costs and documentation

**Files:** The six JavaScript registry/fixture files in the file map, `owned-layout-metrics.test.mjs`, core/chunked observation tests, and `docs/bottleneck-metrics.md`.

- [x] Append the same 61 labels in all closed consumers and independent fixture lists. Test old-prefix stability, missing/duplicate/unknown rows, zero rows and string counter preservation. The old snapshot remains incomplete under the expanded exact consumer contract; do not silently manufacture zero causal traffic.
- [x] Run the pure Node controls under a separate root lease. The pilot fixture requires the existing capture guard environment, independently of the production profile gates:

```sh
env -u NAPI_RS_FORCE_WASI -u NAPI_RS_WASI_FLAVOR -u NODE_PATH \
  -u MOUNT_RS_PROFILE_IO -u MOUNT_RS_TRACE_STORAGE \
  NAPI_RS_NATIVE_LIBRARY_PATH="$PWD/benchmarks/storage/capture-native.cjs" \
  fnm exec --using v24.18.0 node --test benchmarks/storage/owned-layout-metrics.test.mjs \
  benchmarks/storage/owned-backing-pilot.test.mjs \
  scripts/verify-owned-backing-pilot.test.mjs
```

- [x] Create `tests/filesystem_causal_profile_allocations.rs` using the counting-allocator pattern in `tests/storage_diagnostics_allocations.rs`. Warm `profile::enabled`, the profile bank and allocator tracking before the count window. In the window, exercise every newly appended concrete `Event` with `profile::Span::new(...).units(...)`, terminal `profile::add(...)` and cancellation/drop; assert zero additional allocations. Keep snapshots, filesystem/provider futures, executor work and logs outside the window. Add private helper lifecycle coverage in the causal module where it can use those real helpers without exporting test APIs. Include a disabled-clock control. The existing 85-row `diagnostics::storage::Span` gate provides allocator precedent only and does not qualify `diagnostics::profile::Span` or the new helpers. Retain the actual new gate result separately.

The causal helper module adds one ordinary static control, `causal_metrics::tests::phase_capabilities_do_not_add_mutex_guard_send_or_sync_bounds`, and one ignored profile lifecycle control, `causal_metrics::tests::helper_terminal_outcomes_and_drop_partition_once`. The latter needs a separate quiescent process with profiling enabled and one test thread; an ordinary `--lib` run leaves it ignored and does not qualify its terminal partitions.

```sh
MOUNT_RS_PROFILE_IO=1 MOUNT_RS_TRACE_REQUESTS=0 MOUNT_RS_TRACE_STORAGE=0 \
  ./scripts/cargo-shared test -p mount-rs-chunked --lib --locked --offline \
  -- --ignored --exact causal_metrics::tests::helper_terminal_outcomes_and_drop_partition_once \
  --test-threads=1 --nocapture
```

```sh
MOUNT_RS_PROFILE_IO=1 MOUNT_RS_TRACE_REQUESTS=0 MOUNT_RS_TRACE_STORAGE=0 \
  ./scripts/cargo-shared test -p mount-rs-core --test filesystem_causal_profile_allocations \
  --locked --offline -- --ignored --exact warmed_causal_profile_rows_record_without_added_allocations
```
- [x] Re-run the exact dedicated integration command, then focused legacy/concurrent controls and touched-surface formatting/strict lint under the root's bounded leases:

```sh
MOUNT_RS_PROFILE_IO=1 MOUNT_RS_TRACE_REQUESTS=0 MOUNT_RS_TRACE_STORAGE=0 \
  ./scripts/cargo-shared test -p mount-rs-chunked --test filesystem_causal_metrics \
  --locked --offline -- --ignored --test-threads=1 --nocapture
./scripts/cargo-shared fmt --check
./scripts/cargo-shared test -p mount-rs-chunked --lib --locked --offline
./scripts/cargo-shared test -p mount-rs-chunked --test concurrent_cas --locked --offline
./scripts/cargo-shared clippy -p mount-rs-core -p mount-rs-chunked --all-targets --locked --offline -- -D warnings
```

- [x] Qualify the dedicated Unix GREEN only from an actual `12 passed; 0 failed; 0 ignored; 0 filtered out` result with all twelve named cases executed. The target has eleven portable cases and one `#[cfg(unix)]` SQLite compact persistence control; non-Unix execution has eleven. Default all-target output reporting twelve ignored tests on Unix is an unexecuted gate. The retained genuine RED stays seven cases.
- [x] In the GREEN publication stage, add a separate profiling-enabled step to the existing observability CI job in `.github/workflows/ci.yml`; keep the default workspace gates. Retain the complete dedicated log and original test exit status, including failures. The additive step uses the exact command below with `shell: bash`; upload `$RUNNER_TEMP/filesystem-causal-metrics.log` with `if: always()` in the existing allocation/observability artifact. Hosted execution remains a separate pending gate: inspect the actual uploaded log's platform-specific case count and terminal result before claiming hosted coverage.

```sh
set -o pipefail
MOUNT_RS_PROFILE_IO=1 MOUNT_RS_TRACE_REQUESTS=0 MOUNT_RS_TRACE_STORAGE=0 \
  ./scripts/cargo-shared test -p mount-rs-chunked --test filesystem_causal_metrics \
  --locked --offline -- --ignored --test-threads=1 --nocapture \
  2>&1 | tee "$RUNNER_TEMP/filesystem-causal-metrics.log"
```

- [x] Document denominator, reason partitions, canceled waits, queue/reply accounting and inclusive overlapping phase times. Retain the old floor failures and source/build uncertainty.
- [x] Review only the scoped diff and record gates before publication. No runtime throughput, backend saturation or clean Linux qualification claim follows from these memory controls.

### Bounded slow-log addendum

The core owner is authorized to add fixed `MOUNT_RS_PROFILE_SLOW` stderr lines through the existing `MOUNT_RS_PROFILE_IO=1` plus `MOUNT_RS_TRACE_STORAGE=1` opt-in. The threshold is 100 ms; an atomic process budget caps emission at 16 records. Lines contain only a closed event name and numeric elapsed/units fields, without paths, keys, tokens or raw errors. There is no new registry or per-operation allocation. These coarse lines are inclusive spans, not an exclusive trace. The new core controls cover disabled/no-clock behavior, lifecycle/drop partitions, record-budget/privacy behavior and the actual warmed recording primitives. The trace-disabled allocation gate excludes stderr/log execution from its allocation window.

CI has four additive causal gates: the dedicated filesystem semantics target (12 cases on Unix, 11 on Windows), the exact warmed core profile allocation control, the exact private helper lifecycle control, and the exact trace-enabled stderr smoke `causal_profile_slow_span_emits_fixed_stderr`. Retain `filesystem-causal-profile-slow.log` alongside the semantics/allocation logs. The existing legacy storage allocation log remains distinct evidence for the unchanged 85-row storage bank.

## Preparation Status and Handoff

The original seven-case RED was executed and independently accepted at the receipt above. Scoped implementation and local validation are complete. Four deterministic variant controls and one Unix compact SQLite persistence control extend the Unix GREEN count to twelve. Final Rust formatting changed only the chunked implementation and integration-test layout; the dedicated target and strict lint passed again after formatting. The original RED packet is retained unchanged.

The added GREEN cases are `changed_chunker_replay_attributes_only_actual_reprepared_puts`, `empty_and_all_zero_whole_files_dispatch_no_block_puts`, `cancelled_acquired_fallback_records_hold_and_block_rewrite_phase`, `dropped_follower_after_known_commit_retains_commit_and_closed_reply`, and Unix-only `sqlite_compact_selected_gates_and_phases_preserve_payload_after_reopen`. The SQLite case asserts selected write prepare/commit gate counts, refresh/publication phases, stat/read gate phases and persistent bytes through fresh provider connections. Its private directory is RAII-owned. The changed-chunker fixture independently expects two provider publications (initial known CAS loss plus final replay commit); the intervening no-publication/reprepare iterations dispatch no CAS.

## Local validation completed on 2026-09-27

| Gate | Actual local result |
| --- | --- |
| Dedicated filesystem causal target | 12 passed, zero failed/ignored/filtered; repeated after formatting |
| Core and chunked library controls | 135 passed; two ignored, including the helper executed separately below |
| Concurrent CAS controls | 44 passed |
| Exact helper lifecycle control | 1 passed |
| Warmed recording allocation control | 1 passed; zero added allocations across all 61 new rows |
| Exact slow-record CI shell step | 1 passed; fixed stderr record and anchored shell check both succeeded |
| Closed consumer and verifier controls | 117 passed under the explicit capture guard |
| Workspace formatting | Passed after formatting the two reported Rust files |
| Strict core/chunked all-target Clippy | Passed with `-D warnings`; repeated after formatting |

The slow record retained in the local CI-step log was
`MOUNT_RS_PROFILE_SLOW event=filesystem.block_put.initial elapsed_us=110102 units=4`.
The consumer suite's initial launch failed its existing capture-environment
prerequisite; the corrected launch supplied the required capture guard and
passed without changing assertions. The first formatting check failed and its
output was retained before the formatting correction.

Independent reviews covered the genuine RED, production instrumentation and
core/consumer contracts. A review found that the slow-record CI step did not
assert its output; the step now requires the fixed record, and the exact shell
body passed locally. All 15 protected paths in the original checkout must be
verified unchanged before publication.

- [ ] Confirm the four new gates from hosted Linux, macOS and Windows artifacts.
- [ ] Run controlled backend workloads with the new rows and independently
  measured acknowledged payload, then attribute contention and block-put
  amplification. This instrumentation does not itself establish a speedup,
  physical device IOPS or the 10,000-client production target.
