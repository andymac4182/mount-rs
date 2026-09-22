#!/usr/bin/env node

// Fail-closed consistency check for the W07 production rollout ledger. This
// validates the repository's tracking state only; it does not promote local,
// hosted, or fixture evidence into production acceptance.

import { readFile } from "node:fs/promises";

const [
  trackerPath = "WORK_TRACKER.md",
  rolloutPath = "docs/foundationdb-production-rollout.md",
  runbookPath = "docs/W07-operations-runbook.md",
] = process.argv.slice(2);

if (process.argv.length > 5) {
  console.error(
    "usage: verify-w07-rollout-ledger.mjs [WORK_TRACKER.md] [rollout.md] [runbook.md]",
  );
  process.exit(2);
}

function fail(reason) {
  console.error(`W07_ROLLOUT_LEDGER_POLICY_FAIL reason=${reason}`);
  process.exit(1);
}

async function load(path, name) {
  try {
    return await readFile(path, "utf8");
  } catch {
    fail(`${name}-unreadable`);
  }
}

function requireMatch(source, pattern, reason) {
  if (!pattern.test(source)) fail(reason);
}

const tracker = await load(trackerPath, "tracker");
const rollout = await load(rolloutPath, "rollout-ledger");
const runbook = await load(runbookPath, "operations-runbook");

const productionGateIds = Array.from({ length: 15 }, (_, index) => `P${index}`);
const productionGateRows = [
  ...rollout.matchAll(
    /^\|\s*(P(?:0|[1-9]|1[0-4]))\s+—[^|\n]*\|\s*([^|\n]+?)\s*\|/gmu,
  ),
].map((match) => ({ id: match[1], status: match[2].trim() }));
if (productionGateRows.length !== productionGateIds.length) {
  fail("production-gate-ledger-must-have-fifteen-rows");
}
const productionGateStatuses = new Map();
for (const gateId of productionGateIds) {
  const matchingRows = productionGateRows.filter((row) => row.id === gateId);
  if (matchingRows.length !== 1) {
    fail(`production-gate-${gateId.toLowerCase()}-row-must-be-unique`);
  }
  productionGateStatuses.set(gateId, matchingRows[0].status);
}

const terminalGateStatus = /^(?:complete|go|accepted)\b/iu;

const noGo = /\|\s*Production rollout\s*\|\s*\*\*NO-GO\*\*\s*\|/u.test(
  rollout,
);
const go = /\|\s*Production rollout\s*\|\s*\*\*GO\*\*\s*\|/u.test(
  rollout,
);
if (noGo === go) fail("rollout-decision-must-be-exactly-go-or-no-go");

const trackerW07Open =
  /- \[ \] W07\.7 \*\*Production rollout readiness and go\/no-go:/u.test(
    tracker,
  );
const trackerW07Complete = /- \[x\] W07\.7\b/u.test(tracker);
if (trackerW07Open === trackerW07Complete) {
  fail("w07.7-checkbox-must-be-exactly-open-or-complete");
}

const nestedGates = [
  "Identity and least privilege",
  "Time and authority failure policy",
  "FoundationDB durability and recovery",
  "Production-like workload and soak",
  "Observability and operations",
  "Rollout and rollback",
  "Hosted and platform evidence",
];

if (noGo) {
  requireMatch(
    tracker,
    /- \[ \] W07\.7 \*\*Production rollout readiness and go\/no-go:/u,
    "no-go-requires-open-w07.7",
  );
  if (trackerW07Complete) fail("no-go-cannot-mark-w07.7-complete");
  for (const gate of nestedGates) {
    const escaped = gate.replace(/[.*+?^${}()|[\]\\]/gu, "\\$&");
    requireMatch(
      tracker,
      new RegExp(`- \\[ \\] \\*\\*${escaped}:\\*\\*`, "u"),
      `no-go-requires-open-${gate.toLowerCase().replaceAll(" ", "-")}`,
    );
  }
  requireMatch(
    rollout,
    /No P0–P14 gate is currently terminally accepted\./u,
    "no-go-requires-open-p0-p14-ledger",
  );
  requireMatch(
    runbook,
    /current rollout decision remains \*\*NO-GO\*\*/u,
    "no-go-requires-runbook-no-go",
  );
  requireMatch(
    runbook,
    /Not executed — external production gate/u,
    "no-go-requires-unexecuted-drill-boundary",
  );
  for (const [gateId, status] of productionGateStatuses) {
    if (terminalGateStatus.test(status)) {
      fail(`no-go-requires-open-${gateId.toLowerCase()}`);
    }
  }
} else {
  requireMatch(
    tracker,
    /- \[x\] W07\.7 \*\*Production rollout readiness and go\/no-go:/u,
    "go-requires-complete-w07.7",
  );
  for (const gate of nestedGates) {
    const escaped = gate.replace(/[.*+?^${}()|[\]\\]/gu, "\\$&");
    requireMatch(
      tracker,
      new RegExp(`- \\[x\\] \\*\\*${escaped}:\\*\\*`, "u"),
      `go-requires-complete-${gate.toLowerCase().replaceAll(" ", "-")}`,
    );
  }
  requireMatch(
    runbook,
    /current rollout decision remains \*\*GO\*\*/u,
    "go-requires-runbook-go",
  );
  if (/No P0–P14 gate is currently terminally accepted\./u.test(rollout)) {
    fail("go-cannot-retain-open-p0-p14-ledger");
  }
  for (const [gateId, status] of productionGateStatuses) {
    if (!terminalGateStatus.test(status)) {
      fail(`go-requires-terminal-${gateId.toLowerCase()}`);
    }
  }
}

console.log(
  `W07_ROLLOUT_LEDGER_POLICY_PASS decision=${noGo ? "NO-GO" : "GO"} ` +
    `w07_7=${trackerW07Complete ? "complete" : "open"} ` +
    `nested_gates=${nestedGates.length}`,
);
