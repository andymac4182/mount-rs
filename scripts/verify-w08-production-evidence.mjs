#!/usr/bin/env node

// Validate the W08 production evidence packet shape. This is a fail-closed
// admission control for tracking completeness; it cannot authenticate a
// provider, environment, owner, hosted job, or production result.

import { readFile } from "node:fs/promises";

const rawArgs = process.argv.slice(2);
const requireGo = rawArgs.includes("--require-go");
const pathArgs = rawArgs.filter((argument) => argument !== "--require-go");
const [
  evidencePath = "docs/W08-production-evidence.json",
  rolloutPath = "docs/W08-production-rollout.md",
] = pathArgs;

if (
  pathArgs.length > 2 ||
  rawArgs.some((argument) => argument.startsWith("--") && argument !== "--require-go")
) {
  console.error(
    "usage: verify-w08-production-evidence.mjs [evidence.json] " +
      "[rollout.md] [--require-go]",
  );
  process.exit(2);
}

function fail(reason) {
  console.error(`W08_PRODUCTION_EVIDENCE_POLICY_FAIL reason=${reason}`);
  process.exit(1);
}

async function load(path, name) {
  try {
    return await readFile(path, "utf8");
  } catch {
    fail(`${name}-unreadable`);
  }
}

function requireString(value, reason) {
  if (typeof value !== "string" || value.trim() === "") fail(reason);
}

function requireSha(value, reason) {
  if (!/^[0-9a-f]{40}$/u.test(value ?? "")) fail(reason);
}

const terminalStatuses = new Set(["success", "passed", "approved", "complete"]);

const evidenceText = await load(evidencePath, "evidence-packet");
const rollout = await load(rolloutPath, "rollout-ledger");

let packet;
try {
  packet = JSON.parse(evidenceText);
} catch {
  fail("evidence-packet-invalid-json");
}

if (packet === null || typeof packet !== "object" || Array.isArray(packet)) {
  fail("evidence-packet-must-be-object");
}
if (packet.schemaVersion !== 1) fail("unsupported-schema-version");
if (packet.repository !== "andymac4182/mount-rs") {
  fail("repository-must-be-andymac4182-mount-rs");
}

const decisionMatches = [
  ...rollout.matchAll(/^\| Production rollout \| \*\*(NO-GO|GO)\*\* \|$/gmu),
];
if (decisionMatches.length !== 1) fail("rollout-decision-must-be-exactly-one");
const rolloutDecision = decisionMatches[0][1];
if (packet.decision !== "NO-GO" && packet.decision !== "GO") {
  fail("decision-must-be-no-go-or-go");
}
if (packet.decision !== rolloutDecision) fail("decision-mismatch");
if (requireGo && packet.decision !== "GO") {
  fail("production-admission-requires-go");
}

if (packet.decision === "GO") requireSha(packet.sourceRevision, "go-requires-source-revision");
else if (packet.sourceRevision !== null) fail("no-go-source-revision-must-be-null");

const expectedGateIds = Array.from({ length: 9 }, (_, index) => `P0${index + 1}`);
if (!Array.isArray(packet.gates) || packet.gates.length !== expectedGateIds.length) {
  fail("evidence-packet-must-have-nine-gates");
}

const seenGateIds = new Set();
let closedGates = 0;
let evidenceRecords = 0;

for (const gateId of expectedGateIds) {
  const gate = packet.gates.find((candidate) => candidate?.id === gateId);
  if (!gate) fail(`missing-${gateId.toLowerCase()}-evidence-gate`);
  if (seenGateIds.has(gate.id)) fail(`duplicate-${gate.id.toLowerCase()}-evidence-gate`);
  seenGateIds.add(gate.id);

  if (gate.status !== "open" && gate.status !== "closed") {
    fail(`${gateId.toLowerCase()}-status-must-be-open-or-closed`);
  }
  if (!Array.isArray(gate.remainingActions) || gate.remainingActions.some((action) => typeof action !== "string")) {
    fail(`${gateId.toLowerCase()}-remaining-actions-must-be-string-array`);
  }
  if (!Array.isArray(gate.evidence)) fail(`${gateId.toLowerCase()}-evidence-must-be-array`);

  for (const [index, record] of gate.evidence.entries()) {
    if (record === null || typeof record !== "object" || Array.isArray(record)) {
      fail(`${gateId.toLowerCase()}-evidence-${index + 1}-must-be-object`);
    }
    requireSha(record.revision, `${gateId.toLowerCase()}-evidence-${index + 1}-revision`);
    if (
      !Array.isArray(record.providerVersions) ||
      record.providerVersions.length === 0 ||
      record.providerVersions.some((version) => typeof version !== "string" || version.trim() === "")
    ) {
      fail(`${gateId.toLowerCase()}-evidence-${index + 1}-provider-versions`);
    }
    requireString(record.topology, `${gateId.toLowerCase()}-evidence-${index + 1}-topology`);
    requireString(record.environment, `${gateId.toLowerCase()}-evidence-${index + 1}-environment`);
    requireString(record.testOrRunId, `${gateId.toLowerCase()}-evidence-${index + 1}-test-or-run-id`);
    if (
      typeof record.terminalStatus !== "string" ||
      !terminalStatuses.has(record.terminalStatus.trim().toLowerCase())
    ) {
      fail(`${gateId.toLowerCase()}-evidence-${index + 1}-terminal-status-not-terminal`);
    }
    requireString(record.owner, `${gateId.toLowerCase()}-evidence-${index + 1}-owner`);
    requireString(record.cleanupOutcome, `${gateId.toLowerCase()}-evidence-${index + 1}-cleanup-outcome`);
    requireString(record.rollbackOutcome, `${gateId.toLowerCase()}-evidence-${index + 1}-rollback-outcome`);
    requireString(record.evidenceRef, `${gateId.toLowerCase()}-evidence-${index + 1}-evidence-ref`);
    evidenceRecords += 1;
  }

  if (gate.status === "closed") {
    if (packet.decision !== "GO") fail(`no-go-requires-open-${gateId.toLowerCase()}`);
    if (gate.remainingActions.length !== 0) {
      fail(`${gateId.toLowerCase()}-closed-gate-has-remaining-actions`);
    }
    if (gate.evidence.length === 0) fail(`${gateId.toLowerCase()}-closed-gate-needs-evidence`);
    closedGates += 1;
  } else if (packet.decision === "GO") {
    fail(`go-requires-closed-${gateId.toLowerCase()}`);
  } else if (gate.remainingActions.length === 0) {
    fail(`${gateId.toLowerCase()}-open-gate-needs-remaining-actions`);
  }
}

if (packet.decision === "GO" && closedGates !== expectedGateIds.length) {
  fail("go-requires-all-gates-closed");
}

console.log(
  `W08_PRODUCTION_EVIDENCE_POLICY_PASS decision=${packet.decision} ` +
    `gates=${expectedGateIds.length} closed=${closedGates} ` +
    `evidence_records=${evidenceRecords} require_go=${requireGo}`,
);
