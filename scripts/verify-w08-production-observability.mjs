#!/usr/bin/env node

// Credential-free P05 observability/SLO/alerting policy. This validates
// telemetry and alert-contract shape only; it never contacts a collector,
// pager, dashboard or production endpoint.

import { lstat, readFile } from "node:fs/promises";

const configPath = process.argv[2];
const MIN_RETENTION_DAYS = 30;
const MAX_ALERT_ACK_MINUTES = 15;
const REQUIRED_ALERT_ROUTES = ["primary-on-call", "secondary-on-call"];

function fail(reason) {
  console.error(`W08_PRODUCTION_OBSERVABILITY_POLICY_FAIL reason=${reason}`);
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
  if (/[<>]/u.test(value)) fail(`${path}-contains-placeholder`);
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

function requireStringArray(value, path) {
  if (!Array.isArray(value) || value.length === 0) {
    fail(`${path}-must-be-non-empty-array`);
  }
  value.forEach((item, index) => requireString(item, `${path}[${index}]`));
  return value;
}

function rejectInlineSecrets(value, path = "config") {
  if (Array.isArray(value)) {
    value.forEach((item, index) => rejectInlineSecrets(item, `${path}[${index}]`));
    return;
  }
  if (!isObject(value)) return;

  for (const [key, child] of Object.entries(value)) {
    if (/(?:password|secret|token|private[_-]?key|credential|value)/iu.test(key)) {
      if (typeof child === "string") fail(`${path}.${key}-must-not-be-inline`);
    }
    rejectInlineSecrets(child, `${path}.${key}`);
  }
}

if (!configPath || process.argv.length !== 3) {
  console.error(
    "usage: verify-w08-production-observability.mjs <observability-policy.json>",
  );
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
  if (error?.message?.startsWith("W08_PRODUCTION_OBSERVABILITY_POLICY_FAIL")) {
    throw error;
  }
  fail("config-unreadable-or-invalid-json");
}

rejectInlineSecrets(config);
const root = requireObject(config, "config");
rejectUnknown(
  root,
  ["version", "environment_class", "telemetry", "slo", "alerts", "health"],
  "config",
);
if (root.version !== 1) fail("config.version-must-be-1");
requireEnum(
  root.environment_class,
  "environment_class",
  ["production-like-staging", "production"],
);

const telemetry = requireObject(root.telemetry, "config.telemetry");
rejectUnknown(telemetry, ["metrics", "logs", "traces"], "config.telemetry");

const metrics = requireObject(telemetry.metrics, "config.telemetry.metrics");
rejectUnknown(
  metrics,
  ["enabled", "collector", "endpoint_class", "retention_days", "redaction_required"],
  "config.telemetry.metrics",
);
requireBoolean(metrics.enabled, "telemetry.metrics.enabled", true);
requireEnum(metrics.collector, "telemetry.metrics.collector", ["external-managed"]);
requireEnum(metrics.endpoint_class, "telemetry.metrics.endpoint_class", ["private"]);
const metricsRetentionDays = requireSafeInteger(
  metrics.retention_days,
  "telemetry.metrics.retention_days",
  MIN_RETENTION_DAYS,
);
requireBoolean(metrics.redaction_required, "telemetry.metrics.redaction_required", true);

const logs = requireObject(telemetry.logs, "config.telemetry.logs");
rejectUnknown(
  logs,
  ["enabled", "structured", "retention_days", "redaction_required"],
  "config.telemetry.logs",
);
requireBoolean(logs.enabled, "telemetry.logs.enabled", true);
requireBoolean(logs.structured, "telemetry.logs.structured", true);
const logsRetentionDays = requireSafeInteger(
  logs.retention_days,
  "telemetry.logs.retention_days",
  MIN_RETENTION_DAYS,
);
requireBoolean(logs.redaction_required, "telemetry.logs.redaction_required", true);

const traces = requireObject(telemetry.traces, "config.telemetry.traces");
rejectUnknown(
  traces,
  ["enabled", "sampling", "retention_days", "redaction_required"],
  "config.telemetry.traces",
);
requireBoolean(traces.enabled, "telemetry.traces.enabled", true);
requireEnum(traces.sampling, "telemetry.traces.sampling", ["tail-based"]);
requireSafeInteger(traces.retention_days, "telemetry.traces.retention_days", 7);
requireBoolean(traces.redaction_required, "telemetry.traces.redaction_required", true);

const slo = requireObject(root.slo, "config.slo");
rejectUnknown(
  slo,
  ["availability_percent", "write_error_rate_percent", "read_p95_latency_ms", "error_budget_window_days"],
  "config.slo",
);
const availability = requireFiniteNumber(slo.availability_percent, "slo.availability_percent", 99.9);
if (availability > 100) fail("slo.availability_percent-must-be-at-most-100");
const writeErrorRate = requireFiniteNumber(
  slo.write_error_rate_percent,
  "slo.write_error_rate_percent",
  0,
);
if (writeErrorRate > 0.1) fail("slo.write_error_rate_percent-must-be-at-most-0.1");
requireSafeInteger(slo.read_p95_latency_ms, "slo.read_p95_latency_ms", 1);
requireSafeInteger(slo.error_budget_window_days, "slo.error_budget_window_days", 30);

const alerts = requireObject(root.alerts, "config.alerts");
rejectUnknown(
  alerts,
  ["routes", "paging_enabled", "test_alert_required", "acknowledgement_minutes", "runbook_ref", "no_secrets_in_payloads"],
  "config.alerts",
);
const routes = requireStringArray(alerts.routes, "alerts.routes");
if (new Set(routes).size !== routes.length) fail("alerts.routes-must-be-unique");
for (const route of REQUIRED_ALERT_ROUTES) {
  if (!routes.includes(route)) fail(`alerts.routes-must-contain-${route}`);
}
if (routes.length !== REQUIRED_ALERT_ROUTES.length) {
  fail("alerts.routes-must-contain-only-required-routes");
}
requireBoolean(alerts.paging_enabled, "alerts.paging_enabled", true);
requireBoolean(alerts.test_alert_required, "alerts.test_alert_required", true);
const acknowledgementMinutes = requireSafeInteger(
  alerts.acknowledgement_minutes,
  "alerts.acknowledgement_minutes",
  1,
);
if (acknowledgementMinutes > MAX_ALERT_ACK_MINUTES) {
  fail(`alerts.acknowledgement_minutes-must-be-at-most-${MAX_ALERT_ACK_MINUTES}`);
}
const runbookRef = requireString(alerts.runbook_ref, "alerts.runbook_ref");
if (!runbookRef.startsWith("docs/")) fail("alerts.runbook_ref-must-reference-docs");
requireBoolean(alerts.no_secrets_in_payloads, "alerts.no_secrets_in_payloads", true);

const health = requireObject(root.health, "config.health");
rejectUnknown(health, ["liveness_path", "readiness_path", "no_store", "no_sniff"], "config.health");
if (health.liveness_path !== "/healthz") fail("health.liveness_path-must-be-healthz");
if (health.readiness_path !== "/readyz") fail("health.readiness_path-must-be-readyz");
requireBoolean(health.no_store, "health.no_store", true);
requireBoolean(health.no_sniff, "health.no_sniff", true);

console.log(
  "W08_PRODUCTION_OBSERVABILITY_POLICY_PASS " +
    `metrics_retention_days=${metricsRetentionDays} logs_retention_days=${logsRetentionDays} ` +
    `slo_availability=${availability} write_error_rate=${writeErrorRate} ` +
    `alert_ack_minutes=${acknowledgementMinutes} routes=${routes.length} health=/healthz,/readyz`,
);
