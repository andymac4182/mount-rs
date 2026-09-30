# Production runner phase metrics

## Goal and authority

Implement the approved fixture collector brief `/private/tmp/mount-rs-production-runner-phase-metrics-implementation-brief-20260926.md` (SHA256 `0ebb42ee94e85c23ea08ee8d5c05b0f5f27478826a9f54a3d2bc7cd4b0c9ac97`). Historical runner acceptance at `ff30ddc90d2014c24e98d723a71844726df60861` and its frozen evidence remain unchanged. Starting checkout HEAD is `1ad3a1a12e29a1743534bca42b0c3cec4049e0f9`, with separately owned dirty production changes.

Ownership is restricted to `production_target/{metrics,process,mod,oracle,timing,resources,workload}.rs`, necessary integration-test wiring, and this plan. No dependencies, provider/runtime/service edits, staging or commits. Evidence belongs to `/private/tmp/mount-rs-production-phase-metrics-evidence-20260926`. Cargo/native execution requires the root coordinator's serialized lease.

## Design

A bounded sequence of private command-file requests obtains immutable worker metric receipts at phase boundaries. Every receipt binds launched PID, role, server index, controller PID, source/binary/catalog/backend identity, replica generation, sequence, phase and boundary. Stop/reopen retains priority; one request may be outstanding per worker. Duplicate identical requests reuse prior evidence, while conflicting/stale commands fail closed. All ten workers share one deadline of at most30 seconds inside existing setup/work budgets. Partial evidence survives failure.

Each process owns its core/storage/process baseline. Keep raw snapshots and checked deltas, separate counters from gauges, and report disabled, unavailable, not configured, incomplete and nonquiescent independently. Direct SDK storage rows use the existing core recorder supplied by its separate owner. No NAPI raw-bank data is attributed to these Rust processes. SQLite diagnostics stay unavailable unless their existing lock-based API can be called with retained bounded ownership; this slice creates no blocking or detached observation actor.

The separately owned local RemoteServer observer will provide serializable server snapshots through an explicit diagnostic constructor, with no network endpoint. The separately owned resource helper adds opt-in boundary process/device I/O capture, leaving the100ms sampler untouched. Consume each only after its actual accepted API exists; missing dependencies remain explicit unavailable families.

Controller oracle counters retain fixed categories, outcomes, elapsed wall and known bytes across cancellation. Record process CPU as inclusive process cost and observer wall separately. Core/storage wall intervals overlap; API calls, HTTP attempts, STREAM frames, UDP payload and physical I/O remain different quantities. Preserve active throughput timing and report idle-liveness and metric-observer intervals separately.

## Ordered work and gates

1. Write failing identity, checked delta and immutable-publication contract tests. Obtain the Cargo RED slot and retain actual failing output before implementation.
2. Implement minimal receipt helpers and their sequence/coverage contracts; test disabled/missing/reset/partial states and duplicate behavior.
3. Wire worker commands and controller barriers, retaining lifecycle priority, partial receipts and all existing owners. Add fixed phase boundaries without per-file collection or growing historical ledger snapshots.
4. Add retained oracle scalar accounting and observer costs. Test cancellation and stage timing with delayed observation; preserve all existing byte/membership/EOF checks.
5. Integrate accepted local service and process/device observer APIs, explicitly marking remaining coverage gaps.
6. Run scoped formatting, feature/default tests and strict Clippy under the root's serialized Cargo lease. Preserve exact commands, exits and source/binary manifests.
7. Propose the smallest ten-worker diagnostic smoke with concrete ownership, resource, deadline and cleanup limits. Execute only after root clearance, then freeze source and all receipts for independent review.

The initial RED command is:

```sh
CARGO_TARGET_DIR=/private/tmp/mount-rs-public-compact-selection-cargo-target \
  ./scripts/cargo-shared test --locked -p mount-rs-service \
  --features resource-profiling --test quic_production_target \
  target::metrics::tests:: -- --nocapture --test-threads=1
```

## Invariants

Full workload dimensions, modes, routes and oracles are unchanged. Setup600s, work1800s, phase600s, request30s, cleanup95s, final sampler1s, resource floors and existing capability gates remain exact. Metrics completion is separate from workload completion. No full-target, TiDB/cache load, fixture action or capacity claim is authorized by this plan. Profiling disabled is never presented as measured zero. Per-observer CPU isolation and direct SDK raw object-store/HTTP coverage remain explicit gaps.

## Implemented checkpoint

The collector now consumes the separately validated local service observer and OS boundary APIs. Startup, normal command, closed-generation and final worker receipts retain the observer from their actual server generation. Service activity envelopes bracket local core/storage/process capture; changing activity or an incomplete transport registry prevents metrics qualification. Connection retirement totals, UDP counters and frame counters use checked deltas within one process/generation, while RTT/cwnd/MTU and process memory values remain endpoint gauges.

Optional allocation counters are checked only when the allocator instrumentation was enabled in both endpoints. Resource-only builds mark allocation churn unavailable. Fixed oracle/observer accounting retains cancellation, has no timer or recorder clone in disabled spans, and marks counter saturation incomplete. The terminal check includes the completed sampler and metric-publication accounting, except the final receipt's own write. Wall costs overlap and are not exclusive observer CPU.

Actual gates in the new evidence packet: initial metrics8 RED and timing1 RED; expanded metrics13 RED had eight passes and five intended failures; corrected metrics13 and timing1 GREEN, scoped strict Clippy and touched formatting all exited0 with stable source manifests. The extra startup/closed observer wiring correction was found by independent source review; it has no separate claimed RED. Actual ten-worker lifecycle evidence remains pending root clearance. Default-feature, allocation-enabled and selected-device native coverage are not claimed by these focused gates.

Independent review also identified a reset allowance in boundary timing and fallible worker metrics initialization outside cleanup. The corrected runner retains absolute setup/work and per-phase deadlines; before/work/after (including active/idle work) share the phase deadline, and each barrier is additionally capped at30s. A paused real pending-future deadline regression observed the old fresh allowance, then passed for expired and one-second remaining budgets. Observer initialization now runs inside the retained worker result/cleanup funnel; failure retains an explicit unavailable terminal metric with normal context/subprocess/sampler cleanup. This cleanup correction is source-reviewed, without a separate new native failure injection. Final focused count is14 metrics tests plus one timing test, with scoped strict Clippy and touched formatting exit0.
