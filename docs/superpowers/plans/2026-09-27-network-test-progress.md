# Network Test Progress Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this single task inline with an independent frozen-patch review. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Retain bounded local progress and process-resource evidence when the existing S3 provider network test stalls.

**Architecture:** A test-only observer keeps eight fixed request-stage counters. It samples already-local Node process resources at startup, five-second publication boundaries and completion, and immediately freezes its first request failure. It performs no server or provider RPC and preserves the existing workload and outcome.

**Tech Stack:** Node.js 24, `node:test`, existing N-API S3 test, GitHub Actions.

## Global Constraints

- Default concurrency is `32`; the existing accepted range is `1..64`.
- The existing request `AbortSignal.timeout(15_000)` remains unchanged.
- Each JSON record including prefix and newline is at most `4096` bytes; at most `16` publication attempts occur per case.
- Eight closed stage labels; provider is `node-fs` or `sqlite`; no object keys, paths, payloads, error strings or new native calls.
- Counters describe client promise issuance and settlement, not server acceptance, HTTP success or backend IOPS.
- CPU is the Node process delta in microseconds; RSS includes the embedded addon; OS maxRSS is process lifetime and converted from kibibytes to bytes. Capture is non-atomic and does not prove application drain.
- The five-second timer is cooperative publication, not continuous sampling or a hard scheduling guarantee.
- Reporter failures preserve the original workload error and deadline; a terminal record is immutable and late promise settlements cannot rewrite it.
- Protected eleven dirty files and Cargo.lock remain unchanged; no Cargo lease is needed for the pure observer gate.

---

### Task 1: Bounded S3 network test observer

**Files:**
- Create: `bindings/mount-rs-napi/test/network-progress.mjs`
- Create: `bindings/mount-rs-napi/test/network-progress.test.mjs`
- Modify: `bindings/mount-rs-napi/test/s3-provider-network-concurrency.mjs`
- Modify: `.github/workflows/ci.yml` (select the pure observer tests in Node lanes)
- Modify: `docs/bottleneck-metrics.md`

**Interfaces:**
- Produces `createNetworkProgress({ provider, concurrency, enabled?, sample?, clock?, write?, schedule?, cancel? })`.
- Returns `begin(stage)`, `settle(stage, outcome, index?, error?)`, `finish(outcome)` and `receipt()`; `outcome` is `fulfilled|rejected` for settlement and `passed|failed` for completion.
- `sample()` returns `{ cpu_user_us, cpu_system_us, rss_current_bytes, rss_lifetime_peak_bytes }`; `clock()` returns `{ unix_ms, monotonic_ms }`.
- `write(line)` uses the fixed `network_test_progress ` prefix. `false` means backpressure; exceptions are observer failures, never request failures.
- `schedule(callback, 5000)` and `cancel(handle)` own one timer. The disabled path returns before sample, clock, write and timer calls.

- [x] Write and run the missing-module RED test with a controlled clock/resource source and sink. Require fixed schema, process CPU deltas, immutable first failure, pending counts and timer cancellation.

```js
import assert from "node:assert/strict"
import { test } from "node:test"
import { createNetworkProgress } from "./network-progress.mjs"

test("first rejected request freezes pending counts and clears the timer", () => {
  const lines = []
  let cleared = 0
  const progress = createNetworkProgress({
    provider: "sqlite", concurrency: 32,
    sample: () => ({ cpu_user_us: 100, cpu_system_us: 10,
      rss_current_bytes: 1024, rss_lifetime_peak_bytes: 2048 }),
    clock: () => ({ unix_ms: 1000, monotonic_ms: 10 }),
    write: (line) => { lines.push(line); return true },
    schedule: () => 1, cancel: () => { cleared++ },
  })
  progress.begin("concurrent PUT fetch")
  progress.begin("concurrent PUT fetch")
  progress.settle("concurrent PUT fetch", "rejected", 1,
    new DOMException("private error", "TimeoutError"))
  const final = progress.receipt()
  progress.settle("concurrent PUT fetch", "fulfilled", 0)
  progress.finish("passed")
  assert.equal(progress.receipt(), final)
  assert.equal(final.status, "failed")
  assert.equal(final.failure_category, "timeout")
  assert.equal(final.rows[0].in_flight, 1)
  assert.equal(cleared, 1)
  assert.equal(lines.join("").includes("private error"), false)
})
```

Run: `fnm exec --using v24.18.0 node --test --test-timeout=10000 bindings/mount-rs-napi/test/network-progress.test.mjs`. Expect missing-module failure before implementation.

- [x] Implement the fixed counter observer. Keep event recording free of added clocks, sampling and publication; only failure/cadence/completion calls capture resources. Freeze terminal receipts recursively. Reject invalid events as observation failures without throwing through the request.
- [x] Add controls for disabled observers, first rejection, terminal late settlement, success, cooperative cadence, sink errors/backpressure, sample/clock failures, invalid events and 4096-byte/16-publication limits. Run the same bounded Node gate and retain its output.
- [x] Integrate `begin`/`settle` around the existing `withRequestStage` action. Construct one observer per case; finish after existing cleanup. Keep every `15_000`, concurrency assertion, status/body equality and stats assertion unchanged.

```js
progress.begin(stage)
try {
  const result = await action()
  progress.settle(stage, "fulfilled", index)
  return result
} catch (error) {
  progress.settle(stage, "rejected", index, error)
  throw new Error(`${label} ${stage} ${index} failed after ${Math.round(performance.now() - started)} ms`, { cause: error })
}
```

- [x] Run syntax checks on all three JS files, the pure gate, YAML parsing, `git diff --check` and protected-byte checks.
- [ ] Independently review the frozen six-file patch including this plan, then commit only this scope. A native network test is separate evidence; the pure gate does not prove native behavior or fix the observed Windows timeout.
- [ ] Publish the reviewed source and retain the next clean runtime logs before claiming a stall cause or a timeout fix. Keep the full production goal active.

## Evidence and limits

At source `a87868526dbc432bb4a8f28f5961c0532347dcc8`, PR Windows Node job `108480922210` failed at a 15,004 ms SQLite concurrent PUT fetch. The identical Git tree passed the same 32-pair test in push job `108480913645`. The failed log retained no request-progress or process-resource records. This motivates collection, not a unique cause.

Node resource units follow [Node 24 process.resourceUsage](https://nodejs.org/docs/latest-v24.x/api/process.html#processresourceusage) and [process.memoryUsage.rss](https://nodejs.org/docs/latest-v24.x/api/process.html#processmemoryusagerss). No physical IOPS claim is made.

The initial missing-module gate failed as expected. The initial 13-test gate
passed; added controls then exposed stale sink/timer health receipts and mutable
baseline aliases, which were repaired. A 14-test gate passed after repair.
The existing macOS arm64 addon was hashed and exercised with the new JS logger:
both 32-pair NodeFs/SQLite cases, streamed bytes and native statistics passed,
with four bounded progress records. That existing binary is not a clean rebuild
of this source and does not establish Windows behavior. Final expanded pure
controls passed (15 tests). The disabled native control also passed and emitted
no progress records. These two local runs do not measure observer overhead with
statistical confidence. Independent review and clean Linux/Windows runtime
evidence remain separate gates.
