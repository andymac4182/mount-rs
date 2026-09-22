#!/usr/bin/env node

// Exercise the W08 P06 capacity/load/soak policy with temporary fixtures.
// These are credential-free shape tests; they never contact a provider or
// resource collector.

import { spawnSync } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { tmpdir } from "node:os";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const verifierPath = join(repoRoot, "scripts/verify-w08-production-capacity.mjs");
const sourcePath = join(repoRoot, "tests/tidb/production-capacity-policy.json");
const source = JSON.parse(await readFile(sourcePath, "utf8"));

const cases = [
  {
    name: "capacity-baseline",
    expectedStatus: 0,
    expectedOutput: "W08_PRODUCTION_CAPACITY_POLICY_PASS profiles=5",
    config: source,
  },
  {
    name: "required-workload-profiles",
    expectedStatus: 1,
    expectedOutput: "reason=workload.profiles-must-match-required-set",
    config: {
      ...source,
      workload: { ...source.workload, profiles: ["baseline", "peak", "failover", "soak"] },
    },
  },
  {
    name: "minimum-duration",
    expectedStatus: 1,
    expectedOutput: "reason=workload.duration_minutes-must-be-safe-integer-at-least-60",
    config: { ...source, workload: { ...source.workload, duration_minutes: 30 } },
  },
  {
    name: "saturation-exceeds-peak",
    expectedStatus: 1,
    expectedOutput: "reason=workload.saturation_concurrency-must-be-safe-integer-at-least-33",
    config: { ...source, workload: { ...source.workload, saturation_concurrency: 32 } },
  },
  {
    name: "latency-bound",
    expectedStatus: 1,
    expectedOutput: "reason=thresholds.p95_latency_ms-must-be-at-most-1000",
    config: { ...source, thresholds: { ...source.thresholds, p95_latency_ms: 1200 } },
  },
  {
    name: "collector-required",
    expectedStatus: 1,
    expectedOutput: "reason=resources.collector-must-be-one-of-external-managed",
    config: { ...source, resources: { ...source.resources, collector: "local-only" } },
  },
  {
    name: "failover-scenarios-required",
    expectedStatus: 1,
    expectedOutput: "reason=failover.scenarios-must-match-required-set",
    config: {
      ...source,
      failover: {
        ...source.failover,
        scenarios: ["sql-frontend-restart", "tikv-restart", "pd-restart"],
      },
    },
  },
  {
    name: "soak-duration-required",
    expectedStatus: 1,
    expectedOutput: "reason=soak.duration_hours-must-be-safe-integer-at-least-4",
    config: { ...source, soak: { ...source.soak, duration_hours: 2 } },
  },
];

const sandbox = await mkdtemp(join(tmpdir(), "mount-rs-w08-production-capacity-"));
let failed = false;
try {
  for (const testCase of cases) {
    const caseDirectory = join(sandbox, testCase.name);
    await mkdir(caseDirectory);
    const configPath = join(caseDirectory, "capacity.json");
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
        `W08_PRODUCTION_CAPACITY_TEST_FAIL case=${testCase.name} ` +
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
else console.log(`W08_PRODUCTION_CAPACITY_TEST_PASS cases=${cases.length}`);
