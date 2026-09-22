#!/usr/bin/env node

// Credential-free P06 capacity/load/soak policy. This validates workload,
// measurement and acceptance-contract shape only; it never contacts TiDB,
// RustFS, a metrics collector or a production environment.

import { lstat, readFile } from "node:fs/promises";

const configPath = process.argv[2];
const REQUIRED_PROFILES = ["baseline", "peak", "saturation", "failover", "soak"];
const REQUIRED_OPERATIONS = ["write", "read", "truncate", "reopen"];
const REQUIRED_RESOURCE_METRICS = ["cpu", "memory", "disk", "network", "iops"];
const REQUIRED_FAILOVER_SCENARIOS = [
  "sql-frontend-restart",
  "tikv-restart",
  "pd-restart",
  "rustfs-reopen",
];

function fail(reason) {
  console.error(`W08_PRODUCTION_CAPACITY_POLICY_FAIL reason=${reason}`);
  process.exit(1);
}

function isObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function requireObject(value, path) {
  if (!isObject(value)) fail(`${path}-must-be-object`);
  return value;
}

function rejectUnknown(object, allowed, path) {
  for (const key of Object.keys(object)) {
    if (!allowed.includes(key)) fail(`${path}.${key}-is-unknown`);
  }
}

function requireString(value, path) {
  if (typeof value !== "string" || value.length === 0) {
    fail(`${path}-must-be-non-empty-string`);
  }
  if (/[<>]/u.test(value) || /(?:password|secret|token|private[_-]?key|credential)/iu.test(value)) {
    fail(`${path}-contains-placeholder-or-secret`);
  }
  return value;
}

function requireBoolean(value, path, expected) {
  if (value !== expected) fail(`${path}-must-be-${expected}`);
}

function requireSafeInteger(value, path, minimum) {
  if (!Number.isSafeInteger(value) || value < minimum) {
    fail(`${path}-must-be-safe-integer-at-least-${minimum}`);
  }
  return value;
}

function requireFiniteNumber(value, path, minimum) {
  if (typeof value !== "number" || !Number.isFinite(value) || value < minimum) {
    fail(`${path}-must-be-number-at-least-${minimum}`);
  }
  return value;
}

function requireEnum(value, path, allowed) {
  if (!allowed.includes(value)) {
    fail(`${path}-must-be-one-of-${allowed.join("-")}`);
  }
  return value;
}

function requireExactStringArray(value, path, expected) {
  if (!Array.isArray(value) || value.length !== expected.length) {
    fail(`${path}-must-match-required-set`);
  }
  value.forEach((item, index) => requireString(item, `${path}[${index}]`));
  if (new Set(value).size !== value.length || expected.some((item) => !value.includes(item))) {
    fail(`${path}-must-match-required-set`);
  }
  return value;
}

function rejectInlineSecrets(value, path = "config") {
  if (Array.isArray(value)) {
    value.forEach((item, index) => rejectInlineSecrets(item, `${path}[${index}]`));
    return;
  }
  if (!isObject(value)) return;

  for (const [key, child] of Object.entries(value)) {
    if (/(?:password|secret|token|private[_-]?key|credential)/iu.test(key)) {
      if (typeof child === "string") fail(`${path}.${key}-must-not-be-inline`);
    }
    rejectInlineSecrets(child, `${path}.${key}`);
  }
}

if (!configPath || process.argv.length !== 3) {
  console.error("usage: verify-w08-production-capacity.mjs <capacity-policy.json>");
  process.exit(2);
}

let config;
try {
  const metadata = await lstat(configPath);
  if (!metadata.isFile() || metadata.isSymbolicLink()) {
    fail("config-must-be-regular-file");
  }
  config = JSON.parse(await readFile(configPath, "utf8"));
} catch (error) {
  if (error?.message?.startsWith("W08_PRODUCTION_CAPACITY_POLICY_FAIL")) {
    throw error;
  }
  fail("config-unreadable-or-invalid-json");
}

rejectInlineSecrets(config);
const root = requireObject(config, "config");
rejectUnknown(
  root,
  ["version", "environment_class", "workload", "thresholds", "resources", "failover", "soak", "ownership"],
  "config",
);
if (root.version !== 1) fail("config.version-must-be-1");
requireEnum(
  root.environment_class,
  "environment_class",
  ["production-like-staging", "production"],
);

const workload = requireObject(root.workload, "config.workload");
rejectUnknown(
  workload,
  [
    "profiles",
    "operations",
    "payload_bytes",
    "baseline_concurrency",
    "peak_concurrency",
    "saturation_concurrency",
    "duration_minutes",
    "repetitions",
  ],
  "config.workload",
);
requireExactStringArray(workload.profiles, "workload.profiles", REQUIRED_PROFILES);
requireExactStringArray(workload.operations, "workload.operations", REQUIRED_OPERATIONS);
requireSafeInteger(workload.payload_bytes, "workload.payload_bytes", 4096);
const baselineConcurrency = requireSafeInteger(
  workload.baseline_concurrency,
  "workload.baseline_concurrency",
  1,
);
const peakConcurrency = requireSafeInteger(
  workload.peak_concurrency,
  "workload.peak_concurrency",
  baselineConcurrency,
);
const saturationConcurrency = requireSafeInteger(
  workload.saturation_concurrency,
  "workload.saturation_concurrency",
  peakConcurrency + 1,
);
requireSafeInteger(workload.duration_minutes, "workload.duration_minutes", 60);
requireSafeInteger(workload.repetitions, "workload.repetitions", 3);

