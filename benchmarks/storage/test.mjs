import assert from "node:assert/strict"

import {
  BenchmarkTimeoutError,
  errorRecord,
  isTimeout,
  usageError,
  withTimeout,
} from "./errors.mjs"
import { providerById } from "./providers.mjs"
import { cleanupOwnedPaths, parseArgs, runSample } from "./runner.mjs"
import { computeStats, percentile, round, roundStats } from "./stats.mjs"

async function testStats() {
  assert.deepEqual(computeStats([]), { median: 0, p95: 0, p99: 0 })
  assert.deepEqual(computeStats([4, 1, 3, 2]), { median: 2.5, p95: 4, p99: 4 })

  const values = Array.from({ length: 20 }, (_, index) => index + 1)
  assert.deepEqual(computeStats(values), { median: 10.5, p95: 19, p99: 19 })
  assert.equal(percentile([10, 20, 30], 0), 10)
  assert.equal(percentile([10, 20, 30], 95), 30)
  assert.equal(round(1.236), 1.24)
  assert.deepEqual(roundStats({ median: 1.236, p95: 2.345, p99: 3.456 }), {
    median: 1.24,
    p95: 2.35,
    p99: 3.46,
  })

  assert.throws(() => computeStats([1, Number.NaN]), /finite/)
  assert.throws(() => percentile([1], 101), /between 0 and 100/)
}

async function testErrors() {
  assert.equal(await withTimeout(Promise.resolve("ok"), 50, "ready"), "ok")
  await assert.rejects(
    withTimeout(Promise.reject(Object.assign(new Error("boom"), { code: "EIO" })), 50, "read"),
    (error) => {
      assert.equal(error.code, "EIO")
      return true
    },
  )

  const timeout = withTimeout(new Promise(() => {}), 5, "download")
  await assert.rejects(timeout, (error) => {
    assert.equal(error instanceof BenchmarkTimeoutError, true)
    assert.equal(error.code, "BENCHMARK_TIMEOUT")
    assert.equal(error.operation, "download")
    assert.equal(error.timeoutMs, 5)
    assert.equal(isTimeout(error), true)
    return true
  })

  const nativeLike = Object.assign(new Error("missing"), {
    code: "ENOENT",
    errno: -2,
    syscall: "open",
    path: "/owned-by-test",
  })
  assert.deepEqual(errorRecord(nativeLike), {
    name: "Error",
    message: "missing",
    code: "ENOENT",
    errno: -2,
    syscall: "open",
    path: "/owned-by-test",
  })
  assert.equal(errorRecord("plain").message, "plain")
  const secretError = Object.assign(
    new Error("request failed for https://user:pass@example.test/object?token=secret"),
    { path: "https://example.test/object?secret_access_key=secret" },
  )
  const redacted = JSON.stringify(errorRecord(secretError))
  assert.equal(redacted.includes("pass@example"), false)
  assert.equal(redacted.includes("=secret"), false)
  assert.equal(usageError("bad").code, "BENCHMARK_USAGE")
}

async function testDeferredWriteCleanup() {
  let writeSettled = false
  let pathPresent = false
  let unlinkCalls = 0
  const filesystem = {
    writeFile() {
      return new Promise((resolve) => {
        setTimeout(() => {
          writeSettled = true
          pathPresent = true
          resolve()
        }, 25)
      })
    },
    unlink() {
      unlinkCalls += 1
      if (!pathPresent) {
        return Promise.reject(Object.assign(new Error("missing"), { code: "ENOENT" }))
      }
      pathPresent = false
      return Promise.resolve()
    },
  }
  const ownedPaths = new Set()
  const pendingOperations = new Map()
  const sample = await runSample({
    definition: { id: "injected-deferred-write" },
    filesystem,
    payload: Buffer.from("payload"),
    path: "/deferred-write",
    iteration: 1,
    options: { timeoutMs: 5, cleanupTimeoutMs: 10 },
    ownedPaths,
    pendingOperations,
  })
  assert.equal(sample.status, "failed")
  assert.equal(sample.cleanupSucceeded, false)
  assert.equal(sample.cleanupDeferred, true)
  assert.equal(sample.lateOperations[0].operation, "write")
  assert.equal(sample.lateOperations[0].status, "pending")
  assert.equal(unlinkCalls, 0)
  assert.equal(ownedPaths.has("/deferred-write"), true)

  const cleanup = await cleanupOwnedPaths(filesystem, ownedPaths, pendingOperations, 100)
  assert.equal(writeSettled, true)
  assert.equal(unlinkCalls, 1)
  assert.equal(cleanup.remaining, 0)
  assert.deepEqual(cleanup.failures, [])
}

async function testCli() {
  assert.deepEqual(parseArgs(["--smoke"]).sizes, [1])
  assert.deepEqual(parseArgs(["--sizes", "1,4MiB,10MB,16", "--iterations", "3", "--concurrency", "2"]), {
    help: false,
    mode: "full",
    sizes: [1, 4, 10, 16],
    iterations: 3,
    concurrency: 2,
    timeoutMs: 30_000,
    cleanupTimeoutMs: 10_000,
    chunkSizeBytes: 65_536,
    providers: null,
    output: undefined,
    payloadSeed: "mount-rs-storage-benchmark",
    networkContext: "not-provided",
    payloadBytes: undefined,
    minIops: undefined,
  })
  assert.deepEqual(parseArgs(["--smoke", "--providers", "mount-rs-memory,mountx-memory"]).providers, [
    "mount-rs-memory",
    "mountx-memory",
  ])
  assert.equal(parseArgs(["--payload-bytes", "4096", "--min-iops", "1000"]).payloadBytes, 4096)
  assert.equal(parseArgs(["--payload-bytes", "4096", "--min-iops", "1000"]).minIops, 1000)
  assert.throws(() => parseArgs(["--iterations", "0"]), /positive integer/)
  assert.throws(() => parseArgs(["--unknown"]), /unknown argument/)
}

async function testExecutionSurfaceLabels() {
  const definitions = providerById({})
  assert.deepEqual(definitions.get("mount-rs-memory").executionSurface, {
    callerRuntime: "node",
    implementationLanguage: "rust",
    apiBinding: "public-napi",
    nativeAddon: true,
    directRust: "not-run",
  })
  assert.deepEqual(definitions.get("mountx-memory").executionSurface, {
    callerRuntime: "node",
    implementationLanguage: "typescript",
    apiBinding: "actual-typescript-oracle",
    nativeAddon: false,
    directRust: "not-run",
  })
}

await testStats()
await testErrors()
await testCli()
await testExecutionSurfaceLabels()
await testDeferredWriteCleanup()
console.log("storage benchmark unit tests: PASS")
