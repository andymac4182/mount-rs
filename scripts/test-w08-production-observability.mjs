#!/usr/bin/env node

// Exercise the W08 P05 observability/SLO/alerting policy with temporary
// fixtures. These are credential-free shape tests; they do not contact a collector or pager.

import { spawnSync } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { tmpdir } from "node:os";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const verifierPath = join(repoRoot, "scripts/verify-w08-production-observability.mjs");
const sourcePath = join(repoRoot, "tests/tidb/production-observability-policy.json");
const source = JSON.parse(await readFile(sourcePath, "utf8"));

const cases = [
  {
    name: "observability-baseline",
    expectedStatus: 0,
    expectedOutput: "W08_PRODUCTION_OBSERVABILITY_POLICY_PASS metrics_retention_days=90",
    config: source,
  },
  {
    name: "metrics-redaction-required",
    expectedStatus: 1,
    expectedOutput: "reason=telemetry.metrics.redaction_required-must-be-true",
    config: {
      ...source,
      telemetry: {
        ...source.telemetry,
        metrics: { ...source.telemetry.metrics, redaction_required: false },
      },
    },
  },
  {
    name: "logs-retention-floor",
    expectedStatus: 1,
    expectedOutput: "reason=telemetry.logs.retention_days-must-be-safe-integer-at-least-30",
    config: {
      ...source,
      telemetry: {
        ...source.telemetry,
        logs: { ...source.telemetry.logs, retention_days: 14 },
      },
    },
  },
  {
    name: "availability-slo-floor",
    expectedStatus: 1,
    expectedOutput: "reason=slo.availability_percent-must-be-number-at-least-99.9",
    config: { ...source, slo: { ...source.slo, availability_percent: 99.5 } },
  },
  {
    name: "write-error-slo-bound",
    expectedStatus: 1,
    expectedOutput: "reason=slo.write_error_rate_percent-must-be-at-most-0.1",
    config: { ...source, slo: { ...source.slo, write_error_rate_percent: 0.2 } },
  },
  {
    name: "paging-required",
    expectedStatus: 1,
    expectedOutput: "reason=alerts.paging_enabled-must-be-true",
    config: { ...source, alerts: { ...source.alerts, paging_enabled: false } },
  },
  {
    name: "alert-ack-bound",
    expectedStatus: 1,
    expectedOutput: "reason=alerts.acknowledgement_minutes-must-be-at-most-15",
    config: { ...source, alerts: { ...source.alerts, acknowledgement_minutes: 30 } },
  },
  {
    name: "health-contract-required",
    expectedStatus: 1,
    expectedOutput: "reason=health.readiness_path-must-be-readyz",
    config: { ...source, health: { ...source.health, readiness_path: "/status" } },
  },
];

const sandbox = await mkdtemp(join(tmpdir(), "mount-rs-w08-production-observability-"));
let failed = false;
try {
  for (const testCase of cases) {
    const caseDirectory = join(sandbox, testCase.name);
    await mkdir(caseDirectory);
    const configPath = join(caseDirectory, "observability.json");
    await writeFile(configPath, `${JSON.stringify(testCase.config, null, 2)}\n`);

    const result = spawnSync(process.execPath, [verifierPath, configPath], {
      cwd: repoRoot,
      encoding: "utf8",
    });
    const output = `${result.stdout}${result.stderr}`;
    if (
      result.status !== testCase.expectedStatus ||
      !output.includes(testCase.expectedOutput)
    ) {
      console.error(
        `W08_PRODUCTION_OBSERVABILITY_TEST_FAIL case=${testCase.name} ` +
          `status=${result.status} output=${JSON.stringify(output.trim())}`,
      );
      failed = true;
      break;
    }
  }
} finally {
  await rm(sandbox, { recursive: true, force: true });
}

if (failed) process.exitCode = 1;
else console.log(`W08_PRODUCTION_OBSERVABILITY_TEST_PASS cases=${cases.length}`);
