#!/usr/bin/env node

// Exercise both sides of the W07 rollout-ledger policy using temporary copies
// of the repository documents. This verifies the tracking control itself; it
// does not contact a provider or authorize a production rollout.

import { spawnSync } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { tmpdir } from "node:os";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const verifierPath = join(repoRoot, "scripts/verify-w07-rollout-ledger.mjs");
const sourcePaths = {
  tracker: join(repoRoot, "WORK_TRACKER.md"),
  rollout: join(repoRoot, "docs/foundationdb-production-rollout.md"),
  runbook: join(repoRoot, "docs/W07-operations-runbook.md"),
};

const source = {
  tracker: await readFile(sourcePaths.tracker, "utf8"),
  rollout: await readFile(sourcePaths.rollout, "utf8"),
  runbook: await readFile(sourcePaths.runbook, "utf8"),
};

const cases = [
  {
    name: "baseline-no-go",
    expectedStatus: 0,
    expectedOutput: "W07_ROLLOUT_LEDGER_POLICY_PASS decision=NO-GO",
    documents: source,
  },
  {
    name: "premature-w07.7-complete",
    expectedStatus: 1,
    expectedOutput: "reason=no-go-requires-open-w07.7",
    documents: {
      ...source,
      tracker: source.tracker.replace(
        "- [ ] W07.7 **Production rollout readiness and go/no-go:",
        "- [x] W07.7 **Production rollout readiness and go/no-go:",
      ),
    },
  },
  {
    name: "missing-nested-gate",
    expectedStatus: 1,
    expectedOutput: "reason=no-go-requires-open-identity-and-least-privilege",
    documents: {
      ...source,
      tracker: source.tracker.replace(
        "- [ ] **Identity and least privilege:**",
        "- [x] **Identity and least privilege:**",
      ),
    },
  },
  {
    name: "missing-external-drill-boundary",
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
    name: "missing-production-gate-row",
    expectedStatus: 1,
    expectedOutput: "reason=production-gate-ledger-must-have-fifteen-rows",
    documents: {
      ...source,
      rollout: source.rollout.replace(/^\| P4 —[^\n]*\n/mu, ""),
    },
  },
  {
    name: "no-go-terminal-production-gate",
    expectedStatus: 1,
    expectedOutput: "reason=no-go-requires-open-p8",
    documents: {
      ...source,
      rollout: source.rollout.replace(
        /^(\| P8 — [^|]+\| )[^|]+( \|)/mu,
        "$1Complete$2",
      ),
    },
  },
  {
    name: "premature-go",
    expectedStatus: 1,
    expectedOutput: "reason=go-requires-complete-w07.7",
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
    expectedOutput: "W07_ROLLOUT_LEDGER_POLICY_PASS decision=GO",
    documents: {
      tracker: (() => {
        let tracker = source.tracker.replace(
          "- [ ] W07.7 **Production rollout readiness and go/no-go:",
          "- [x] W07.7 **Production rollout readiness and go/no-go:",
        );
        for (const gate of [
          "Identity and least privilege",
          "Time and authority failure policy",
          "FoundationDB durability and recovery",
          "Production-like workload and soak",
          "Observability and operations",
          "Rollout and rollback",
          "Hosted and platform evidence",
        ]) {
          tracker = tracker.replace(
            `- [ ] **${gate}:**`,
            `- [x] **${gate}:**`,
          );
        }
        return tracker;
      })(),
      rollout: (() => {
        let rollout = source.rollout
          .replace(
            "| Production rollout | **NO-GO** |",
            "| Production rollout | **GO** |",
          )
          .replace(
            "No P0–P14 gate is currently terminally accepted.",
            "P0–P14 gate ledger accepted by release owner.",
          );
        for (let index = 0; index < 15; index += 1) {
          const gateId = `P${index}`;
          rollout = rollout.replace(
            new RegExp(`^(\\| ${gateId} — [^|]+\\| )[^|]+( \\|)`, "mu"),
            "$1Complete$2",
          );
        }
        return rollout;
      })(),
      runbook: source.runbook.replace(
        "current rollout decision remains **NO-GO**",
        "current rollout decision remains **GO**",
      ),
    },
  },
];

const sandbox = await mkdtemp(join(tmpdir(), "mount-rs-w07-rollout-ledger-"));
try {
  for (const testCase of cases) {
    const caseDirectory = join(sandbox, testCase.name);
    await mkdir(caseDirectory);
    const paths = {
      tracker: join(caseDirectory, "WORK_TRACKER.md"),
      rollout: join(caseDirectory, "rollout.md"),
      runbook: join(caseDirectory, "runbook.md"),
    };
    await Promise.all([
      writeFile(paths.tracker, testCase.documents.tracker),
      writeFile(paths.rollout, testCase.documents.rollout),
      writeFile(paths.runbook, testCase.documents.runbook),
    ]);

    const result = spawnSync(
      process.execPath,
      [verifierPath, paths.tracker, paths.rollout, paths.runbook],
      { cwd: repoRoot, encoding: "utf8" },
    );
    const output = `${result.stdout}${result.stderr}`;
    if (
      result.status !== testCase.expectedStatus ||
      !output.includes(testCase.expectedOutput)
    ) {
      console.error(
        `W07_ROLLOUT_LEDGER_TEST_FAIL case=${testCase.name} ` +
          `status=${result.status} output=${JSON.stringify(output.trim())}`,
      );
      process.exitCode = 1;
      break;
    }
  }
} finally {
  await rm(sandbox, { recursive: true, force: true });
}

if (process.exitCode !== 1) {
  console.log(`W07_ROLLOUT_LEDGER_TEST_PASS cases=${cases.length}`);
}
