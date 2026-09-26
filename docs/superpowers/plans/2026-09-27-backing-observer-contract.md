# Backing Observer Contract Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task by task. Root owns publication and live execution.

**Goal:** Correct the bounded observer's stats request and documented block counter dialect while retaining strict identity checks and unavailable counter semantics.

**Architecture:** The observer negotiates once before bounded fanout across distinct owned CIDs, with each CID's inspect then `/stats?stream=false` sequence kept serial. Replies fold in canonical allowlist order. Each response must still echo the full CID. Block operation names use a closed mapping from `read`/`write` and `Read`/`Write` to canonical keys. Fixed identity fault flags distinguish absent, wrong-type and mismatched response IDs without retaining their values.

**Tech Stack:** Node 24.18.0, native `node:test`, injected inert HTTP and clock fixtures.

## Evidence and scope

Run [36260734302](https://github.com/andymac4182/mount-rs/actions/runs/36260734302), attempt 1, job 108455902375, reports a clean locked release build at `2cc0ac8e3896cd788081c40658bb4b63c968fc9e`. The API artifact ZIP digest matches the download, six committed benchmark source hashes match the receipts, and the build/pilot addon hashes agree. The addon binary is not retained for independent rehashing. Its 400 successful lifecycle iterations produced 1,200 application operations and 400 verified reads in 7,731.095 ms: 155.217 logical operations/s against the unchanged 1,000 floor. This is the fixed legacy direct native workload with identical 4 KiB payloads and warm cache reads. It does not characterize compact/MRC5, cold blob reads, remote traffic or the full production target.

Native phase evidence has 47 core rows, 78 storage rows and one raw API instance with 10 rows. Metadata publication accumulated 7.579 s, SQL metadata writes 7.539 s and concurrent gate waits 317.218 s. These inclusive spans overlap. They support investigating legacy publication and queueing; backing CPU, disk and network saturation are unmeasured.

All 16 selected container stats projections failed `stats_identity`; failed response bodies were discarded. The actual Engine revision and missing-versus-mismatched response shape are unavailable. The pinned [Moby v28.0.0 one-shot path](https://github.com/moby/moby/blob/v28.0.0/daemon/stats.go#L36) directly encodes `GetContainerStats`, while its normal path assigns the container ID. The [Linux builder](https://github.com/moby/moby/blob/v28.0.0/daemon/stats_unix.go#L44) sets `Read` without ID, and [StatsResponse](https://github.com/moby/moby/blob/v28.0.0/api/types/container/stats.go#L143) marks ID optional. This reproduces a supported API contract mismatch with the old request; it does not prove the unique cause of this run's discarded bodies.

The [cgroup-v2 builder](https://github.com/moby/moby/blob/v28.0.0/daemon/stats_unix.go#L148) emits lowercase `read`/`write` byte counters and omits other block fields. Supporting those byte names cannot create physical operation counters.

## Global constraints

- Own only `benchmarks/storage/backing-engine-transport.mjs`, its `.test.mjs`, `benchmarks/storage/backing-observer.mjs`, its `.test.mjs`, this new plan and the exact standard-stats matcher in `benchmarks/storage/owned-backing-pilot.test.mjs` transferred for the integration fix.
- Keep exact full-CID route/response checks, ownership labels, running state, image/start/restart identity and generation/fixture guards.
- Keep 16 containers, 257 requests, 64 boundaries, 1 MiB responses, 128 device entries, 32 interfaces, 8 MiB journal, 2,000 ms request/hook deadlines and 60,000 ms owner deadline.
- Scheduling changes are approved only in these observer/transport files. Keep at most one active request per CID and total active requests bounded by the allowlist count, at most 16.
- Do not change the pilot entry, checker, workflow, native sources, workload dimensions, storage algorithms, floors or qualification gates.
- Unknown block counters stay `null`/incomplete; no physical IOPS claim and no manufactured zeros.
- Execute only guarded offline tests. No actual Engine, native addon, backing provisioner or production controller.

## Options and timing trade-off

The chosen standard non-streaming endpoint asks the server to emit its own ID and preserves strict validation. Its [normal path discards the first update and returns the second](https://github.com/moby/moby/blob/v28.0.0/daemon/stats.go#L60). The [collector](https://github.com/moby/moby/blob/v28.0.0/daemon/stats/collector.go#L78) wakes on subscriptions, iterates registered containers, publishes and sleeps for its configured interval; [daemon initialization configures one second](https://github.com/moby/moby/blob/v28.0.0/daemon/daemon.go#L952). Subscription timing, collection cost and missed initial iterations can bring two frames to or beyond the 2,000 ms hook bound. Parallel capture removes the serial eight-peer multiplier, but cannot guarantee the bound. A controlled 100 ms first frame plus 1,000 ms second frame succeeds; a 1,000 ms first frame plus delayed 1,001 ms second frame fails honestly. These are inert contracts, not measured Engine cadence.

Report enclosing hook wall/CPU separately from the workload timer, and mark summed request wall/CPU inclusive because concurrent windows overlap. At most 16 simultaneous 1 MiB response budgets replace one simultaneous budget; per-response and journal caps remain enforced. The factory reports `peak_in_flight` and `in_flight` at the receipt cutoff.

Finalization aborts owned requests and waits for logical capture wrappers to retire before freezing counters and issues. If an injected transport ignores abort, its underlying work remains explicitly in flight and the receipt stays incomplete. The parser preserves numeric JSON types for `id`, `Id` and the closed ownership-label keys so numeric values cannot match an expected string identity. Numeric counters retain their exact source tokens. Fixed identity flags distinguish absent, wrong-type and mismatched stats IDs without retaining raw IDs or response bodies. Deadline and byte-cap faults remain the primary reason when they cause cancellation.

Accepting absent IDs or injecting the expected ID would weaken evidence and is excluded. Deadline changes remain excluded. Missing physical operation fields can still prevent full qualification after the request and scheduling fix.

### Task1: Standard stats request

**Files:** Transport and observer modules and their existing tests.

- [x] Add a failing production-linked inert test using the real observer and transport. The fixture echoes ID only for standard `/stats?stream=false`, mirroring the pinned paths. Assert a complete boundary and the exact request path.
- [x] Change existing route controls to accept only standard non-streaming stats and reject the old one-shot query, alternate ordering and extra query parameters before dispatch.
- [x] Run Node 24 with the explicit capture binding, retain RED logs and confirm request/identity failures.
- [x] Change the exact route expression and observer query to `/stats?stream=false`; record `stream=false` in the version receipt.
- [x] Add production-linked eight-member inert two-frame success, one peer failure and 2,000 ms deadline controls with canonical order, fixed identity flags and listener retirement. Verify 16 active owned CIDs, refusal of 17 and same-CID overlap, and safe late responses after replacement requests.

The production route delta is:

```javascript
const STATS = /^\/v1\.(4[1-9]|5[01])\/containers\/([a-f0-9]{64})\/stats\?stream=false$/
// captureBoundary retains the same exact entry.cid and parent signal.
const sampled = await request(`/v${version}/containers/${entry.cid}/stats?stream=false`, entry.cid, metadata.signal)
```

### Task2: Counter dialect and failure diagnosis

**Files:** Observer module and its existing test.

- [x] Add RED interval tests for lowercase byte counters, absent physical-operation counters, mixed-dialect duplicate keys and reset behavior. Existing device mismatch tests remain active.
- [x] Canonicalize only the four documented spellings before existing duplicate-key and lossless uint64 validation.
- [x] Add RED controls for fixed missing/type/mismatch identity flags. Preserve the original `stats_identity` rejection and exact-ID requirement; retain no failed response body or raw identity value.
- [x] Add RED external/concurrent finalization and ignored-abort controls, then retire bounded wrappers and preserve unresolved underlying work and primary cancellation reasons.
- [x] Add RED all-digit numeric stats/inspect IDs and numeric ownership-label controls; preserve their original JSON types while retaining lossless numeric counters.
- [x] Reproduce the combined-gate hang with the actual guarded selected-workload test; update only its obsolete one-shot request expectation and observe the same two controls finish successfully.
- [ ] Run all relevant capture benchmark/observer/transport/pilot/checker suites; inspect terminal output, syntax and scoped diff.
- [x] Freeze exact source and focused validation log hashes for independent review. Root stages and commits only reviewed paths after the joint gate and review.

The counter mapping is:

```javascript
const operation = entry?.op === "read" || entry?.op === "Read" ? "Read"
  : entry?.op === "write" || entry?.op === "Write" ? "Write" : null
if (operation === null) continue
const key = `${uint(entry.major)}:${uint(entry.minor)}:${operation}`
```

## Verification commands

From the repo root, with native loading explicitly forbidden by the existing capture suites:

```sh
NAPI_RS_NATIVE_LIBRARY_PATH="$PWD/benchmarks/storage/capture-native.cjs" \
  fnm exec --using v24.18.0 node --test \
  benchmarks/storage/backing-engine-transport.test.mjs \
  benchmarks/storage/backing-observer.test.mjs
```

Unset `NAPI_RS_FORCE_WASI`, `NAPI_RS_WASI_FLAVOR`, `NODE_PATH` and `NODE_OPTIONS` before testing. The fixture transport never connects to a socket. Retained raw diagnosis and test logs remain in a task-owned 0700 temporary directory with 0600 files.

Focused final gate: 75 tests passed, zero failed/cancelled, on Node 24.18.0. Earlier RED controls observed the original request/identity/dialect/parallel failures, two real finalization gaps and numeric identity-type collisions. Four owned JavaScript files pass syntax checks and the scoped diff passes whitespace checks. The joint whole Node gate and independent source/security review are pending root coordination before commit.

The first combined Node gate timed out after 180 seconds. A bounded selected-workload reproduction under the original capture/native/network guards also timed out after 15 seconds. Its fake HTTP response exposed headers, then asserted the obsolete `stream=false&one-shot=true` path before ending the body. Updating only that fixture matcher to `stream=false` made the same two controls pass with terminal exit 0 in 0.245 seconds. Both launches recorded their actual Popen PID/process group; the timed-out launch terminated only its owned group. No production finalization change was needed for this integration failure. Root will repeat the combined gate against the six-path freeze.

## Acceptance limits

Green inert tests establish the authored request, parser and deadline contracts. Hosted Engine response identity, actual sampling cadence, cgroup metric availability, overhead and resource qualification still require a separately authorized run. No allocation, full-target capacity or physical-device saturation verdict follows from this slice.
