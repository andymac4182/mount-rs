# Metadata refresh stage metrics

> **For agentic workers:** Execute the independent source tasks with the subagent-driven-development skill. Root owns serial runtime gates and final review.

**Goal:** Explain repeated metadata work before changing storage format or refresh semantics.

**Architecture:** Append sixteen fixed events to the existing opt-in profile bank. SQLite scopes expose authority queries, physical-path checks, anchor query/decode, selected/full guard scans and decoding, and read connection/transaction setup. Filesystem scopes identify the refresh caller without changing its validation, retry, authority or publication behavior.

**Tech Stack:** Existing Rust `profile::Span`, SQLite fixtures, Node benchmark projectors.

## Global constraints

- Preserve the first 118 core labels and the 85 storage labels; current core inventory becomes 134.
- No new dependencies, dynamic labels, extra database queries, graph traversals, path/payload logging or retry changes.
- All durations are inclusive wall time. Guard scans include decoding and map insertion; decode rows are nested and cannot be added as exclusive time.
- Attempted guard rows are counted as the existing cursor yields them, before decoding. Decode units use existing JSON input lengths, including malformed input.
- Use `MOUNT_RS_PROFILE_IO=1`; bounded slow logs reuse `MOUNT_RS_TRACE_STORAGE=1`, the existing 100 ms threshold and 16-record process budget.
- Run normal Rust through `scripts/cargo-shared` with the existing separate temporary target. Preserve unrelated work and the installed native addon.

## Task 1: Actual operation controls and SQLite stages

**Files:** `providers/mount-rs-sqlite/src/compact_tests.rs`, `providers/mount-rs-sqlite/src/compact.rs`, `src/diagnostics/profile.rs`.

- [x] Add an ignored serial real-SQLite test named `compact_profile_stages_account_for_selected_full_and_failed_decode`. Use string label lookup so missing measurements fail assertions rather than compilation.
- [x] Run the exact test with profiling enabled and retain its semantic failure.
- [x] Wrap authority SELECT and physical-stamp validation separately; wrap anchor SELECT/extraction and decode/authority comparison separately. Use successful returned JSON bytes for query units and attempted input bytes for decode units.
- [x] In `guards`, use one selected/full span around its existing scan and increment its units on each yielded row. Pass the fixed selected/full decode event into the existing decoder, without copying its input.
- [x] Wrap only read-side connection lock acquisition and deferred transaction construction in `compact_snapshot` and `compact_load`.
- [x] Assert selected reads observe one guard and Full snapshots observe all guards; malformed guard bodies still record attempted work and fail closed.

## Task 2: Refresh caller scopes

**Files:** `filesystems/mount-rs-chunked/tests/filesystem_causal_metrics.rs`, `filesystems/mount-rs-chunked/src/lib.rs`.

- [x] Extend the existing persisted SQLite public control with exact fresh-create, path, data-read and EOF refresh counts after its full-byte/fresh-reopen oracle.
- [x] Run the existing isolated causal gate and retain the missing-row failure.
- [x] Add `Span::new(Event::FilesystemRefreshReplaceProbe)` around the actual replacement probe refresh; similarly scope create capture, batch capture, path structure, read before and read after. Every scope ends immediately after its existing refresh await.
- [x] Preserve the existing per-traversed-inode `filesystem.inode_path_guard` row. Read before/after rows count EOF checks too.

## Task 3: Export, logging and validation

**Files:** `tests/filesystem_causal_profile_allocations.rs`, the six owned-layout/pilot projector and test files, `.github/workflows/ci.yml`, `docs/bottleneck-metrics.md`.

- [x] Append the sixteen fixed rows to Rust and all closed consumer inventories in identical order. Retain exact integer strings and missing historical rows as unavailable.
- [x] Extend the warmed actual Span/add/drop allocation control to all appended rows. Keep slow-log smoke coverage and disabled-path clock/recording controls.
- [x] Add the exact ignored SQLite control to CI with profiling on and one test thread.
- [x] Run focused core/chunked/SQLite tests, the exact profiling controls, Node consumers, formatting and strict scoped Clippy. Review changes independently before publication.
- [x] Build a fresh native artifact and run the unchanged controlled SQLite lifecycle workload. Retain source/binary identity, full payload verification, raw profile output and original failures; report no physical IOPS or production-capacity claim from logical counters.


## Evidence

Semantic RED and GREEN receipts, the 354 Rust / 143 Node gates, warmed recording allocation control, fresh native build and both 400-lifecycle arms are summarized in `docs/bottleneck-metrics.md`. Source review corrected the Unix CI gate. CI execution/merge and the broader production goal remain pending.
