import assert from "node:assert/strict"
import { readFile } from "node:fs/promises"

import {
  validateArtifact,
  W26_IOPS_MINIMUM,
  W26_IOPS_PROFILE,
} from "../../scripts/verify-w26-ozone-iops-artifact.mjs"
import { validateEvidencePacket } from "../../scripts/verify-w26-ozone-evidence-packet.mjs"
import { validateContract } from "../../scripts/verify-w26-ozone-rollout-contract.mjs"

import {
  BenchmarkTimeoutError,
  errorRecord,
  isTimeout,
  usageError,
  withTimeout,
} from "./errors.mjs"
import { foundationDbMetadataOptions, providerById, providerSummary } from "./providers.mjs"
import { cleanupOwnedPaths, parseArgs, runBenchmark, runSample, runSteadySample } from "./runner.mjs"
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
  assert.equal(parseArgs(["--layout", "inode", "--workload", "steady-overwrite"]).layout, "inode")
  assert.equal(parseArgs(["--workload", "steady-overwrite"]).workload, "steady-overwrite")
  assert.throws(() => parseArgs(["--layout", "unknown"]), /layout/)
  assert.throws(() => parseArgs(["--workload", "unknown"]), /workload/)
  assert.equal(parseArgs(["--workload", "steady-overwrite", "--payload-bytes", "1", "--iterations", "255"]).iterations, 255)
  assert.throws(() => parseArgs(["--workload", "steady-overwrite", "--payload-bytes", "1", "--iterations", "256"]), /generation capacity/)
  assert.equal(parseArgs(["--workload", "lifecycle", "--payload-bytes", "1", "--iterations", "256"]).iterations, 256)

  assert.deepEqual(parseArgs(["--smoke"]).sizes, [1])
  assert.deepEqual(parseArgs(["--sizes", "1,4MiB,10MB,16", "--iterations", "3", "--concurrency", "2"]), {
    help: false,
    mode: "full",
    layout: "legacy",
    workload: "lifecycle",
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
      writeMs: { median: 1, p95: 2, p99: 3 },
      readMs: { median: 1, p95: 2, p99: 3 },
      throughputMbps: { median: 1, p95: 2, p99: 3 },
      deleteMs: { median: 1, p95: 2, p99: 3 },
      successRate: 1,
      elapsedMs: 1_000,
      operationsPerLifecycle: 3,
      successfulIterations: W26_IOPS_PROFILE.iterations,
      failedIterations: 0,
      successfulOperations: W26_IOPS_PROFILE.iterations * 3,
      attemptedOperations: W26_IOPS_PROFILE.iterations * 3,
      iops: 1_200,
      iopsTarget: W26_IOPS_MINIMUM,
      iopsTargetMet: true,
      timeoutCount: 0,
      cleanupFailureCount: 0,
      operationSuccess: {
        write: W26_IOPS_PROFILE.iterations,
        read: W26_IOPS_PROFILE.iterations,
        delete: W26_IOPS_PROFILE.iterations,
        verifiedReads: W26_IOPS_PROFILE.iterations,
      },
      statSampleCounts: {
        writeMs: W26_IOPS_PROFILE.iterations,
        readMs: W26_IOPS_PROFILE.iterations,
        throughputMbps: W26_IOPS_PROFILE.iterations,
        deleteMs: W26_IOPS_PROFILE.iterations,
      },
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
      payloadSizesBytes: [W26_IOPS_PROFILE.payloadBytes],
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
  for (const extra of [{ layout: "inode" }, { workload: "steady-overwrite" }]) {
    assert.throws(() => validateArtifact({ ...artifact, config: { ...artifact.config, ...extra } }, { providers: ["mount-rs-split-sqlite-r2"], minimumIops: W26_IOPS_MINIMUM }), /layout|workload/)
  }
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
  assert.throws(
    () => validateArtifact({
      ...artifact,
      config: { ...artifact.config, payloadSizesBytes: [8192] },
    }, {
      providers: ["mount-rs-split-sqlite-r2"],
    }),
    /config\.payloadSizesBytes-must-match-fixed-payload-profile/,
  )
  assert.throws(
    () => validateArtifact({
      ...artifact,
      providers: [{
        ...artifact.providers[0],
        sizes: [{
          ...artifact.providers[0].sizes[0],
          summary: { ...artifact.providers[0].sizes[0].summary, timeoutCount: 1 },
        }],
      }],
    }, {
      providers: ["mount-rs-split-sqlite-r2"],
    }),
    /size\.summary\.timeoutCount-must-equal-0/,
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
      "W26_OZONE_PRODUCTION_ROLLOUT_CONTRACT_PASS providers=sqlite,pglite,tidb,foundationdb",
      "W26_OZONE_PRODUCTION_ROLLOUT_CONTRACT_NEGATIVE_PASS case=slo",
      "W26_OZONE_PRODUCTION_ROLLOUT_CONTRACT_NEGATIVE_PASS case=inline-secret",
    ].join("\n"),
    baseLog: [
      "OZONE_HEALTHY endpoint=http://127.0.0.1:9876 release=2.2.1",
      "OZONE_READY endpoint=http://127.0.0.1:9876 image=apache/ozone:2.2.1",
      "OZONE_FAULT_WINDOW_PASS container=ozone",
      "OZONE_RESTART_READY endpoint=http://127.0.0.1:9876",
      "OZONE_INTEGRATION_PASS endpoint=https://ozone.example.test",
      "OZONE_CLEANUP_PASS container=ozone",
    ].join("\n"),
    compositionsLog: [
      "OZONE_SQLITE_CHUNKED_COMPOSITION_PASS metadata=/tmp/ozone.sqlite revision=18",
      "OZONE_PGLITE_CHUNKED_COMPOSITION_PASS volume=mount-rs-ozone/metadata revision=18",
      "OZONE_CHUNKED_BOUNDED_READDIR_PASS provider_owner=ozone-sqlite-seed entries=2",
      "OZONE_CHUNKED_BOUNDED_READDIR_PASS provider_owner=ozone-pglite-seed entries=2",
      "test actual_binary_runs_live_ozone_split_provider_self_test ... ok",
      "SUMMARY node-sdk pass=8 skip=1 fail=0",
      "OZONE_NODE_CLI_PASS prefix=mount-rs-ozone/cli",
      "OZONE_CLI_REMOTE_HTTP_PASS mode=rust prefix=mount-rs-ozone/cli-http",
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
      "TIDB_RUSTFS_CHUNKED_BOUNDED_READDIR_PASS phase=seed entries=2",
      "TIDB_CHUNKED_RUSTFS_SEED_PASS",
      "TIDB_NAPI_BOUNDED_READDIR_PASS phase=seed prefix=mount-rs/tidb",
      "TIDB_NAPI_BOUNDED_READDIR_PASS phase=reopen prefix=mount-rs/tidb",
      "TIDB_ACCEPTANCE evidence=durable",
      "TIDB_OZONE_IOPS_PASS provider=tidb-r2 target=1000 output=artifacts/ozone-tidb-iops.json",
      "OZONE_INTEGRATION_PASS endpoint=https://ozone.example.test",
      "OZONE_CLEANUP_PASS container=ozone",
    ].join("\n"),
    tidbArtifact: qualificationArtifact("mount-rs-split-tidb-r2"),
    foundationdbLog: [
      "OZONE_READY endpoint=http://127.0.0.1:9876 image=apache/ozone:2.2.1",
      "OZONE_BLOCK_CONTRACT_PASS prefix=mount-rs-ozone/foundationdb",
      "OZONE_FAULT_WINDOW_PASS container=ozone",
      "OZONE_GATEWAY_FAILURE_PASS prefix=mount-rs-ozone/foundationdb error=transport",
      "OZONE_RESTART_READY endpoint=http://127.0.0.1:9876",
      "OZONE_RESTART_REOPEN_PASS prefix=mount-rs-ozone/foundationdb",
      "FOUNDATIONDB_CONFIGURED topology=durable redundancy=double storage=ssd servers=fdb1,fdb2,fdb3",
      "FOUNDATIONDB_TRANSACTION_READY server=fdb1",
      "FOUNDATIONDB_NAPI_BOUNDED_READDIR_PASS phase=seed prefix=mount-rs/foundationdb",
      "FOUNDATIONDB_NAPI_BOUNDED_READDIR_PASS phase=reopen prefix=mount-rs/foundationdb",
      "FOUNDATIONDB_NAPI_PASS image=node:24-bookworm",
      "FOUNDATIONDB_OZONE_IOPS_PASS provider=foundationdb-r2 target=1000 output=artifacts/ozone-foundationdb-iops.json",
      "FOUNDATIONDB_TEST_PASS topology=durable manifest=providers/mount-rs-foundationdb/Cargo.toml platform=linux/amd64",
      "OZONE_INTEGRATION_PASS endpoint=https://ozone.example.test",
      "OZONE_CLEANUP_PASS container=ozone",
    ].join("\n"),
    foundationdbArtifact: qualificationArtifact("mount-rs-split-foundationdb-r2"),
  }

  assert.deepEqual(validateEvidencePacket(packet), {
    revision: W26_TEST_REVISION,
    artifacts: ["ozone-compositions", "ozone-tidb", "ozone-foundationdb"],
    policyMarkers: 11,
  })
  assert.throws(
    () => validateEvidencePacket({
      ...packet,
      compositionsLog: packet.compositionsLog.replace(
        "SUMMARY node-sdk pass=8 skip=1 fail=0",
        "SUMMARY node-sdk pass=7 skip=1 fail=0",
      ),
    }),
    /ozone-compositions-log-missing-marker=SUMMARY node-sdk pass=8 skip=1 fail=0/,
  )
  assert.throws(
    () => validateEvidencePacket({ ...packet, tidbLog: packet.tidbLog.replace("TIDB_ACCEPTANCE ", "") }),
    /tidb-log-missing-marker=TIDB_ACCEPTANCE/,
  )
  assert.throws(
    () => validateEvidencePacket({
      ...packet,
      compositionsLog: packet.compositionsLog.replace("OZONE_NODE_CLI_PASS ", ""),
    }),
    /ozone-compositions-log-missing-marker=OZONE_NODE_CLI_PASS/,
  )
  const mismatchedFoundationDb = structuredClone(packet.foundationdbArtifact)
  mismatchedFoundationDb.environment.sourceControl.mountRs.revision = "b".repeat(40)
  assert.throws(
    () => validateEvidencePacket({ ...packet, foundationdbArtifact: mismatchedFoundationDb }),
    /foundationdb-artifact-source-revision-does-not-match-packet/,
  )
}

