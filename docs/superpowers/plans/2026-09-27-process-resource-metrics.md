# Process resource metrics implementation plan

> **For agentic workers:** Use subagent-driven-development for source controls. Root owns serial bounded runtime gates and final review.

**Goal:** Retain process resource observations that the current collectors already capture, and enable existing disk observations at saturation workload boundaries.

**Architecture:** Extend the phase report and its closed public projection without adding sampler calls. Enable the existing own-process and explicitly selected device observer only at the two saturation boundaries. Keep the 134 core, 85 storage and 21 QUIC service inventories unchanged.

**Tech Stack:** Node 24 phase diagnostics, existing Rust resource/device observers, injected counter controls and an actual own-process OS observation.

## Constraints

- Use existing observations; add no dependencies, process/device inventory, background sampler or database work.
- Keep workload counter deltas between `before.resourcesEnd` and `after.resourcesStart`. Report boundary observation work separately.
- Missing, unsafe, negative or reset cumulative counters remain `unavailable`. A measured zero remains zero.
- Memory changes are signed endpoint differences, not allocation churn. `maxRSS` is a process lifetime high-water mark, converted from Node KiB to bytes without subtracting it as a phase peak.
- Block accounting is process `getrusage` activity, not filesystem syscall counts, device IOPS or NAND operations.
- Historical reports keep their existing CPU and end-memory observations. New optional observations remain unavailable.
- Logs and public projections retain only fixed fields. Never retain injected paths, keys, tokens or payloads.
- Use `scripts/cargo-shared` and the existing temporary Cargo target. Preserve the installed native addon and unrelated work.

## Task 1: Retain phase observations

**Files:** `benchmarks/storage/process-resource-metrics.test.mjs`, `benchmarks/storage/diagnostics.mjs`, `benchmarks/storage/owned-layout-metrics.mjs`.

- [x] Add controls using `takePhaseSnapshot`, `finishPhase` and `projectOwnedLayoutPhaseMetrics`. Prove distinct workload/observer intervals, measured zero, historical absence, malformed/unsafe/reset values, signed memory decreases, exact decimal projection and private-field exclusion.
- [x] Run the controls against the current implementation and retain semantic failures.
- [x] Append `minor_page_faults`, `major_page_faults`, `filesystem_input_operations`, and `filesystem_output_operations` to `resources_work`; retain the same four rows in `resources_observer`.
- [x] Append fixed `memory_start_bytes` and `memory_delta_bytes`, and `lifetime_peak_rss_bytes: {start, end, scope: "process_lifetime_high_water_mark; not_phase_peak"}`.
- [x] Emit exact `resource_measurement` metadata with schema `mount-rs.process-resources.v1`. The public projector accepts only that metadata and known fixed observations, preserving unavailable historical additions.
- [x] Run Node controls and existing diagnostic/projection regression gates with a bounded parent and pinned input closure.

## Task 2: Saturation disk boundaries

**Files:** `crates/mount-rs-service/tests/support/resource_profile.rs`, `crates/mount-rs-service/tests/quic_tidb_saturation.rs`, `crates/mount-rs-service/tests/support/device_io.rs`.

- [x] Add `Snapshot::capture_io_boundary(clients)` by capturing existing clients/network first and assigning `device_io::Snapshot::capture_from_env()`.
- [x] Use this method for the before/after saturation workload snapshots. Keep the periodic process sampler on its ordinary disk-disabled path.
- [x] Extend the existing ignored `own_process_os_io_boundary_retains_native_identity_and_observer_cost` control to exercise the new method with an empty client slice as well as the existing connection/process methods. Check actual own PID, available process bytes, explicit unselected device, observed interval/cost, and disabled ordinary sampling.
- [x] Run that exact real OS control with `resource-profiling`, profiling enabled, and both explicit device selection variables unset. Run scoped Rust formatting/Clippy and meaningful affected tests.

## Task 3: Publish evidence

**Files:** `docs/bottleneck-metrics.md`, this plan, scoped PR.

- [x] Document additive field semantics and remaining cache/client/WebSocket/peer timing gaps.
- [x] Independently review the patch and retained runtime evidence. Publish only the tested metrics slice; retain the publication receipt separately from concurrent E2E fixture work.
- [x] Keep CI, selected-device physical IOPS, backend daemon measurements and full production qualification distinct from this local gate.

## Evidence

The initial modeled gate had 39 passes and 24 semantic missing-observation failures.
After implementation and the independent Windows semantics correction, the new
process/projection gate passed 63 executions, the existing consumer gate passed
143, and all ten diagnostic controls passed. The scoped Rust resource suite
passed 66 cases with five default-ignored workload/OS gates; the explicitly named
own-process OS control passed one actual Mac case. Formatting and strict scoped
Clippy passed. All bounded parents retained matching source pins, log hashes,
observed child reap, group absence and pipe EOF, with no timeout or automatic retry.

The unchanged native artifact was joined to a successful current incremental
release build. Two serial full-byte-verified compact SQLite arms are retained in
`docs/benchmarks/process-resource-metrics-20260927/report.json`. Profiling-disabled
resource absence, uncontrolled cache state and logical/device distinctions are
retained. Independent source, actual-arm, CI-wiring and report reviews passed.
The scoped publication is tracked by its PR receipt. CI execution and the broad
goal are separate gates.
