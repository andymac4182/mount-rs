#!/usr/bin/env node

// Validate the W07 production evidence packet shape. This is fail-closed
// admission control for tracking completeness; it cannot authenticate a
// provider, environment, owner, hosted job, or production result.

import { readFile } from "node:fs/promises";

const rawArgs = process.argv.slice(2);
const requireGo = rawArgs.includes("--require-go");
const pathArgs = rawArgs.filter((argument) => argument !== "--require-go");
const [
  evidencePath = "docs/W07-production-evidence.json",
  rolloutPath = "docs/foundationdb-production-rollout.md",
] = pathArgs;

if (
  pathArgs.length > 2 ||
  rawArgs.some(
    (argument) => argument.startsWith("--") && argument !== "--require-go",
  )
) {
  console.error(
    "usage: verify-w07-production-evidence.mjs [evidence.json] " +
      "[rollout.md] [--require-go]",
  );
  process.exit(2);
}

function fail(reason) {
  console.error(`W07_PRODUCTION_EVIDENCE_POLICY_FAIL reason=${reason}`);
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

const placeholderPattern =
  /(?:\b(?:TBD|TODO|UNKNOWN|PENDING|CHANGEME)\b|<[^>\n]+>|\{\{[^}\n]+\}\})/iu;

function requireConcreteString(value, reason) {
  requireString(value, reason);
  if (placeholderPattern.test(value.trim())) fail(`${reason}-must-be-concrete`);
}

function requireOptionalConcreteString(value, reason) {
  if (value !== null) requireConcreteString(value, reason);
}

function requireSha(value, reason) {
  if (!/^[0-9a-f]{40}$/u.test(value ?? "")) fail(reason);
}

const terminalStatuses = new Set(["success", "passed", "approved", "complete"]);
const expectedGates = [
  { id: "W07-P01", name: "Identity and least privilege" },
  { id: "W07-P02", name: "Time and authority failure policy" },
  { id: "W07-P03", name: "FoundationDB durability and recovery" },
  { id: "W07-P04", name: "Production-like workload and soak" },
  { id: "W07-P05", name: "Observability and operations" },
  { id: "W07-P06", name: "Rollout and rollback" },
  { id: "W07-P07", name: "Hosted and platform evidence" },
];

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

if (packet.decision === "GO") {
  requireSha(packet.sourceRevision, "go-requires-source-revision");
} else if (packet.sourceRevision !== null) {
  fail("no-go-source-revision-must-be-null");
}

if (!Array.isArray(packet.gates) || packet.gates.length !== expectedGates.length) {
  fail("evidence-packet-must-have-seven-gates");
}

const seenGateIds = new Set();
let closedGates = 0;
let evidenceRecords = 0;

for (const expected of expectedGates) {
  const gate = packet.gates.find((candidate) => candidate?.id === expected.id);
  const gateKey = expected.id.toLowerCase();
  if (!gate) fail(`missing-${gateKey}-evidence-gate`);
  if (seenGateIds.has(gate.id)) fail(`duplicate-${gateKey}-evidence-gate`);
  seenGateIds.add(gate.id);

  if (gate.name !== expected.name) fail(`${gateKey}-name-mismatch`);
  for (const field of ["accountableOwner", "targetEnvironment", "testOrRunId"]) {
    if (!Object.hasOwn(gate, field)) {
      fail(`${gateKey}-${field}-field-required`);
    }
    requireOptionalConcreteString(gate[field], `${gateKey}-${field}`);
  }
  if (gate.status !== "open" && gate.status !== "closed") {
    fail(`${gateKey}-status-must-be-open-or-closed`);
  }
  if (
    !Array.isArray(gate.remainingActions) ||
    gate.remainingActions.some((action) => typeof action !== "string")
  ) {
    fail(`${gateKey}-remaining-actions-must-be-string-array`);
  }
  if (!Array.isArray(gate.evidence)) fail(`${gateKey}-evidence-must-be-array`);

  for (const [index, record] of gate.evidence.entries()) {
    const recordKey = `${gateKey}-evidence-${index + 1}`;
    if (record === null || typeof record !== "object" || Array.isArray(record)) {
      fail(`${recordKey}-must-be-object`);
    }
    requireSha(record.revision, `${recordKey}-revision`);
    if (
      !Array.isArray(record.providerVersions) ||
      record.providerVersions.length === 0 ||
      record.providerVersions.some(
        (version) =>
          typeof version !== "string" ||
          version.trim() === "" ||
          placeholderPattern.test(version.trim()),
      )
    ) {
      fail(`${recordKey}-provider-versions`);
    }
    requireConcreteString(record.topology, `${recordKey}-topology`);
    requireConcreteString(record.environment, `${recordKey}-environment`);
    requireConcreteString(record.testOrRunId, `${recordKey}-test-or-run-id`);
    if (
      typeof record.terminalStatus !== "string" ||
      !terminalStatuses.has(record.terminalStatus.trim().toLowerCase())
    ) {
      fail(`${recordKey}-terminal-status-not-terminal`);
    }
    requireConcreteString(record.owner, `${recordKey}-owner`);
    requireConcreteString(record.cleanupOutcome, `${recordKey}-cleanup-outcome`);
    requireConcreteString(record.rollbackOutcome, `${recordKey}-rollback-outcome`);
    requireConcreteString(record.evidenceRef, `${recordKey}-evidence-ref`);
    evidenceRecords += 1;
  }

  if (gate.status === "closed") {
    if (packet.decision !== "GO") fail(`no-go-requires-open-${gateKey}`);
    if (gate.remainingActions.length !== 0) {
      fail(`${gateKey}-closed-gate-has-remaining-actions`);
    }
    if (gate.evidence.length === 0) fail(`${gateKey}-closed-gate-needs-evidence`);
    for (const field of ["accountableOwner", "targetEnvironment", "testOrRunId"]) {
      requireConcreteString(gate[field], `${gateKey}-${field}`);
    }
    closedGates += 1;
  } else if (packet.decision === "GO") {
    fail(`go-requires-closed-${gateKey}`);
  } else if (gate.remainingActions.length === 0) {
    fail(`${gateKey}-open-gate-needs-remaining-actions`);
  }
}

if (packet.decision === "GO" && closedGates !== expectedGates.length) {
  fail("go-requires-all-gates-closed");
}

console.log(
  `W07_PRODUCTION_EVIDENCE_POLICY_PASS decision=${packet.decision} ` +
    `gates=${expectedGates.length} closed=${closedGates} ` +
    `evidence_records=${evidenceRecords} require_go=${requireGo}`,
);
