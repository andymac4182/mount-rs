import assert from "node:assert/strict"

import {
  validateArtifact,
  W26_IOPS_MINIMUM,
  W26_IOPS_PROFILE,
} from "../../scripts/verify-w26-ozone-iops-artifact.mjs"
import { validateEvidencePacket } from "../../scripts/verify-w26-ozone-evidence-packet.mjs"

import {
  BenchmarkTimeoutError,
  errorRecord,
  isTimeout,
  usageError,
  withTimeout,
} from "./errors.mjs"
import { providerById, providerSummary } from "./providers.mjs"
import { cleanupOwnedPaths, parseArgs, runBenchmark, runSample } from "./runner.mjs"
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
    requireConfigured: false,
  })
  assert.deepEqual(parseArgs(["--smoke", "--providers", "mount-rs-memory,mountx-memory"]).providers, [
    "mount-rs-memory",
    "mountx-memory",
  ])
  assert.equal(parseArgs(["--payload-bytes", "4096", "--min-iops", "1000"]).payloadBytes, 4096)
  assert.equal(parseArgs(["--payload-bytes", "4096", "--min-iops", "1000"]).minIops, 1000)
  assert.equal(parseArgs(["--require-configured"]).requireConfigured, true)
  assert.throws(() => parseArgs(["--iterations", "0"]), /positive integer/)
  assert.throws(() => parseArgs(["--unknown"]), /unknown argument/)
}

async function testRequiredProviderConfiguration() {
  const result = await runBenchmark(
    parseArgs([
      "--providers",
      "mount-rs-split-tidb-r2",
      "--sizes",
      "1",
      "--iterations",
      "1",
      "--require-configured",
    ]),
    {},
  )
  assert.equal(result.status, "failed")
  assert.equal(result.counts.providersSkipped, 1)
  assert.equal(result.configurationFailures[0].provider, "mount-rs-split-tidb-r2")
  assert.ok(
    result.configurationFailures[0].missingConfiguration.includes(
      "MOUNT_RS_TIDB_URL (or TIDB_URL)",
    ),
  )
  assert.ok(
    result.configurationFailures[0].missingConfiguration.includes(
      "MOUNT_RS_R2_ENDPOINT (or R2_ENDPOINT)",
    ),
  )
}

const W26_TEST_REVISION = "a".repeat(40)

function qualificationArtifact(
  provider = "mount-rs-split-sqlite-r2",
  revision = W26_TEST_REVISION,
) {
  const providers = Array.isArray(provider) ? provider : [provider]
  const size = {
    status: "ok",
    summary: {
      successfulIterations: W26_IOPS_PROFILE.iterations,
      failedIterations: 0,
      successfulOperations: W26_IOPS_PROFILE.iterations * 3,
      attemptedOperations: W26_IOPS_PROFILE.iterations * 3,
      iops: 1_200,
      iopsTarget: W26_IOPS_MINIMUM,
      iopsTargetMet: true,
    },
  }
  return {
    schemaVersion: "mount-rs.storage-benchmark.v1",
    status: "ok",
    environment: {
      sourceControl: {
        mountRs: {
          revision,
          revisionVerified: true,
          dirty: false,
          dirtyEntryCount: 0,
        },
      },
    },
    config: {
      sizesMiB: [1],
      payloadBytes: W26_IOPS_PROFILE.payloadBytes,
      iterations: W26_IOPS_PROFILE.iterations,
      concurrency: W26_IOPS_PROFILE.concurrency,
      minIops: W26_IOPS_MINIMUM,
      requireConfigured: true,
    },
    counts: {
      providersRequested: providers.length,
      providersFailed: 0,
      providersSkipped: 0,
      configurationFailures: 0,
      sizeResultsFailed: 0,
      sizeResultsSkipped: 0,
    },
    configurationFailures: [],
    providers: providers.map((providerId) => ({
      provider: providerId,
      status: "ok",
      cleanup: {
        remainingPaths: 0,
        failures: [],
        resource: { status: "ok" },
      },
      sizes: [size],
    })),
  }
}