async function testProductionRolloutContract() {
  const fixturePath = new URL(
    "../../tests/ozone/production-rollout-contract.json",
    import.meta.url,
  )
  const contract = JSON.parse(await readFile(fixturePath, "utf8"))
  assert.deepEqual(validateContract(contract), {
    status: "pass",
    metadataProviders: ["sqlite", "pglite", "tidb", "foundationdb"],
    customerOwnedRecovery: true,
  })

  assert.throws(
    () => validateContract({
      ...contract,
      service: { ...contract.service, rtoMinutes: 10 },
    }),
    /service\.rtoMinutes-must-be-5/,
  )
  assert.throws(
    () => validateContract({
      ...contract,
      ozone: {
        ...contract.ozone,
        auth: { ...contract.ozone.auth, secretKeyRef: "inline-secret" },
      },
    }),
    /secretKeyRef-must-not-be-inline/,
  )
}

async function testW26WorkflowKeepsProvenanceClean() {
  const workflow = await readFile(".github/workflows/ci.yml", "utf8")
  const compositionStart = workflow.indexOf("  ozone-compositions:")
  const compositionEnd = workflow.indexOf("  ozone-tidb:", compositionStart)
  assert.notEqual(compositionStart, -1)
  assert.notEqual(compositionEnd, -1)
  const compositionJob = workflow.slice(compositionStart, compositionEnd)
  assert.match(
    compositionJob,
    /MOUNT_RS_OZONE_IOPS_OUTPUT: \$\{\{ runner\.temp \}\}\/w26-ozone-compositions\/ozone-iops\.json/u,
  )
  assert.match(
    compositionJob,
    /tee "\$RUNNER_TEMP\/w26-ozone-compositions\/ozone-compositions\.log"/u,
  )
  assert.doesNotMatch(compositionJob, /tee artifacts\/ozone-compositions\.log/u)
  assert.doesNotMatch(compositionJob, /artifacts\/ozone-iops\.json/u)

  const rustStart = workflow.indexOf("  rust:")
  const rustEnd = workflow.indexOf("  http-observability:", rustStart)
  assert.notEqual(rustStart, -1)
  assert.notEqual(rustEnd, -1)
  assert.match(workflow.slice(rustStart, rustEnd), /timeout-minutes: 25/u)
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

const fdbConfig = { clusterFile: "/owned/fdb.cluster", leaseAuthority: "shared-provider", sharedProvider: true, authorityPrefix: "owned-authority" }
assert.equal(foundationDbMetadataOptions(fdbConfig, { runId: "test", layout: "legacy" }).authorityPrefix, "owned-authority")
const inodeFdb = foundationDbMetadataOptions(fdbConfig, { runId: "test", layout: "inode" })
assert.equal(inodeFdb.leaseAuthority, "revision-cas")
assert.equal("authorityPrefix" in inodeFdb, false)

async function testSteadyOverwriteOracle() {
  const initial = Buffer.from([0x51, 1, 2, 3, 0xa7])
  const calls = []
  const handle = {
    async write(bytes, offset, length, position) {
      calls.push(["write", position, length])
      bytes.copy(initial, position, offset, offset + length)
      return { bytesWritten: length }
    },
    async read(target, offset, length, position) {
      calls.push(["read", position, length])
      initial.copy(target, offset, position, position + length)
      return { bytesRead: length, buffer: target }
    },
  }
  const args = { definition: { id: "steady-test" }, handle, payload: Buffer.from([1, 2, 3]), path: "/owned", iteration: 1, generation: 1, options: { timeoutMs: 100, cleanupTimeoutMs: 100 }, pendingOperations: new Map() }
  const success = await runSteadySample(args)
  assert.equal(success.success, true)
  assert.equal(success.deleteSucceeded, null)
  assert.deepEqual(calls, [["write", 1, 3], ["read", 0, 5]])
  initial[0] = 0 // An unchanged boundary is still part of the full byte oracle.
  const corrupt = await runSteadySample({ ...args, iteration: 2, generation: 2 })
  assert.equal(corrupt.success, false)
  assert.equal(corrupt.payloadVerified, false)
  const partial = await runSteadySample({ ...args, handle: { ...handle, async write() { return { bytesWritten: 1 } } } })
  assert.equal(partial.success, false)
  assert.equal(partial.readSucceeded, false)
  let attempts = 0
  const pendingArgs = { ...args, options: { timeoutMs: 5, cleanupTimeoutMs: 5 }, handle: { async write() { attempts += 1; return new Promise(() => {}) } }, pendingOperations: new Map() }
  assert.equal((await runSteadySample(pendingArgs)).success, false)
  assert.equal(pendingArgs.pendingOperations.size, 1)
  assert.equal((await runSteadySample(pendingArgs)).success, false)
  assert.equal(attempts, 1, "unresolved operation must not be replayed on its handle")
}
async function testSteadyGenerationsRejectDroppedWrites() {
  function lane() {
    const payload = Buffer.alloc(8, 0x39)
    const file = Buffer.concat([Buffer.from([0x51]), payload, Buffer.from([0xa7])])
    const state = { payload, file, drop: false }
    state.handle = {
      async write(bytes, offset, length, position) {
        if (!state.drop) bytes.copy(file, position, offset, offset + length)
        return { bytesWritten: length }
      },
      async read(target) { file.copy(target); return { bytesRead: file.length, buffer: target } },
    }
    return state
  }
  async function sample(state, iteration, generation) {
    return runSteadySample({ definition: { id: "generation-test" }, handle: state.handle, payload: state.payload, path: "/owned", iteration, generation, options: { timeoutMs: 100, cleanupTimeoutMs: 100 }, pendingOperations: new Map() })
  }
  const distinct = lane()
  const seen = new Set([distinct.file.toString("hex")])
  for (const generation of [1, 256, 257, Number.MAX_SAFE_INTEGER]) {
    assert.equal((await sample(distinct, generation, generation)).success, true)
    seen.add(distinct.file.toString("hex"))
  }
  assert.equal(seen.size, 5, "all supported generations must differ from setup and each other")
  await assert.rejects(() => runSteadySample({ definition: { id: "tiny" }, payload: Buffer.alloc(1), generation: 256, path: "/tiny", iteration: 256, pendingOperations: new Map() }), /generation.*capacity/)
  const first256 = lane()
  first256.drop = true
  const firstOverwrite = await sample(first256, 256, 1) // First operation of lane 256.
  const recurring = lane()
  assert.equal((await sample(recurring, 1, 1)).success, true)
  recurring.drop = true
  const stale257 = await sample(recurring, 257, 2) // Scheduler returns to the same lane.
  const sequence256 = lane()
  sequence256.drop = true
  const wrappedSequence = await sample(sequence256, 256, 256)
  const recurring257 = lane()
  assert.equal((await sample(recurring257, 1, 1)).success, true)
  recurring257.drop = true
  const wrappedStale = await sample(recurring257, 257, 257)
  assert.deepEqual(
    [firstOverwrite.success, stale257.success, wrappedSequence.success, wrappedStale.success],
    [false, false, false, false],
    "iteration 256/concurrency 256 and same-lane 1/257 must reject dropped writes",
  )
}
await testSteadyGenerationsRejectDroppedWrites()
await testSteadyOverwriteOracle()
await testStats()
await testErrors()
await testCli()
await testRequiredProviderConfiguration()
await testQualificationArtifact()
await testEvidencePacket()
await testProductionRolloutContract()
await testW26WorkflowKeepsProvenanceClean()
await testExecutionSurfaceLabels()
await testOzoneProviderMatrix()
await testDeferredWriteCleanup()
console.log("storage benchmark unit tests: PASS")
