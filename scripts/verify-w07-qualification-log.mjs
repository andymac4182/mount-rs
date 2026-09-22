#!/usr/bin/env node

// Validate the terminal markers emitted by the dedicated W07 qualification
// lane. This is an evidence-integrity check, not a production acceptance
// check: it never opens a provider and it never treats a fixture as live proof.

import { readFile, writeFile } from "node:fs/promises";

const [logPath, expectedRoundsRaw = "5", summaryPath] = process.argv.slice(2);

if (!logPath || !/^\d+$/u.test(expectedRoundsRaw)) {
  console.error(
    "usage: verify-w07-qualification-log.mjs <log> [expected-soak-rounds] [summary-json]",
  );
  process.exit(2);
}

const expectedRounds = Number(expectedRoundsRaw);
if (!Number.isSafeInteger(expectedRounds) || expectedRounds <= 0) {
  console.error("W07_PRODUCTION_QUALIFICATION_FAIL reason=invalid-expected-rounds");
  process.exit(2);
}

let log;
try {
  log = await readFile(logPath, "utf8");
} catch {
  console.error("W07_PRODUCTION_QUALIFICATION_FAIL reason=log-unreadable");
  process.exit(1);
}

const lines = log.split(/\r?\n/u);
const findLine = (pattern) => lines.find((line) => pattern.test(line));
const marker = (line, name) => line.slice(line.indexOf(name)).trim();
const missing = [];

const requiredMarkers = [
  [
    "config-policy-pass",
    "W07_PRODUCTION_CONFIG_POLICY_PASS",
    /W07_PRODUCTION_CONFIG_POLICY_PASS config_shape=splitstore metadata=foundationdb durable_metadata=true durable_blocks=true lease_authority=shared-provider lease_ttl=explicit-bounded blocks_tls=https secrets=external\b/u,
  ],
  [
    "config-policy-negative",
    "W07_PRODUCTION_CONFIG_POLICY_FAIL",
    /W07_PRODUCTION_CONFIG_POLICY_FAIL reason=config\.driver\.storage\.blocks\.secret_access_key-must-not-be-inline\b/u,
  ],
  [
    "config-policy-negative-lease-ttl",
    "W07_PRODUCTION_CONFIG_POLICY_FAIL",
    /W07_PRODUCTION_CONFIG_POLICY_FAIL reason=storage\.lease_ttl_ms-must-be-positive-safe-integer-at-most-24h\b/u,
  ],
  [
    "lease-publication-policy",
    "FOUNDATIONDB_LEASE_PUBLICATION_POLICY_PASS",
    /FOUNDATIONDB_LEASE_PUBLICATION_POLICY_PASS lease_ttl_ms=\d+ publication_interval_ms=\d+ max_forward_jump_ms=\d+\b/u,
  ],
  [
    "authority-heartbeat",
    "FOUNDATIONDB_AUTHORITY_HEARTBEAT_RUNNING",
    /FOUNDATIONDB_AUTHORITY_HEARTBEAT_RUNNING\b.*\binterval_seconds=\d+\s+max_forward_jump_seconds=\d+\s+duration_seconds=\d+\b/u,
  ],
  [
    "authority-stats",
    "FOUNDATIONDB_AUTHORITY_STATS_PASS",
    /FOUNDATIONDB_AUTHORITY_STATS_PASS\b publication_attempts=\d+ publication_successes=\d+ publication_failures=\d+ reader_attempts=\d+ reader_successes=\d+ reader_failures=\d+ last_published_time_ms=\d+ last_observed_time_ms=\d+\b/u,
  ],
  [
    "rustfs-network",
    "FOUNDATIONDB_RUSTFS_NETWORK_READY",
    /FOUNDATIONDB_RUSTFS_NETWORK_READY\b/u,
  ],
  [
    "block-endpoint",
    "FOUNDATIONDB_BLOCK_ENDPOINT_REACHABLE",
    /FOUNDATIONDB_BLOCK_ENDPOINT_REACHABLE status=403\b/u,
  ],
  [
    "chunked-composition",
    "FOUNDATIONDB_RUSTFS_CHUNKED_PASS",
    /FOUNDATIONDB_RUSTFS_CHUNKED_PASS\b/u,
  ],
  ["napi", "FOUNDATIONDB_NAPI_PASS", /FOUNDATIONDB_NAPI_PASS\b/u],
  [
    "service-restart-ready",
    "FOUNDATIONDB_SERVICE_RESTART_READY",
    /FOUNDATIONDB_SERVICE_RESTART_READY\b/u,
  ],
  [
    "rustfs-service-restart",
    "FOUNDATIONDB_RUSTFS_SERVICE_RESTART_PASS",
    /FOUNDATIONDB_RUSTFS_SERVICE_RESTART_PASS\b/u,
  ],
  [
    "cli",
    "FOUNDATIONDB_CLI_PASS",
    /FOUNDATIONDB_CLI_PASS mode=foundationdb-rustfs-fuse\b/u,
  ],
  [
    "rustfs-combination",
    "RUSTFS_COMBO_PASS",
    /RUSTFS_COMBO_PASS\b/u,
  ],
  [
    "rustfs-integration",
    "RUSTFS_INTEGRATION_PASS",
    /RUSTFS_INTEGRATION_PASS\b/u,
  ],
  [
    "foundationdb-workload-profile",
    "FOUNDATIONDB_W07_WORKLOAD_PASS",
    /FOUNDATIONDB_W07_WORKLOAD_PASS profile=w07-bounded provider=mount-rs-split-foundationdb-r2 size_mib=1 payload_bytes=4096 iterations=400 concurrency=64 minimum_iops=1 measured_iops=\d+(?:\.\d+)?\b/u,
  ],
];

