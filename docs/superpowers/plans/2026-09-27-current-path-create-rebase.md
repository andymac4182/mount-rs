# Current-path Fresh-create Rebase Implementation Plan

> **For agentic workers:** Use subagent-driven-development for independent controls, scoped implementation and two-stage review. Root owns runtime leases and publication.

**Goal:** Eliminate unnecessary full whole-file replay for concurrent fresh creates while preserving the current-path semantics of the existing replay path.

**Architecture:** Before applying a prepared concurrent fresh create to the current batch candidate, resolve its current path, validate its current directory parent and authority, and compare the prepared layout's chunker with the current default. Rebind only the ephemeral mutation's revision and inode when those checks pass. Keep the existing apply guard, durable publication and unknown-commit handling.

**Tech Stack:** Rust, existing ChunkedFs/Namespace contracts, SQLite public filesystem controls, opt-in profile atomics, Kani on supported Linux runners.

## Global Constraints

- Base `18bc29598a4da3583a44b952cfdf75568dfef908`; branch `codex/compact-fresh-create-rebase` in the existing causal checkout.
- This optimization applies only to concurrent fresh creates. Preserve the exclusive/writeback same-revision remap and fallback acknowledgement behavior; existing-file replacements retain their original revision, path and write-base checks.
- Re-evaluate against every current accumulated batch candidate and after each confirmed uncommitted CAS loss. Do not mutate the queued original or cache an eligibility decision across attempts.
- Resolve final symlinks using the same `walk(..., true, "open", 0)` as full replay. Current-path semantics permit a changed symlink or recreated parent; bytes must land only in the current authorized directory.
- Explicitly validate the resolved parent exists and is a Directory, then call the current authority check. Preserve missing-parent, non-directory, symlink-loop and authority errors.
- Compare the prepared layout's chunker to the current namespace default. A mismatch takes the existing full replay/reprepare path. New inode uid/gid/mode/umask come from the current namespace through unchanged apply.
- Mutate only the ephemeral `expected_revision` and `inode`, together after all required checks. Preserve path, new_inode, original, layout and data_length.
- Preserve `next_inode.checked_add`, block flush before publication, exact provider CAS/physical guards, generation/backing validation, metadata flush, lease fencing, response cancellation and fail-closed unknown commits.
- A current existing path goes through the unchanged apply guard, retaining path-present counter attribution and conflict/replay behavior.
- Preserve the existing 114-row core prefix and 85 storage rows. The subsequent user-requested measurement work appends four rows as described in `2026-09-27-compact-local-work-metrics.md`; it adds no wire/storage format, dependency, dynamic label or additional captured allocation.
- Protected original checkout's 15 paths remain untouched. All runtime children use the accepted bounded owned parent with frozen source, retained logs and terminal receipts.
- Existing user authorization for the large write/performance improvement and active goal covers this scoped behavior change. Continue without another approval step.

## File Map

| File | Responsibility |
| --- | --- |
| `filesystems/mount-rs-chunked/src/lib.rs` | Register the private helper, invoke it only for concurrent fresh batch candidates, preserve unchanged apply/publication. |
| `filesystems/mount-rs-chunked/src/create_rebase.rs` | Actual current-path eligibility and ephemeral rebind helper; direct semantic controls and bounded production-linked Kani harness. |
| `filesystems/mount-rs-chunked/tests/compact_snapshot_revision.rs` | Change the real unrelated peer-update expectation to no replay, retaining full bytes/EOF/fresh reopen. |
| `filesystems/mount-rs-chunked/tests/current_path_create_rebase.rs` | Public SQLite paused-block controls for peer allocation, current symlink parent, path collision and rejected current parent. |
| `.github/workflows/ci.yml` | Explicit opt-in public controls with complete retained log and narrow Kani invocation. |
| `scripts/verify-formal` | Include the new named bounded production-helper proof in the manual formal gate. |
| `docs/bottleneck-metrics.md` | Record counter semantics, actual frozen native comparison and remaining Full capture/guard amplification. |

## Task 1: Public semantic RED

- [x] Rename the existing real peer test to `unrelated_peer_compact_update_rebases_prepared_create`. After verifying both files in a fresh reopened filesystem, expect one committed/replied request, one passed create guard, zero conflict/replay/no-publication, and unchanged payload.
- [x] Add a separate Unix opt-in target with a paused first block PUT and persistent SQLite metadata/blocks. Capture its profile baseline after the peer operation, immediately before releasing the creator, to exclude peer request counters.
- [x] Implement these real public cases:
  - `peer_allocation_rebases_prepared_create`: peer creates `/peer` while creator prepares `/created`; both distinct inode numbers and full file bytes/EOF survive fresh reopen; creator conflict/replay deltas are zero.
  - `retargeted_symlink_rebases_into_current_parent`: peer replaces `/alias` target from `/old` to `/new` while `/alias/created` is prepared; only `/new/created` is created, `/old/created` stays absent; full bytes/EOF survive fresh reopen with no creator replay.
  - `occupied_current_path_keeps_guard_conflict_and_replay`: peer creates the requested path; creator retains one path-present guard conflict and replay, and its later acknowledged full replacement survives reopen.
  - `removed_current_parent_is_rejected`: peer removes the empty parent while creator is prepared; creator returns ENOENT, no file is acknowledged or published under a detached parent, peer removal survives reopen.
