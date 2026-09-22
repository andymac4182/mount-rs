#!/usr/bin/env node

// Credential-free regression cases for the W07 workload-artifact guard.

import assert from "node:assert/strict";
import { validateW07WorkloadArtifact, W07_WORKLOAD_PROFILE } from "./verify-w07-workload-artifact.mjs";

function fixture(overrides = {}) {
  const summary = {
    writeMs: { median: 1, p95: 2, p99: 3 },
    readMs: { median: 1, p95: 2, p99: 3 },
    throughputMbps: { median: 1, p95: 2, p99: 3 },
    deleteMs: { median: 1, p95: 2, p99: 3 },
    successRate: 1,
    elapsedMs: 1_000,
    operationsPerLifecycle: 3,
    successfulIterations: W07_WORKLOAD_PROFILE.iterations,
    failedIterations: 0,
    successfulOperations: W07_WORKLOAD_PROFILE.iterations * 3,
    attemptedOperations: W07_WORKLOAD_PROFILE.iterations * 3,
    iops: 32,
    iopsTarget: W07_WORKLOAD_PROFILE.minimumIops,
    iopsTargetMet: true,
    timeoutCount: 0,
    cleanupFailureCount: 0,
    operationSuccess: {
      write: W07_WORKLOAD_PROFILE.iterations,
      read: W07_WORKLOAD_PROFILE.iterations,
      delete: W07_WORKLOAD_PROFILE.iterations,
      verifiedReads: W07_WORKLOAD_PROFILE.iterations,
    },
    statSampleCounts: {
      writeMs: W07_WORKLOAD_PROFILE.iterations,
      readMs: W07_WORKLOAD_PROFILE.iterations,
      throughputMbps: W07_WORKLOAD_PROFILE.iterations,
      deleteMs: W07_WORKLOAD_PROFILE.iterations,
    },
  };
  const size = {
    status: "ok",
    sizeMiB: W07_WORKLOAD_PROFILE.sizeMiB,
    iterationsRequested: W07_WORKLOAD_PROFILE.iterations,
    concurrency: W07_WORKLOAD_PROFILE.concurrency,
    summary,
  };
  return {
    schemaVersion: "mount-rs.storage-benchmark.v1",
    status: "ok",
    config: {
      sizesMiB: [W07_WORKLOAD_PROFILE.sizeMiB],
      payloadSizesBytes: [W07_WORKLOAD_PROFILE.payloadBytes],
      payloadBytes: W07_WORKLOAD_PROFILE.payloadBytes,
      iterations: W07_WORKLOAD_PROFILE.iterations,
      concurrency: W07_WORKLOAD_PROFILE.concurrency,
      minIops: W07_WORKLOAD_PROFILE.minimumIops,
      requireConfigured: true,
    },
    counts: {
      providersRequested: 1,
      providersFailed: 0,
      providersSkipped: 0,
      configurationFailures: 0,
      sizeResultsFailed: 0,
      sizeResultsSkipped: 0,
    },
    configurationFailures: [],
    providers: [
      {
        provider: W07_WORKLOAD_PROFILE.provider,
        status: "ok",
        cleanup: { remainingPaths: 0, failures: [], resource: { status: "ok" } },
        sizes: [size],
      },
    ],
    ...overrides,
  };
}

const cases = [
  {
    name: "valid-profile",
    run() {
      const result = validateW07WorkloadArtifact(fixture());
      assert.deepEqual(result.profile, W07_WORKLOAD_PROFILE);
      assert.deepEqual(result.measuredIops, [32]);
    },
  },
  {
    name: "wrong-concurrency",
    run() {
      const artifact = fixture();
      artifact.providers[0].sizes[0].concurrency = 16;
      assert.throws(
        () => validateW07WorkloadArtifact(artifact),
        /mount-rs-split-foundationdb-r2\.concurrency-must-equal-64/,
      );
    },
  },
  {
    name: "zero-completed-rate",
    run() {
      const artifact = fixture();
      artifact.providers[0].sizes[0].summary.iops = 0;
      artifact.providers[0].sizes[0].summary.iopsTargetMet = false;
      assert.throws(
        () => validateW07WorkloadArtifact(artifact),
        /iops-below-1/,
      );
    },
  },
  {
    name: "wrong-provider",
    run() {
      const artifact = fixture();
      artifact.providers[0].provider = "mount-rs-split-sqlite-r2";
      assert.throws(
        () => validateW07WorkloadArtifact(artifact),
        /providers-do-not-match-requested-provider-set/,
      );
    },
  },
];

for (const testCase of cases) testCase.run();
console.log(`W07_WORKLOAD_ARTIFACT_TEST_PASS cases=${cases.length}`);