const found = Object.fromEntries(
  requiredMarkers.map(([name, token, pattern]) => {
    const line = findLine(pattern);
    if (!line) missing.push(name);
    return [name, line ? marker(line, token) : null];
  }),
);

const leasePolicyPattern =
  /FOUNDATIONDB_LEASE_PUBLICATION_POLICY_PASS lease_ttl_ms=(\d+) publication_interval_ms=(\d+) max_forward_jump_ms=(\d+)\b/u;
const leasePolicyLine = findLine(leasePolicyPattern);
const leasePolicyMatch = leasePolicyLine?.match(leasePolicyPattern);
const leasePolicy = leasePolicyMatch
  ? {
      marker: marker(leasePolicyLine, "FOUNDATIONDB_LEASE_PUBLICATION_POLICY_PASS"),
      leaseTtlMs: Number(leasePolicyMatch[1]),
      publicationIntervalMs: Number(leasePolicyMatch[2]),
      maxForwardJumpMs: Number(leasePolicyMatch[3]),
    }
  : null;

const heartbeatPattern =
  /FOUNDATIONDB_AUTHORITY_HEARTBEAT_RUNNING\b.*\binterval_seconds=(\d+)\s+max_forward_jump_seconds=(\d+)\s+duration_seconds=(\d+)\b/u;
const heartbeatLine = findLine(heartbeatPattern);
const heartbeatMatch = heartbeatLine?.match(heartbeatPattern);
if (!heartbeatMatch) {
  missing.push("authority-heartbeat-values");
}

const authorityHeartbeat = heartbeatMatch
  ? {
      marker: marker(
        heartbeatLine,
        "FOUNDATIONDB_AUTHORITY_HEARTBEAT_RUNNING",
      ),
      intervalSeconds: Number(heartbeatMatch[1]),
      maxForwardJumpSeconds: Number(heartbeatMatch[2]),
      durationSeconds: Number(heartbeatMatch[3]),
    }
  : null;

if (
  authorityHeartbeat &&
  (!Number.isSafeInteger(authorityHeartbeat.intervalSeconds) ||
    !Number.isSafeInteger(authorityHeartbeat.maxForwardJumpSeconds) ||
    !Number.isSafeInteger(authorityHeartbeat.durationSeconds) ||
    authorityHeartbeat.intervalSeconds <= 0 ||
    authorityHeartbeat.maxForwardJumpSeconds <= 0 ||
    authorityHeartbeat.durationSeconds < authorityHeartbeat.intervalSeconds)
) {
  missing.push("authority-heartbeat-values");
}

const authorityStatsPattern =
  /FOUNDATIONDB_AUTHORITY_STATS_PASS\b publication_attempts=(\d+) publication_successes=(\d+) publication_failures=(\d+) reader_attempts=(\d+) reader_successes=(\d+) reader_failures=(\d+) last_published_time_ms=(\d+) last_observed_time_ms=(\d+)\b/u;