- [x] Run the unchanged public revision target before production wiring; accept only the requested no-conflict/no-replay assertion failure after full readback/reopen, not compiler/setup/runtime failure as RED. Actual revision target: 1 pass, 1 expected conflict assertion failure. Actual new target: occupied-path and removed-parent pass; peer-allocation and retargeted-symlink fail the expected conflict assertion after full bytes/EOF/fresh reopen. Both runs retain 436 identical source pins, actual child exit 101 and complete owned settlement.

```sh
MOUNT_RS_PROFILE_IO=1 MOUNT_RS_TRACE_STORAGE=0 MOUNT_RS_TRACE_REQUESTS=0 \
CARGO_TARGET_DIR=/private/tmp/mount-rs-causal-metrics-target-20260927 \
./scripts/cargo-shared test -p mount-rs-chunked --test compact_snapshot_revision \
--locked --offline -- --ignored --test-threads=1 --nocapture
```

## Task 2: Current-candidate helper and batch wiring

The private interface is:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PreparedCreateRebase { Unchanged, Rebased, ChunkerChanged }

pub(super) fn rebase_prepared_create(
    namespace: &Namespace,
    current_revision: u64,
    mutation: &mut WholeFileMutation,
    require_parent: impl FnOnce(InodeId) -> Result<()>,
) -> Result<PreparedCreateRebase>
```

- [x] Return `Unchanged` without edits for a non-fresh mutation or a current existing node. Those cases still call unchanged `apply_whole_file_mutation`.
- [x] For an absent path, explicitly look up `entry.parent`, require Directory, invoke `require_parent(entry.parent)`, then compare `mutation.layout.chunker` and `namespace.default_chunker`. Return `ChunkerChanged` without edits on mismatch.
- [x] Only after all checks assign `mutation.expected_revision = current_revision` and `mutation.inode = namespace.next_inode`; return `Rebased`.
- [x] In the batch loop clone the queued mutation and current accumulated namespace as before. For concurrent fresh creates call the helper with `|parent| self.require_inode_authority(&candidate, parent)`. `ChunkerChanged` returns request Conflict; helper errors return request error; otherwise call unchanged apply. Exclusive fresh creates retain the old same-revision inode remap. Keep all publication and response code unchanged.
- [x] Direct helper controls cover full-range revision/inode rebinding, current defaults, false/failure mutation immutability, occupied path, changed chunker, non-fresh replacement, missing/non-directory/loop parent, current-parent authority denial, current symlink resolution and overflow at the unchanged apply boundary.
- [x] Add a bounded Kani harness that calls the actual helper over a concrete root graph/current path and symbolic full-range revision/inode, path occupancy, chunker match and authority refusal. Assert exact binding only for eligible creates, unchanged mutation fields otherwise, and reachable acceptance/refusal covers. State that this does not prove arbitrary path graphs, async publication or provider durability.

## Task 3: Focused gates and actual profile

- [x] Run both public targets explicitly with profiling enabled and serial execution, and direct helper controls; inspect every executed name and result.
- [x] Run core/chunked libs, compact inode, concurrent CAS, concurrent inode, existing causal/guard/allocation controls, touched formatting and strict Clippy. Preserve the existing default ignored count separately.
- [x] Add explicit CI invocation/log retention for `current_path_create_rebase`; add the named proof to the manual formal script and a narrow supported-host CI action.
- [x] Freeze source, independently review the changed source and the narrow owned-parent delta before native measurement.
- [x] Build and hash a fresh native addon; run the unchanged four SQLite arms: legacy/compact, profiling enabled/disabled, 400 iterations, concurrency 64, 4096-byte payloads, 65536-byte chunks and original 1000 logical operations/sec floor. Preserve all actual results and prior floor failures.
- [x] Verify every 400 full readback/EOF, 1200 acknowledged logical operations, exact native source/build/runtime identity, counters and owned cleanup. Measure request-conflict/replay, metadata capture/publication, inode-body bytes, PUT amplification, SQL and pager counts. Single arms are exploratory; use a controlled repeated order before claiming causal speedup.
- [ ] Independently audit actual measurement and limitations, commit scoped files after passing local gates, publish/attach a draft stacked PR and report exact-head CI. Merge only after required stacked gates pass.

## Interpretation Limits

This change removes one source of unnecessary fresh-create replay. Each preparation still captures Full metadata, and Full publication still validates physical guards. It does not implement FileCreate batching or prove independent-host durability, physical device IOPS, transport/cache performance or the full 10,000-client production topology. Current local storage and TiDB capacity floors remain binding.
