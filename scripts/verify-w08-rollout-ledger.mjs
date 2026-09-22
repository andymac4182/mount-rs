#!/usr/bin/env node

// Fail-closed consistency check for the W08 production rollout ledger. This
// validates repository tracking state only; it does not promote local,
// hosted, provider, native, or fixture evidence into production acceptance.

import { readFile } from "node:fs/promises";

const rawArgs = process.argv.slice(2);
const requireGo = rawArgs.includes("--require-go");
const pathArgs = rawArgs.filter((argument) => argument !== "--require-go");
const [
  trackerPath = "WORK_TRACKER.md",
  rolloutPath = "docs/W08-production-rollout.md",
  runbookPath = "docs/W08-operations-runbook.md",
  ledgerPath = "docs/W08-progress-ledger.md",
] = pathArgs;

if (
  pathArgs.length > 4 ||
  rawArgs.some((argument) => argument.startsWith("--") && argument !== "--require-go")
) {
  console.error(
    "usage: verify-w08-rollout-ledger.mjs [WORK_TRACKER.md] " +
      "[rollout.md] [runbook.md] [progress-ledger.md] [--require-go]",
  );
  process.exit(2);
}

function fail(reason) {
  console.error(`W08_ROLLOUT_LEDGER_POLICY_FAIL reason=${reason}`);
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
const ledger = await load(ledgerPath, "progress-ledger");

const decisionMatches = [
  ...rollout.matchAll(/^\| Production rollout \| \*\*(NO-GO|GO)\*\* \|$/gmu),
];
if (decisionMatches.length !== 1) {
  fail("rollout-decision-must-be-exactly-one-go-or-no-go");
}
const decision = decisionMatches[0][1];
if (requireGo && decision !== "GO") {
  fail("production-admission-requires-go");
}

const functionalItems = 37;
for (let item = 1; item <= functionalItems; item += 1) {
  requireMatch(
    tracker,
    new RegExp(`^- \\[x\\] W08\\.${item}(?!\\d)\\b`, "mu"),
    `w08-${item}-must-be-checked`,
  );
}

const productionGates = 9;
for (let gate = 1; gate <= productionGates; gate += 1) {
  const id = `P0${gate}`;
  const trackerGate = new RegExp(`^- \\[([ x])\\] \\*\\*W08-${id}\\b`, "mu");
  const trackerMatch = tracker.match(trackerGate);
  if (!trackerMatch) fail(`missing-${id}-tracker-gate`);

  requireMatch(
    rollout,
    new RegExp(`^\\| ${id} \\u2014[^\\n]*\\|`, "mu"),
    `missing-${id}-rollout-gate`,
  );
  requireMatch(
    ledger,
    new RegExp(`^\\| \\*\\*${id}\\*\\* \\|`, "mu"),
    `missing-${id}-ledger-gate`,
  );

  if (decision === "NO-GO" && trackerMatch[1] !== " ") {
    fail(`no-go-requires-open-${id.toLowerCase()}`);
  }
}

if (decision === "NO-GO") {
  requireMatch(
    rollout,
    /No P01–P09 item is terminally accepted\./u,
    "no-go-requires-open-p01-p09-ledger",
  );
  requireMatch(
    ledger,
    /production rollout decision is currently\s+\*\*NO-GO\*\*/u,
    "no-go-requires-ledger-no-go",
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
} else {
  for (let gate = 1; gate <= productionGates; gate += 1) {
    const id = `P0${gate}`;
    requireMatch(
      tracker,
      new RegExp(`^- \\[x\\] \\*\\*W08-${id}\\b`, "mu"),
      `go-requires-complete-${id.toLowerCase()}`,
    );
  }
  if (/No P01–P09 item is terminally accepted\./u.test(rollout)) {
    fail("go-cannot-retain-open-p01-p09-ledger");
  }
  requireMatch(
    ledger,
    /production rollout decision is currently\s+\*\*GO\*\*/u,
    "go-requires-ledger-go",
  );
  requireMatch(
    runbook,
    /current rollout decision remains \*\*GO\*\*/u,
    "go-requires-runbook-go",
  );
}

console.log(
  `W08_ROLLOUT_LEDGER_POLICY_PASS decision=${decision} ` +
    `functional_items=${functionalItems} production_gates=${productionGates}`,
);