const authorityStatsLine = findLine(authorityStatsPattern);
const authorityStatsMatch = authorityStatsLine?.match(authorityStatsPattern);
if (!authorityStatsMatch) {
  missing.push("authority-stats-values");
}

const authorityStats = authorityStatsMatch
  ? {
      marker: marker(authorityStatsLine, "FOUNDATIONDB_AUTHORITY_STATS_PASS"),
      publicationAttempts: Number(authorityStatsMatch[1]),
      publicationSuccesses: Number(authorityStatsMatch[2]),
      publicationFailures: Number(authorityStatsMatch[3]),
      readerAttempts: Number(authorityStatsMatch[4]),
      readerSuccesses: Number(authorityStatsMatch[5]),
      readerFailures: Number(authorityStatsMatch[6]),
      lastPublishedTimeMs: Number(authorityStatsMatch[7]),
      lastObservedTimeMs: Number(authorityStatsMatch[8]),
    }
  : null;

if (
  authorityStats &&
  (!Object.values(authorityStats)
    .slice(1)
    .every((value) => Number.isSafeInteger(value) && value >= 0) ||
    authorityStats.publicationAttempts <= 0 ||
    authorityStats.publicationSuccesses <= 0 ||
    authorityStats.readerAttempts <= 0 ||
    authorityStats.readerSuccesses <= 0 ||
    authorityStats.publicationAttempts !==
      authorityStats.publicationSuccesses + authorityStats.publicationFailures ||
    authorityStats.readerAttempts !==
      authorityStats.readerSuccesses + authorityStats.readerFailures ||
    authorityStats.lastPublishedTimeMs <= 0 ||
    authorityStats.lastObservedTimeMs <= 0)
) {
  missing.push("authority-stats-values");
}

if (
  leasePolicy &&
  (!Number.isSafeInteger(leasePolicy.leaseTtlMs) ||
    !Number.isSafeInteger(leasePolicy.publicationIntervalMs) ||
    !Number.isSafeInteger(leasePolicy.maxForwardJumpMs) ||
    leasePolicy.leaseTtlMs <= 0 ||
    leasePolicy.leaseTtlMs > 24 * 60 * 60 * 1000 ||
    leasePolicy.publicationIntervalMs <= 0 ||
    leasePolicy.publicationIntervalMs >= leasePolicy.leaseTtlMs ||
    leasePolicy.maxForwardJumpMs <= 0 ||
    leasePolicy.maxForwardJumpMs > leasePolicy.leaseTtlMs)
) {
  missing.push("lease-policy-values");
}

if (
  leasePolicy &&
  authorityHeartbeat &&
  (authorityHeartbeat.intervalSeconds * 1000 !==
    leasePolicy.publicationIntervalMs ||
    authorityHeartbeat.maxForwardJumpSeconds * 1000 !==
      leasePolicy.maxForwardJumpMs ||
    authorityHeartbeat.intervalSeconds * 1000 >= leasePolicy.leaseTtlMs)
) {
  missing.push("authority-heartbeat-policy-values");
}

const soakPattern = new RegExp(
  `FOUNDATIONDB_SOAK_PASS rounds=${expectedRounds}\\b`,
  "u",
);
const soakLine = findLine(soakPattern);
if (!soakLine) missing.push(`soak-rounds-${expectedRounds}`);
found.soak = soakLine ? marker(soakLine, "FOUNDATIONDB_SOAK_PASS") : null;

const testPattern = new RegExp(
  `FOUNDATIONDB_TEST_PASS topology=durable.*platform=linux/amd64.*service_restart=pass soak_rounds=${expectedRounds}\\b`,
  "u",
);
const testLine = findLine(testPattern);
if (!testLine) missing.push("durable-test-summary");
found.test = testLine ? marker(testLine, "FOUNDATIONDB_TEST_PASS") : null;

const latencyPattern =
  /FOUNDATIONDB_LATENCY_PASS workload=(\S+) operations=(\d+) p50_us=(\d+) p95_us=(\d+) p99_us=(\d+) total_ms=(\d+) throughput_ops_per_sec=([0-9]+(?:\.[0-9]+)?)/u;
