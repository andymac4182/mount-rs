#!/usr/bin/env node

// Exercise the W04 rollout-ledger policy with temporary document copies. This
// tests tracking controls only; it does not contact a provider or authorize a
// production rollout.

import { spawnSync } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { tmpdir } from "node:os";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const verifierPath = join(repoRoot, "scripts/verify-w04-rollout-ledger.mjs");
const sourcePaths = {
  tracker: join(repoRoot, "WORK_TRACKER.md"),
  rollout: join(repoRoot, "docs/W04-production-rollout.md"),
  ledger: join(repoRoot, "docs/w04-progress-ledger.md"),
};
const source = {
  tracker: await readFile(sourcePaths.tracker, "utf8"),
  rollout: await readFile(sourcePaths.rollout, "utf8"),
  ledger: await readFile(sourcePaths.ledger, "utf8"),
};

const closeOpenGates = (text) =>
  text.replace(/\| (P0[3-7])([^|]*)\| OPEN \|/g, "| $1$2| CLOSED |");

const cases = [
  {
    name: "baseline-no-go",
    expectedStatus: 0,
    expectedOutput: "W04_ROLLOUT_LEDGER_POLICY_PASS decision=NO-GO",
    documents: source,
  },
  {
    name: "premature-gate-closure",
    expectedStatus: 1,
    expectedOutput: "reason=no-go-requires-open-p03",
    documents: {
      ...source,
      rollout: source.rollout.replace(
        "| P03 persistent deployment and version policy | OPEN |",
        "| P03 persistent deployment and version policy | CLOSED |",
      ),
    },
  },
  {
    name: "premature-go",
    expectedStatus: 1,
    expectedOutput: "reason=production-decision-mismatch",
    documents: {
      ...source,
      rollout: source.rollout.replace(
        "| Production rollout | **NO-GO** |",
        "| Production rollout | **GO** |",
      ),
    },
  },
  {
    name: "missing-functional-item",
    expectedStatus: 1,
    expectedOutput: "reason=w04.2-must-be-checked",
    documents: {
      ...source,
      tracker: source.tracker.replaceAll("- [x] W04.2", "- [ ] W04.2"),
    },
  },
  {
    name: "complete-go-ledger",
    expectedStatus: 0,
    expectedOutput: "W04_ROLLOUT_LEDGER_POLICY_PASS decision=GO",
    args: ["--require-go"],
    documents: {
      ...source,
      rollout: closeOpenGates(source.rollout)
        .replace("| Production rollout | **NO-GO** |", "| Production rollout | **GO** |")
        .replace("The current decision is **NO-GO**.", "The current decision is **GO**.")
        .replace(
          "The W04 production decision remains **NO-GO**",
          "The W04 production decision remains **GO**",
        ),
      ledger: source.ledger
        .replace(
          "| Production rollout decision | NO-GO | 0% final release approval |",
          "| Production rollout decision | GO | 100% final release approval |",
        ),
    },
  },
];

const sandbox = await mkdtemp(join(tmpdir(), "mount-rs-w04-rollout-ledger-"));
let failed = false;
try {
  for (const testCase of cases) {
    const caseDirectory = join(sandbox, testCase.name);
    await mkdir(caseDirectory);
    const paths = {
      tracker: join(caseDirectory, "WORK_TRACKER.md"),
      rollout: join(caseDirectory, "rollout.md"),
      ledger: join(caseDirectory, "ledger.md"),
    };
    await Promise.all([
      writeFile(paths.tracker, testCase.documents.tracker),
      writeFile(paths.rollout, testCase.documents.rollout),
      writeFile(paths.ledger, testCase.documents.ledger),
    ]);

    const result = spawnSync(
      process.execPath,
      [verifierPath, ...(testCase.args ?? []), paths.tracker, paths.rollout, paths.ledger],
      { cwd: repoRoot, encoding: "utf8" },
    );
    const output = `${result.stdout}${result.stderr}`;
    if (
      result.status !== testCase.expectedStatus ||
      !output.includes(testCase.expectedOutput)
    ) {
      console.error(
        `W04_ROLLOUT_LEDGER_TEST_FAIL case=${testCase.name} ` +
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
else console.log(`W04_ROLLOUT_LEDGER_TEST_PASS cases=${cases.length}`);
