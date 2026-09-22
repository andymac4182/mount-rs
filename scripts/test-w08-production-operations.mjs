#!/usr/bin/env node

// Exercise the W08 P08 operations/readiness policy with temporary fixtures.
// These tests validate only credential-free control shape.

import { spawnSync } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { tmpdir } from "node:os";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const verifierPath = join(repoRoot, "scripts/verify-w08-production-operations.mjs");
const sourcePath = join(repoRoot, "tests/tidb/production-operations-policy.json");
const source = JSON.parse(await readFile(sourcePath, "utf8"));

const cases = [
  {
    name: "operations-baseline",
    expectedStatus: 0,
    expectedOutput: "W08_PRODUCTION_OPERATIONS_POLICY_PASS drills=9",
    config: source,
  },
  {
    name: "all-drills-required",
    expectedStatus: 1,
    expectedOutput: "reason=drills-must-have-nine-entries",
    config: { ...source, drills: source.drills.slice(0, 8) },
  },
  {
    name: "drill-gate-mapping",
    expectedStatus: 1,
    expectedOutput: "reason=drills.D08.gate_refs-must-match-required-set",
    config: {
      ...source,
      drills: source.drills.map((drill) =>
        drill.id === "D08" ? { ...drill, gate_refs: ["P08"] } : drill,
      ),
    },
  },
  {
    name: "evidence-fields-required",
    expectedStatus: 1,
    expectedOutput: "reason=evidence_record.fields-must-match-required-set",
    config: {
      ...source,
      evidence_record: {
        ...source.evidence_record,
        fields: source.evidence_record.fields.filter((field) => field !== "result"),
      },
    },
  },
  {
    name: "incident-closure-integrity",
    expectedStatus: 1,
    expectedOutput: "reason=incident.closure_requires_fresh_client-must-be-true",
    config: {
      ...source,
      incident: { ...source.incident, closure_requires_fresh_client: false },
    },
  },
  {
    name: "on-call-acknowledgement",
    expectedStatus: 1,
    expectedOutput: "reason=on_call.max_acknowledgement_minutes-must-be-at-most-15",
    config: {
      ...source,
      on_call: { ...source.on_call, max_acknowledgement_minutes: 30 },
    },
  },
  {
    name: "drill-cleanup-required",
    expectedStatus: 1,
    expectedOutput: "reason=drills.D03.cleanup_required-must-be-true",
    config: {
      ...source,
      drills: source.drills.map((drill) =>
        drill.id === "D03" ? { ...drill, cleanup_required: false } : drill,
      ),
    },
  },
  {
    name: "runbook-template-boundary",
    expectedStatus: 1,
    expectedOutput: "reason=runbook.current_decision-must-be-NO-GO",
    config: {
      ...source,
      runbook: { ...source.runbook, current_decision: "GO" },
    },
  },
];

const sandbox = await mkdtemp(join(tmpdir(), "mount-rs-w08-production-operations-"));
let failed = false;
try {
  for (const testCase of cases) {
    const caseDirectory = join(sandbox, testCase.name);
    await mkdir(caseDirectory);
    const configPath = join(caseDirectory, "operations.json");
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
        `W08_PRODUCTION_OPERATIONS_TEST_FAIL case=${testCase.name} ` +
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
else console.log(`W08_PRODUCTION_OPERATIONS_TEST_PASS cases=${cases.length}`);