const parseLatency = (line, match) => ({
  marker: marker(line, "FOUNDATIONDB_LATENCY_PASS"),
  workload: match[1],
  operations: Number(match[2]),
  p50Us: Number(match[3]),
  p95Us: Number(match[4]),
  p99Us: Number(match[5]),
  totalMs: Number(match[6]),
  throughputOpsPerSec: Number(match[7]),
});
const latencySamples = lines.flatMap((line) => {
  const match = line.match(latencyPattern);
  return match ? [parseLatency(line, match)] : [];
});
if (latencySamples.length === 0) {
  missing.push("latency");
}
if (latencySamples.length < expectedRounds + 1) {
  missing.push(`latency-samples-${expectedRounds + 1}`);
}

const latency = latencySamples[0] ?? null;

const provenanceFields = [
  [
    "repository",
    process.env.W07_QUALIFICATION_REPOSITORY,
    /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/u,
  ],
  ["workflow", process.env.W07_QUALIFICATION_WORKFLOW, /\S/u],
  ["ref", process.env.W07_QUALIFICATION_REF, /\S/u],
  [
    "sourceRevision",
    process.env.W07_QUALIFICATION_SOURCE_REVISION,
    /^[0-9a-f]{40}$/iu,
  ],
  ["runId", process.env.W07_QUALIFICATION_RUN_ID, /^[1-9]\d*$/u],
  ["runAttempt", process.env.W07_QUALIFICATION_RUN_ATTEMPT, /^[1-9]\d*$/u],
  ["runner", process.env.W07_QUALIFICATION_RUNNER, /\S/u],
];
const provenanceRequested =
  process.env.W07_REQUIRE_PROVENANCE === "1" ||
  provenanceFields.some(([, value]) => value !== undefined);
let provenance = null;
if (provenanceRequested) {
  const missingProvenance = provenanceFields
    .filter(([, value]) => value === undefined || value.length === 0)
    .map(([name]) => name);
  const invalidProvenance = provenanceFields
    .filter(
      ([, value, pattern]) =>
        value !== undefined && value.length > 0 && !pattern.test(value),
    )
    .map(([name]) => name);
  if (missingProvenance.length > 0) {
    missing.push(`provenance-missing-${missingProvenance.join("+")}`);
  }
  if (invalidProvenance.length > 0) {
    missing.push(`provenance-invalid-${invalidProvenance.join("+")}`);
  }
  if (missingProvenance.length === 0 && invalidProvenance.length === 0) {
    provenance = Object.fromEntries(
      provenanceFields.map(([name, value]) => [name, value]),
    );
  }
}

if (
  latencySamples.some(
    (sample) =>
      !Number.isSafeInteger(sample.operations) ||
      !Number.isSafeInteger(sample.p50Us) ||
      !Number.isSafeInteger(sample.p95Us) ||
      !Number.isSafeInteger(sample.p99Us) ||
      !Number.isSafeInteger(sample.totalMs) ||
      sample.operations <= 0 ||
      sample.p50Us < 0 ||
      sample.p50Us > sample.p95Us ||
      sample.p95Us > sample.p99Us ||
      sample.totalMs <= 0 ||
      sample.throughputOpsPerSec <= 0,
  )
) {
  missing.push("latency-values");
}

if (
  lines.some((line) =>
    /(?:FOUNDATIONDB_SOAK_FAIL|RUSTFS_COMBO_FAIL|FOUNDATIONDB_CLI_FAIL)\b/u.test(
      line,
    ),
  )
) {
  missing.push("unexpected-failure-marker");
}

if (missing.length > 0) {
  console.error(
    `W07_PRODUCTION_QUALIFICATION_FAIL missing=${missing.join(",")}`,
  );
  process.exit(1);
}

const summary = {
  schema: 2,
  result: "qualification-pass",
  expectedSoakRounds: expectedRounds,
  markers: found,
  leasePolicy,
  authorityHeartbeat,
  authorityStats,
  latency,
  latencySamples,
  provenance,
};

if (summaryPath) {
  await writeFile(summaryPath, `${JSON.stringify(summary, null, 2)}\n`);
}

if (provenance) {
  console.log(
    "W07_PRODUCTION_QUALIFICATION_PROVENANCE_PASS " +
      `repository=${provenance.repository} workflow=${provenance.workflow} ` +
      `ref=${provenance.ref} source_revision=${provenance.sourceRevision} ` +
      `run_id=${provenance.runId} run_attempt=${provenance.runAttempt} ` +
      `runner=${provenance.runner}`,
  );
}
console.log(
  `W07_PRODUCTION_QUALIFICATION_EVIDENCE_PASS rounds=${expectedRounds} ` +
    `workload=${latency.workload} operations=${latency.operations}`,
);
