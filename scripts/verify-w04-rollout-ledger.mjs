#!/usr/bin/env node

// Fail-closed consistency check for the W04 production rollout ledger. This
// validates repository tracking state only; it does not authenticate a
// deployment, provider, artifact, operator, or production result.

import { readFile } from "node:fs/promises";

const rawArgs = process.argv.slice(2);
const requireGo = rawArgs.includes("--require-go");
const pathArgs = rawArgs.filter((argument) => argument !== "--require-go");
const [
  trackerPath = "WORK_TRACKER.md",
  rolloutPath = "docs/W04-production-rollout.md",
  ledgerPath = "docs/w04-progress-ledger.md",
] = pathArgs;

if (
  pathArgs.length > 3 ||
  rawArgs.some((argument) => argument.startsWith("--") && argument !== "--require-go")
) {
  console.error(
    "usage: verify-w04-rollout-ledger.mjs [WORK_TRACKER.md] " +
      "[docs/W04-production-rollout.md] [docs/w04-progress-ledger.md] " +
      "[--require-go]",
  );
  process.exit(2);
}

function fail(reason) {
  console.error(`W04_ROLLOUT_LEDGER_POLICY_FAIL reason=${reason}`);
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
const rollout = await load(rolloutPath, "rollout");
const ledger = await load(ledgerPath, "progress-ledger");

for (const item of ["W04.1", "W04.2", "W04.3"]) {
  requireMatch(tracker, new RegExp(`^- \\[x\\] ${item}\\b`, "mu"), `${item.toLowerCase()}-must-be-checked`);
}

requireMatch(ledger, /^## Live current status \(authoritative\)/mu, "missing-live-status-section");
requireMatch(
  ledger,
  /^\| Work item \| Status \| Completion \| Evidence \| Remaining action \| Provisional estimate \| External blocker or gate \|$/mu,
  "live-status-table-must-track-production-fields",
);
requireMatch(ledger, /^## Session time log/mu, "missing-session-time-log");
requireMatch(ledger, /Percentages and time estimates\s+are provisional/u, "missing-provisional-estimate-boundary");

const ledgerDecisionMatches = [
  ...ledger.matchAll(/^\| Production rollout decision \| (NO-GO|GO) \|/gmu),
];
if (ledgerDecisionMatches.length !== 1) {
  fail("ledger-production-decision-must-be-exactly-one-current-row");
}
const ledgerDecision = ledgerDecisionMatches[0][1];

const rolloutDecisionMatches = [
  ...rollout.matchAll(/^\| Production rollout \| \*\*(NO-GO|GO)\*\* \|/gmu),
];
if (rolloutDecisionMatches.length !== 1) {
  fail("rollout-production-decision-must-be-exactly-one-summary-row");
}
const rolloutDecision = rolloutDecisionMatches[0][1];
if (rolloutDecision !== ledgerDecision) fail("production-decision-mismatch");
if (requireGo && rolloutDecision !== "GO") fail("production-admission-requires-go");

requireMatch(
  rollout,
  new RegExp(`The current decision is \\*\\*${rolloutDecision}\\*\\*\\.`, "u"),
  "rollout-prose-decision-must-match-summary",
);
requireMatch(rollout, /^## Release decision and sign-off/mu, "missing-release-sign-off-section");
requireMatch(
  rollout,
  new RegExp(`The W04 production decision remains \\*\\*${rolloutDecision}\\*\\*`, "u"),
  "release-sign-off-decision-must-match-summary",
);

const expectedGateIds = ["P01", "P02", "P03", "P04", "P05", "P06", "P07"];
const gateMatches = [
  ...rollout.matchAll(/^\| (P0[1-7])(?: [^|]+)? \| (OPEN|CLOSED) \|/gmu),
];
if (gateMatches.length !== expectedGateIds.length) {
  fail("production-gate-summary-must-have-seven-rows");
}

const gates = new Map();
for (const [, id, status] of gateMatches) {
  if (gates.has(id)) fail(`duplicate-${id.toLowerCase()}-production-gate`);
  gates.set(id, status);
}
for (const id of expectedGateIds) {
  if (!gates.has(id)) fail(`missing-${id.toLowerCase()}-production-gate`);
}

const openGates = expectedGateIds.filter((id) => gates.get(id) === "OPEN");
if (rolloutDecision === "NO-GO") {
  if (openGates.length === 0) fail("no-go-requires-open-production-gate");
  for (const id of ["P03", "P04", "P05", "P06", "P07"]) {
    if (gates.get(id) !== "OPEN") fail(`no-go-requires-open-${id.toLowerCase()}`);
  }
  requireMatch(ledger, /\| Production rollout decision \| NO-GO \| 0% final release approval \|/u, "no-go-requires-zero-release-approval");
  requireMatch(rollout, /Not executed|Not connected|Not assigned/u, "no-go-requires-unmet-deployment-gates");
} else {
  if (openGates.length !== 0) fail("go-requires-closed-production-gates");
  for (const id of expectedGateIds) {
    if (gates.get(id) !== "CLOSED") fail(`go-requires-closed-${id.toLowerCase()}`);
  }
}

console.log(
  `W04_ROLLOUT_LEDGER_POLICY_PASS decision=${rolloutDecision} ` +
    `production_gates=${expectedGateIds.length} open=${openGates.length} ` +
    `require_go=${requireGo}`,
);
