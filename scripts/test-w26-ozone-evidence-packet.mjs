#!/usr/bin/env node

// Exercise the complete packet with the markers emitted by the actual Ozone
// FoundationDB path: Ozone gateway recovery, FoundationDB readiness, N-API
// split-store reopen, and the qualified FoundationDB/R2 workload artifact.

import assert from "node:assert/strict"
import { validateEvidencePacket } from "./verify-w26-ozone-evidence-packet.mjs"

const revision = "a".repeat(40)
const benchmarkProfile = {
  iterations: 400,
  concurrency: 64,
  payloadBytes: 4096,
  minimumIops: 1000,
}

function artifact(providerIds) {
  const sample = { median: 1, p95: 2, p99: 3 }
  const summary = {
    successRate: 1,
    elapsedMs: 1,
    operationsPerLifecycle: 3,
    timeoutCount: 0,
    cleanupFailureCount: 0,
    successfulIterations: benchmarkProfile.iterations,
    failedIterations: 0,
    successfulOperations: benchmarkProfile.iterations * 3,
    attemptedOperations: benchmarkProfile.iterations * 3,
    operationSuccess: {
      write: benchmarkProfile.iterations,
      read: benchmarkProfile.iterations,
      delete: benchmarkProfile.iterations,
      verifiedReads: benchmarkProfile.iterations,
    },
    statSampleCounts: {
      writeMs: benchmarkProfile.iterations,
      readMs: benchmarkProfile.iterations,
      throughputMbps: benchmarkProfile.iterations,
      deleteMs: benchmarkProfile.iterations,
    },
    writeMs: sample,
    readMs: sample,
    throughputMbps: sample,
    deleteMs: sample,
    iops: benchmarkProfile.minimumIops,
    iopsTarget: benchmarkProfile.minimumIops,
    iopsTargetMet: true,
  }
  return {
    schemaVersion: "mount-rs.storage-benchmark.v1",
    status: "ok",
    environment: {
      sourceControl: {
        mountRs: {
          revisionVerified: true,
          revision,
          dirty: false,
          dirtyEntryCount: 0,
        },
      },
    },
    config: {
      requireConfigured: true,
      minIops: benchmarkProfile.minimumIops,
      payloadBytes: benchmarkProfile.payloadBytes,
      iterations: benchmarkProfile.iterations,
      concurrency: benchmarkProfile.concurrency,
      sizesMiB: [1],
      payloadSizesBytes: [benchmarkProfile.payloadBytes],
    },
    counts: {
      providersRequested: providerIds.length,
      providersFailed: 0,
      providersSkipped: 0,
      configurationFailures: 0,
      sizeResultsFailed: 0,
      sizeResultsSkipped: 0,
    },
    configurationFailures: [],
    providers: providerIds.map((provider) => ({
      provider,
      status: "ok",
      cleanup: { remainingPaths: 0, failures: [], resource: { status: "ok" } },
      sizes: [{ status: "ok", summary: structuredClone(summary) }],
    })),
  }
}

function log(lines) {
  return `${lines.join("\n")}\n`
}