async function testQualificationArtifact() {
  const artifact = qualificationArtifact()
  assert.deepEqual(
    validateArtifact(artifact, {
      providers: ["mount-rs-split-sqlite-r2"],
      minimumIops: W26_IOPS_MINIMUM,
    }),
    {
      providers: ["mount-rs-split-sqlite-r2"],
      minimumIops: W26_IOPS_MINIMUM,
      sizesMiB: [1],
      profile: { ...W26_IOPS_PROFILE },
    },
  )
  assert.throws(
    () => validateArtifact({ ...artifact, config: { ...artifact.config, minIops: 999 } }, {
      providers: ["mount-rs-split-sqlite-r2"],
    }),
    /config\.minIops-must-be-safe-integer-at-least-1000/,
  )
  assert.throws(
    () => validateArtifact({ ...artifact, counts: { ...artifact.counts, providersSkipped: 1 } }, {
      providers: ["mount-rs-split-sqlite-r2"],
    }),
    /counts\.providersSkipped-must-equal-0/,
  )
}

async function testEvidencePacket() {
  const packet = {
    expectedRevision: W26_TEST_REVISION,
    policyLog: [
      "W26_OZONE_PRODUCTION_CONFIG_POLICY_PASS metadata=sqlite blocks=r2",
      "W26_OZONE_PRODUCTION_CONFIG_POLICY_PASS metadata=pglite blocks=r2",
      "W26_OZONE_PRODUCTION_CONFIG_POLICY_PASS metadata=tidb blocks=r2",
      "W26_OZONE_PRODUCTION_CONFIG_POLICY_PASS metadata=foundationdb blocks=r2",
      "W26_OZONE_PRODUCTION_CONFIG_POLICY_NEGATIVE_PASS case=insecure-blocks",
      "W26_OZONE_PRODUCTION_CONFIG_POLICY_NEGATIVE_PASS case=inline-secret",
      "W26_OZONE_PRODUCTION_CONFIG_POLICY_NEGATIVE_PASS case=foundationdb-unsafe",
      "W26_OZONE_PRODUCTION_CONFIG_POLICY_NEGATIVE_PASS case=tidb-tls-weak",
    ].join("\n"),
    baseLog: [
      "OZONE_FAULT_WINDOW_PASS container=ozone",
      "OZONE_INTEGRATION_PASS endpoint=https://ozone.example.test",
      "OZONE_CLEANUP_PASS container=ozone",
    ].join("\n"),
    compositionsLog: [
      "OZONE_COMPOSITION_PGLITE_READY endpoint=127.0.0.1:1",
      "OZONE_IOPS_PASS providers=mount-rs-split-sqlite-r2,mount-rs-split-pglite-r2 target=1000 output=artifacts/ozone-iops.json",
      "OZONE_INTEGRATION_PASS endpoint=https://ozone.example.test",
      "OZONE_CLEANUP_PASS container=ozone",
      "OZONE_COMPOSITION_CLEANUP_PASS",
    ].join("\n"),
    compositionsArtifact: qualificationArtifact([
      "mount-rs-split-sqlite-r2",
      "mount-rs-split-pglite-r2",
    ]),
    tidbLog: [
      "TIDB_ACCEPTANCE evidence=durable",
      "TIDB_OZONE_IOPS_PASS provider=tidb-r2 target=1000 output=artifacts/ozone-tidb-iops.json",
      "OZONE_INTEGRATION_PASS endpoint=https://ozone.example.test",
      "OZONE_CLEANUP_PASS container=ozone",
    ].join("\n"),
    tidbArtifact: qualificationArtifact("mount-rs-split-tidb-r2"),
    foundationdbLog: [
      "FOUNDATIONDB_NAPI_PASS image=node:24-bookworm",
      "FOUNDATIONDB_OZONE_IOPS_PASS provider=foundationdb-r2 target=1000 output=artifacts/ozone-foundationdb-iops.json",
      "FOUNDATIONDB_TEST_PASS topology=durable",
      "OZONE_INTEGRATION_PASS endpoint=https://ozone.example.test",
      "OZONE_CLEANUP_PASS container=ozone",
    ].join("\n"),
    foundationdbArtifact: qualificationArtifact("mount-rs-split-foundationdb-r2"),
  }

  assert.deepEqual(validateEvidencePacket(packet), {
    revision: W26_TEST_REVISION,
    artifacts: ["ozone-compositions", "ozone-tidb", "ozone-foundationdb"],
    policyMarkers: 8,
  })
  assert.throws(
    () => validateEvidencePacket({ ...packet, tidbLog: packet.tidbLog.replace("TIDB_ACCEPTANCE ", "") }),
    /tidb-log-missing-marker=TIDB_ACCEPTANCE/,
  )
  const mismatchedFoundationDb = structuredClone(packet.foundationdbArtifact)
  mismatchedFoundationDb.environment.sourceControl.mountRs.revision = "b".repeat(40)
  assert.throws(
    () => validateEvidencePacket({ ...packet, foundationdbArtifact: mismatchedFoundationDb }),
    /foundationdb-artifact-source-revision-does-not-match-packet/,
  )
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

async function testOzoneProviderMatrix() {
  const r2Environment = {
    MOUNT_RS_R2_ENDPOINT: "https://ozone.example.test",
    MOUNT_RS_R2_BUCKET: "bucket",
    MOUNT_RS_R2_ACCESS_KEY_ID: "access-key",
    MOUNT_RS_R2_SECRET_ACCESS_KEY: "secret-value",
  }
  const definitions = providerById(r2Environment)
  for (const provider of [
    "mount-rs-split-sqlite-r2",
    "mount-rs-split-pglite-r2",
    "mount-rs-split-tidb-r2",
    "mount-rs-split-foundationdb-r2",
  ]) {
    assert.ok(definitions.has(provider), `missing Ozone provider definition: ${provider}`)
  }
  assert.equal(
    definitions.get("mount-rs-split-sqlite-r2").availability(r2Environment).configured,
    true,
  )
  assert.equal(
    definitions.get("mount-rs-split-pglite-r2").availability(r2Environment).configured,
    false,
  )
  assert.equal(
    definitions.get("mount-rs-split-tidb-r2").availability(r2Environment).configured,
    false,
  )
  assert.equal(
    definitions.get("mount-rs-split-foundationdb-r2").availability(r2Environment).configured,
    false,
  )

  const configuredDefinitions = providerById({
    ...r2Environment,
    MOUNT_RS_PGLITE_DATABASE_URL: "postgres://pglite",
    MOUNT_RS_TIDB_URL: "mysql://tidb",
    MOUNT_RS_NAPI_FOUNDATIONDB: "1",
    MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE: "/run/fdb/fdb.cluster",
  })
  assert.equal(
    configuredDefinitions.get("mount-rs-split-pglite-r2").availability({}).configured,
    true,
  )
  assert.equal(
    configuredDefinitions.get("mount-rs-split-tidb-r2").availability({}).configured,
    true,
  )
  assert.equal(
    configuredDefinitions.get("mount-rs-split-foundationdb-r2").availability({}).configured,
    true,
  )

  const summary = JSON.stringify(
    providerSummary(
      configuredDefinitions.get("mount-rs-split-pglite-r2"),
      65_536,
    ),
  )
  assert.equal(summary.includes("secret-value"), false)
  assert.equal(summary.includes("access-key"), false)
}

await testStats()
await testErrors()
await testCli()
await testRequiredProviderConfiguration()
await testQualificationArtifact()
await testEvidencePacket()
await testExecutionSurfaceLabels()
await testOzoneProviderMatrix()
await testDeferredWriteCleanup()
console.log("storage benchmark unit tests: PASS")
