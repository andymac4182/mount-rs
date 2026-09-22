#!/usr/bin/env node

// Bounded TiDB/RustFS workload evidence. This is intentionally opt-in and
// emits client-side latency/throughput only; resource headroom, cost and
// production SLO acceptance remain external gates.

import assert from "node:assert/strict";
import { performance } from "node:perf_hooks";
import { cleanupR2Prefix, listR2Prefix, rustfsConfigFromEnv } from "./r2-cleanup.mjs";

function safeRunId() {
  const configured = process.env.MOUNT_RS_PROVIDER_MATRIX_RUN_ID ?? "";
  return /^[A-Za-z0-9._-]+$/u.test(configured) ? configured : String(process.pid);
}
function boundedInteger(name, fallback, { min, max }) {
  const raw = process.env[name] ?? String(fallback);
  if (!/^\d+$/u.test(raw)) {
    console.error(`TIDB_RUSTFS_SOAK_FAIL reason=${name}-must-be-non-negative-integer`);
    process.exit(2);
  }
  const value = Number(raw);
  if (!Number.isSafeInteger(value) || value < min || value > max) {
    console.error(`TIDB_RUSTFS_SOAK_FAIL reason=${name}-outside-bounds`);
    process.exit(2);
  }
  return value;
}

function percentile(values, fraction) {
  const sorted = [...values].sort((left, right) => left - right);
  const position = (sorted.length - 1) * fraction;
  const lower = Math.floor(position);
  const upper = Math.ceil(position);
  if (lower === upper) return sorted[lower];
  return sorted[lower] + (sorted[upper] - sorted[lower]) * (position - lower);
}

const operations = boundedInteger("MOUNT_RS_TIDB_RUSTFS_SOAK_OPERATIONS", 0, {
  min: 0,
  max: 10_000,
});
if (operations === 0) {
  console.log("SKIP tidb-rustfs-soak gate=MOUNT_RS_TIDB_RUSTFS_SOAK_OPERATIONS");
  process.exit(0);
}

const concurrency = boundedInteger("MOUNT_RS_TIDB_RUSTFS_SOAK_CONCURRENCY", 8, {
  min: 1,
  max: 64,
});
const payloadBytes = boundedInteger("MOUNT_RS_TIDB_RUSTFS_SOAK_PAYLOAD_BYTES", 65_536, {
  min: 1,
  max: 4 * 1024 * 1024,
});
const tidbUrl = process.env.MOUNT_RS_TIDB_URL;
const rustfs = rustfsConfigFromEnv();
if (!tidbUrl) {
  console.error("TIDB_RUSTFS_SOAK_FAIL reason=MOUNT_RS_TIDB_URL-missing");
  process.exit(2);
}
if (rustfs.missing.length > 0) {
  console.error("TIDB_RUSTFS_SOAK_FAIL reason=rustfs-environment-missing");
  process.exit(2);
}

// Delay loading the native addon so an explicitly disabled soak remains a
// credential-free, mount-free skip on developer machines.
const { createChunkedDriver } = await import("../../bindings/mount-rs-napi/index.js");
const runId = safeRunId();
const prefix = `mount-rs-provider-matrix/${runId}/tidb-rustfs-soak`;
const volumeKey = `mount-rs-provider-matrix/${runId}/tidb-rustfs-soak-metadata`;
const protectedKeys = await listR2Prefix(rustfs.config, prefix);
const latencies = [];
const failures = [];
let filesystem;
let nextOperation = 0;

function payloadFor(operation) {
  const payload = Buffer.allocUnsafe(payloadBytes);
  for (let index = 0; index < payload.length; index += 1) {
    payload[index] = (operation + index) & 0xff;
  }
  return payload;
}

try {
  filesystem = await createChunkedDriver({
    metadata: {
      kind: "tidb",
      uri: tidbUrl,
      key: volumeKey,
      durable: true,
    },
    blocks: {
      kind: "r2",
      endpoint: rustfs.config.endpoint,
      bucket: rustfs.config.bucket,
      key: prefix,
      accessKeyId: rustfs.config.accessKeyId,
      secretAccessKey: rustfs.config.secretAccessKey,
      durable: true,
    },
    chunkSize: 65_536,
    owner: `w08-soak-${runId}`,
    ttlMs: 120_000,
  });
  await filesystem.mkdir("/w08-soak", { recursive: true, mode: 0o755 });

  const started = performance.now();
  async function worker() {
    while (true) {
      const operation = nextOperation;
      nextOperation += 1;
      if (operation >= operations) return;

      const path = `/w08-soak/operation-${operation}`;
      const payload = payloadFor(operation);
      const operationStarted = performance.now();
      try {
        await filesystem.writeFile(path, payload);
        const actual = Buffer.from(await filesystem.readFile(path));
        assert.deepEqual(actual, payload);
        latencies.push(performance.now() - operationStarted);
      } catch (error) {
        failures.push(error?.code ?? error?.name ?? "Error");
      }
    }
  }

  await Promise.all(Array.from({ length: concurrency }, () => worker()));
  const elapsedMs = performance.now() - started;
  if (failures.length > 0) {
    console.error(`TIDB_RUSTFS_SOAK_FAIL reason=operation-errors count=${failures.length}`);
    process.exitCode = 1;
  } else {
    const throughput = operations / (elapsedMs / 1_000);
    console.log(
      "TIDB_RUSTFS_SOAK_PASS " +
        `operations=${operations} concurrency=${concurrency} payload_bytes=${payloadBytes} ` +
        `elapsed_ms=${elapsedMs.toFixed(2)} p50_ms=${percentile(latencies, 0.5).toFixed(2)} ` +
        `p95_ms=${percentile(latencies, 0.95).toFixed(2)} ` +
        `p99_ms=${percentile(latencies, 0.99).toFixed(2)} ` +
        `throughput_ops_s=${throughput.toFixed(2)} errors=0`,
    );
  }
} finally {
  try {
    await filesystem?.shutdown();
  } finally {
    await cleanupR2Prefix(rustfs.config, prefix, protectedKeys);
  }
}
