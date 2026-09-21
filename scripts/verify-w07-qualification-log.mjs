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
    /W07_PRODUCTION_CONFIG_POLICY_PASS\b/u,
  ],
  [
    "config-policy-negative",
    "W07_PRODUCTION_CONFIG_POLICY_FAIL",
    /W07_PRODUCTION_CONFIG_POLICY_FAIL reason=config\.driver\.storage\.blocks\.secret_access_key-must-not-be-inline\b/u,
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
    "rustfs-combination",
    "RUSTFS_COMBO_PASS",
    /RUSTFS_COMBO_PASS\b/u,
  ],
  [
    "rustfs-integration",
    "RUSTFS_INTEGRATION_PASS",
    /RUSTFS_INTEGRATION_PASS\b/u,
  ],
];

const found = Object.fromEntries(
  requiredMarkers.map(([name, token, pattern]) => {
    const line = findLine(pattern);
    if (!line) missing.push(name);
    return [name, line ? marker(line, token) : null];
  }),
);

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
const latencyLine = findLine(latencyPattern);
const latencyMatch = latencyLine?.match(latencyPattern);
if (!latencyMatch) {
  missing.push("latency");
}

const latency = latencyMatch
  ? {
      marker: marker(latencyLine, "FOUNDATIONDB_LATENCY_PASS"),
      workload: latencyMatch[1],
      operations: Number(latencyMatch[2]),
      p50Us: Number(latencyMatch[3]),
      p95Us: Number(latencyMatch[4]),
      p99Us: Number(latencyMatch[5]),
      totalMs: Number(latencyMatch[6]),
      throughputOpsPerSec: Number(latencyMatch[7]),
    }
  : null;

if (
  latency &&
  (latency.operations <= 0 ||
    latency.p50Us > latency.p95Us ||
    latency.p95Us > latency.p99Us ||
    latency.totalMs <= 0 ||
    latency.throughputOpsPerSec <= 0)
) {
  missing.push("latency-values");
}

if (missing.length > 0) {
  console.error(
    `W07_PRODUCTION_QUALIFICATION_FAIL missing=${missing.join(",")}`,
  );
  process.exit(1);
}

const summary = {
  schema: 1,
  result: "qualification-pass",
  expectedSoakRounds: expectedRounds,
  markers: found,
  latency,
};

if (summaryPath) {
  await writeFile(summaryPath, `${JSON.stringify(summary, null, 2)}\n`);
}

console.log(
  `W07_PRODUCTION_QUALIFICATION_EVIDENCE_PASS rounds=${expectedRounds} ` +
    `workload=${latency.workload} operations=${latency.operations}`,
);
