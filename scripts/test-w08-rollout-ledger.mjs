#!/usr/bin/env node

// Exercise both sides of the W08 rollout-ledger policy using temporary copies
// of repository documents. This tests the tracking control only; it does not
// contact a provider or authorize a production rollout.

import { spawnSync } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { tmpdir } from "node:os";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const verifierPath = join(repoRoot, "scripts/verify-w08-rollout-ledger.mjs");
const sourcePaths = {
  tracker: join(repoRoot, "WORK_TRACKER.md"),
  rollout: join(repoRoot, "docs/W08-production-rollout.md"),
  runbook: join(repoRoot, "docs/W08-operations-runbook.md"),
  ledger: join(repoRoot, "docs/W08-progress-ledger.md"),
};

const source = {
  tracker: await readFile(sourcePaths.tracker, "utf8"),
  rollout: await readFile(sourcePaths.rollout, "utf8"),
  runbook: await readFile(sourcePaths.runbook, "utf8"),
  ledger: await readFile(sourcePaths.ledger, "utf8"),
};

const cases = [
  {
    name: "baseline-no-go",
    expectedStatus: 0,
    expectedOutput: "W08_ROLLOUT_LEDGER_POLICY_PASS decision=NO-GO",
    documents: source,
  },
  {
    name: "premature-production-gate",
    expectedStatus: 1,
    expectedOutput: "reason=no-go-requires-open-p01",
    documents: {
      ...source,
      tracker: source.tracker.replace(
        "- [ ] **W08-P01 (25%)",
        "- [x] **W08-P01 (25%)",
      ),
    },
  },
  {
    name: "missing-drill-boundary",
    expectedStatus: 1,
    expectedOutput: "reason=no-go-requires-unexecuted-drill-boundary",
    documents: {
      ...source,
      runbook: source.runbook.replace(
        "Not executed — external production gate",
        "Drill status not recorded",
      ),
    },
  },
  {
    name: "premature-functional-item",
    expectedStatus: 1,
    expectedOutput: "reason=w08-32-must-be-checked",
    documents: {
      ...source,
      tracker: source.tracker.replace(
        "- [x] W08.32 **Production rollout ledger",
        "- [ ] W08.32 **Production rollout ledger",
      ),
    },
  },
  {
    name: "premature-go",
    expectedStatus: 1,
    expectedOutput: "reason=go-requires-complete-p01",
    documents: {
      ...source,
      rollout: source.rollout.replace(
        "| Production rollout | **NO-GO** |",
        "| Production rollout | **GO** |",
      ),
    },
  },
  {
    name: "complete-go-ledger",
    expectedStatus: 0,
    expectedOutput: "W08_ROLLOUT_LEDGER_POLICY_PASS decision=GO",
    args: ["--require-go"],
    documents: {
      tracker: source.tracker.replaceAll(
        /- \[ \] \*\*W08-P0([1-9])\b/g,
        "- [x] **W08-P0$1",
      ),
      rollout: source.rollout
        .replace(
          "| Production rollout | **NO-GO** |",
          "| Production rollout | **GO** |",
        )
        .replace("No P01–P09 item is terminally accepted.", "P01–P09 accepted by release owner."),
      runbook: source.runbook.replace(
        "current rollout decision remains **NO-GO**",
        "current rollout decision remains **GO**",
      ),
      ledger: source.ledger.replace(
        "production rollout decision is currently\n**NO-GO**",
        "production rollout decision is currently\n**GO**",
      ),
    },
  },
  {
    name: "production-admission-no-go",
    expectedStatus: 1,
    expectedOutput: "reason=production-admission-requires-go",
    args: ["--require-go"],
    documents: source,
  },
];

const sandbox = await mkdtemp(join(tmpdir(), "mount-rs-w08-rollout-ledger-"));
let failed = false;
try {
  for (const testCase of cases) {
    const caseDirectory = join(sandbox, testCase.name);
    await mkdir(caseDirectory);
    const paths = {
      tracker: join(caseDirectory, "WORK_TRACKER.md"),
      rollout: join(caseDirectory, "rollout.md"),
      runbook: join(caseDirectory, "runbook.md"),
      ledger: join(caseDirectory, "ledger.md"),
    };
    await Promise.all([
      writeFile(paths.tracker, testCase.documents.tracker),
      writeFile(paths.rollout, testCase.documents.rollout),
      writeFile(paths.runbook, testCase.documents.runbook),
      writeFile(paths.ledger, testCase.documents.ledger),
    ]);

    const result = spawnSync(
      process.execPath,
      [
        verifierPath,
        ...(testCase.args ?? []),
        paths.tracker,
        paths.rollout,
        paths.runbook,
        paths.ledger,
      ],
      { cwd: repoRoot, encoding: "utf8" },
    );
    const output = `${result.stdout}${result.stderr}`;
    if (
      result.status !== testCase.expectedStatus ||
      !output.includes(testCase.expectedOutput)
    ) {
      console.error(
        `W08_ROLLOUT_LEDGER_TEST_FAIL case=${testCase.name} ` +
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
else console.log(`W08_ROLLOUT_LEDGER_TEST_PASS cases=${cases.length}`);