const thresholds = requireObject(root.thresholds, "config.thresholds");
rejectUnknown(
  thresholds,
  [
    "p95_latency_ms",
    "p99_latency_ms",
    "max_error_rate_percent",
    "min_throughput_ops_per_second",
    "max_resource_utilization_percent",
    "min_headroom_percent",
    "max_growth_percent",
    "max_cost_per_hour",
  ],
  "config.thresholds",
);
const p95Latency = requireSafeInteger(thresholds.p95_latency_ms, "thresholds.p95_latency_ms", 1);
if (p95Latency > 1000) fail("thresholds.p95_latency_ms-must-be-at-most-1000");
const p99Latency = requireSafeInteger(thresholds.p99_latency_ms, "thresholds.p99_latency_ms", p95Latency);
if (p99Latency > 2000) fail("thresholds.p99_latency_ms-must-be-at-most-2000");
if (p99Latency < p95Latency) fail("thresholds.p99_latency_ms-must-be-at-least-p95");
const maxErrorRate = requireFiniteNumber(
  thresholds.max_error_rate_percent,
  "thresholds.max_error_rate_percent",
  0,
);
if (maxErrorRate > 0.1) fail("thresholds.max_error_rate_percent-must-be-at-most-0.1");
requireFiniteNumber(thresholds.min_throughput_ops_per_second, "thresholds.min_throughput_ops_per_second", 1);
const maxResourceUtilization = requireFiniteNumber(
  thresholds.max_resource_utilization_percent,
  "thresholds.max_resource_utilization_percent",
  1,
);
if (maxResourceUtilization > 75) fail("thresholds.max_resource_utilization_percent-must-be-at-most-75");
const minHeadroom = requireFiniteNumber(thresholds.min_headroom_percent, "thresholds.min_headroom_percent", 25);
if (minHeadroom < 25 || minHeadroom > 100) fail("thresholds.min_headroom_percent-must-be-between-25-and-100");
const maxGrowth = requireFiniteNumber(thresholds.max_growth_percent, "thresholds.max_growth_percent", 0);
if (maxGrowth > 10) fail("thresholds.max_growth_percent-must-be-at-most-10");
requireFiniteNumber(thresholds.max_cost_per_hour, "thresholds.max_cost_per_hour", 1);

const resources = requireObject(root.resources, "config.resources");
rejectUnknown(
  resources,
  ["collector", "metrics", "interval_seconds", "retention_days", "headroom_required"],
  "config.resources",
);
requireEnum(resources.collector, "resources.collector", ["external-managed"]);
requireExactStringArray(resources.metrics, "resources.metrics", REQUIRED_RESOURCE_METRICS);
requireSafeInteger(resources.interval_seconds, "resources.interval_seconds", 1);
if (resources.interval_seconds > 60) fail("resources.interval_seconds-must-be-at-most-60");
requireSafeInteger(resources.retention_days, "resources.retention_days", 30);
requireBoolean(resources.headroom_required, "resources.headroom_required", true);

const failover = requireObject(root.failover, "config.failover");
rejectUnknown(
  failover,
  ["required", "scenarios", "recovery_slo_seconds", "fresh_client_readback_required", "integrity_check_required"],
  "config.failover",
);
requireBoolean(failover.required, "failover.required", true);
requireExactStringArray(failover.scenarios, "failover.scenarios", REQUIRED_FAILOVER_SCENARIOS);
const recoverySloSeconds = requireSafeInteger(
  failover.recovery_slo_seconds,
  "failover.recovery_slo_seconds",
  1,
);
if (recoverySloSeconds > 300) fail("failover.recovery_slo_seconds-must-be-at-most-300");
requireBoolean(failover.fresh_client_readback_required, "failover.fresh_client_readback_required", true);
requireBoolean(failover.integrity_check_required, "failover.integrity_check_required", true);

const soak = requireObject(root.soak, "config.soak");
rejectUnknown(
  soak,
  ["duration_hours", "reopen_cycles", "resource_growth_limit_percent", "cleanup_required"],
  "config.soak",
);
const soakHours = requireSafeInteger(soak.duration_hours, "soak.duration_hours", 4);
requireSafeInteger(soak.reopen_cycles, "soak.reopen_cycles", 2);
const resourceGrowthLimit = requireFiniteNumber(
  soak.resource_growth_limit_percent,
  "soak.resource_growth_limit_percent",
  0,
);
if (resourceGrowthLimit > 10) fail("soak.resource_growth_limit_percent-must-be-at-most-10");
requireBoolean(soak.cleanup_required, "soak.cleanup_required", true);

const ownership = requireObject(root.ownership, "config.ownership");
rejectUnknown(
  ownership,
  ["workload_owner", "performance_owner", "cost_owner", "operations_owner"],
  "config.ownership",
);
for (const [key, value] of Object.entries(ownership)) {
  requireString(value, `ownership.${key}`);
}

console.log(
  "W08_PRODUCTION_CAPACITY_POLICY_PASS " +
    `profiles=${REQUIRED_PROFILES.length} peak_concurrency=${peakConcurrency} ` +
    `saturation_concurrency=${saturationConcurrency} soak_hours=${soakHours} ` +
    `p95_ms=${p95Latency} p99_ms=${p99Latency} error_rate=${maxErrorRate} ` +
    `headroom=${minHeadroom} recovery_seconds=${recoverySloSeconds}`,
);