function packet() {
  return {
    expectedRevision: revision,
    policyLog: log([
      "W26_OZONE_PRODUCTION_CONFIG_POLICY_PASS metadata=sqlite accepted",
      "W26_OZONE_PRODUCTION_CONFIG_POLICY_PASS metadata=pglite accepted",
      "W26_OZONE_PRODUCTION_CONFIG_POLICY_PASS metadata=tidb accepted",
      "W26_OZONE_PRODUCTION_CONFIG_POLICY_PASS metadata=foundationdb accepted",
      "W26_OZONE_PRODUCTION_CONFIG_POLICY_NEGATIVE_PASS case=insecure-blocks",
      "W26_OZONE_PRODUCTION_CONFIG_POLICY_NEGATIVE_PASS case=inline-secret",
      "W26_OZONE_PRODUCTION_CONFIG_POLICY_NEGATIVE_PASS case=foundationdb-unsafe",
      "W26_OZONE_PRODUCTION_CONFIG_POLICY_NEGATIVE_PASS case=tidb-tls-weak",
      "W26_OZONE_PRODUCTION_ROLLOUT_CONTRACT_PASS accepted",
      "W26_OZONE_PRODUCTION_ROLLOUT_CONTRACT_NEGATIVE_PASS case=slo",
      "W26_OZONE_PRODUCTION_ROLLOUT_CONTRACT_NEGATIVE_PASS case=inline-secret",
    ]),
    baseLog: log([
      "OZONE_HEALTHY endpoint=http://127.0.0.1:9878",
      "OZONE_READY endpoint=http://127.0.0.1:9878",
      "OZONE_FAULT_WINDOW_PASS container=ozone",
      "OZONE_RESTART_READY endpoint=http://127.0.0.1:9878",
      "OZONE_INTEGRATION_PASS endpoint=http://127.0.0.1:9878",
      "OZONE_CLEANUP_PASS container=ozone",
    ]),
    compositionsLog: log([
      "OZONE_SQLITE_CHUNKED_COMPOSITION_PASS revision=test",
      "OZONE_PGLITE_CHUNKED_COMPOSITION_PASS revision=test",
      "OZONE_CHUNKED_BOUNDED_READDIR_PASS provider_owner=ozone-sqlite-seed entries=1",
      "OZONE_CHUNKED_BOUNDED_READDIR_PASS provider_owner=ozone-pglite-seed entries=1",
      "test actual_binary_runs_live_ozone_split_provider_self_test ... ok",
      "SUMMARY node-sdk pass=8 skip=1 fail=0",
      "OZONE_NODE_CLI_PASS mode=cli",
      "OZONE_CLI_REMOTE_HTTP_PASS mode=http",
      "OZONE_COMPOSITION_PGLITE_READY uri=postgres://local",
      "OZONE_IOPS_PASS providers=mount-rs-split-sqlite-r2,mount-rs-split-pglite-r2 target=1000 output=fixture",
      "OZONE_INTEGRATION_PASS endpoint=http://127.0.0.1:9878",
      "OZONE_CLEANUP_PASS container=ozone",
      "OZONE_COMPOSITION_CLEANUP_PASS",
    ]),
    compositionsArtifact: artifact(["mount-rs-split-sqlite-r2", "mount-rs-split-pglite-r2"]),
    tidbLog: log([
      "TIDB_RUSTFS_CHUNKED_BOUNDED_READDIR_PASS phase=seed entries=1",
      "TIDB_CHUNKED_RUSTFS_SEED_PASS volume=test",
      "TIDB_NAPI_BOUNDED_READDIR_PASS phase=seed entries=1",
      "TIDB_NAPI_BOUNDED_READDIR_PASS phase=reopen entries=1",
      "TIDB_ACCEPTANCE topology=durable",
      "TIDB_OZONE_IOPS_PASS provider=tidb-r2 target=1000 output=fixture",
      "OZONE_INTEGRATION_PASS endpoint=http://127.0.0.1:9878",
      "OZONE_CLEANUP_PASS container=ozone",
    ]),
    tidbArtifact: artifact(["mount-rs-split-tidb-r2"]),
    foundationdbLog: log([
      "OZONE_HEALTHY endpoint=http://127.0.0.1:9878",
      "OZONE_READY endpoint=http://127.0.0.1:9878",
      "OZONE_BLOCK_CONTRACT_PASS prefix=mount-rs-ozone/test",
      "OZONE_FAULT_WINDOW_PASS container=ozone",
      "OZONE_GATEWAY_FAILURE_PASS prefix=mount-rs-ozone/test error=transport",
      "OZONE_RESTART_READY endpoint=http://127.0.0.1:9878",
      "OZONE_RESTART_REOPEN_PASS prefix=mount-rs-ozone/test",
      "FOUNDATIONDB_CONFIGURED topology=durable redundancy=double storage=ssd servers=fdb1,fdb2,fdb3",
      "FOUNDATIONDB_TRANSACTION_READY server=fdb1",
      "FOUNDATIONDB_NAPI_BOUNDED_READDIR_PASS phase=seed prefix=test",
      "FOUNDATIONDB_NAPI_BOUNDED_READDIR_PASS phase=reopen prefix=test",
      "FOUNDATIONDB_NAPI_PASS image=node:24-bookworm",
      "FOUNDATIONDB_OZONE_IOPS_PASS provider=foundationdb-r2 target=1000 output=fixture",
      "FOUNDATIONDB_TEST_PASS topology=durable manifest=providers/mount-rs-foundationdb/Cargo.toml platform=linux/amd64",
      "OZONE_INTEGRATION_PASS endpoint=http://127.0.0.1:9878",
      "OZONE_CLEANUP_PASS container=ozone",
    ]),
    foundationdbArtifact: artifact(["mount-rs-split-foundationdb-r2"]),
  }
}

const accepted = validateEvidencePacket(packet())
assert.deepEqual(accepted.artifacts, [
  "ozone-compositions",
  "ozone-tidb",
  "ozone-foundationdb",
])

const missingGatewayRecovery = packet()
missingGatewayRecovery.foundationdbLog = missingGatewayRecovery.foundationdbLog.replace(
  "OZONE_RESTART_REOPEN_PASS prefix=mount-rs-ozone/test",
  "OZONE_RESTART_REOPEN_FAIL prefix=mount-rs-ozone/test",
)
assert.throws(
  () => validateEvidencePacket(missingGatewayRecovery),
  /ozone-foundationdb-log-missing-marker=OZONE_RESTART_REOPEN_PASS prefix=/,
)

const wrongProvider = packet()
wrongProvider.foundationdbArtifact.providers[0].provider = "mount-rs-foundationdb"
assert.throws(
  () => validateEvidencePacket(wrongProvider),
  /ozone-foundationdb-artifact-invalid/,
)

console.log("W26_OZONE_EVIDENCE_PACKET_TEST_PASS cases=3")
