#!/usr/bin/env node

// Exercise the W07 production evidence packet shape with temporary copies.
// These are tracking-policy tests only; they do not contact providers or
// authorize production rollout.

import { spawnSync } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { tmpdir } from "node:os";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const verifierPath = join(repoRoot, "scripts/verify-w07-production-evidence.mjs");
const evidencePath = join(repoRoot, "docs/W07-production-evidence.json");
const rolloutPath = join(repoRoot, "docs/foundationdb-production-rollout.md");
const sourceEvidence = JSON.parse(await readFile(evidencePath, "utf8"));
const sourceRollout = await readFile(rolloutPath, "utf8");

function goRollout() {
  return sourceRollout.replace(
    "| Production rollout | **NO-GO** |",
    "| Production rollout | **GO** |",
  );
}

function completeGoPacket() {
  const packet = structuredClone(sourceEvidence);
  packet.decision = "GO";
  packet.sourceRevision = "a".repeat(40);
  packet.gates = packet.gates.map((gate) => ({
    ...gate,
    status: "closed",
    accountableOwner: "fixture-owner",
    targetEnvironment: "fixture-environment",
    testOrRunId: `fixture-${gate.id}`,
    remainingActions: [],
    evidence: [
      {
        recordId: `fixture-record-${gate.id}`,
        revision: "b".repeat(40),
        providerVersions: ["provider-image@sha256:fixture"],
        configuration: "config-sha256:fixture",
        topology: "production-like fixture topology",
        authority: "prefix=fixture; writer=fixture-writer; consumer=fixture-reader",
        environment: "fixture-environment",
        people: "incident-commander=fixture-ic; operator=fixture-operator; release-owner=fixture-owner; approver=fixture-approver",
        timestamps: {
          startedAt: "2026-09-22T00:00:00Z",
          completedAt: "2026-09-22T00:05:00Z",
          cleanedUpAt: "2026-09-22T00:06:00Z",
        },
        result: "PASS; fixture RPO/RTO/SLO impact and cleanup result recorded",
        testOrRunId: `fixture-${gate.id}`,
        terminalStatus: "success",
        owner: "fixture-owner",
        cleanupOutcome: "fixture cleanup recorded",
        rollbackOutcome: "fixture rollback recorded",
        evidenceRef: `docs/fixture-${gate.id}.md`,
      },
    ],
  }));
  return packet;
}

const cases = [
  {
    name: "baseline-no-go",
    expectedStatus: 0,
    expectedOutput: "W07_PRODUCTION_EVIDENCE_POLICY_PASS decision=NO-GO",
    evidence: sourceEvidence,
    rollout: sourceRollout,
  },
  {
    name: "decision-mismatch",
    expectedStatus: 1,
    expectedOutput: "reason=decision-mismatch",
    evidence: { ...sourceEvidence, decision: "GO" },
    rollout: sourceRollout,
  },
  {
    name: "missing-gate",
    expectedStatus: 1,
    expectedOutput: "reason=evidence-packet-must-have-seven-gates",
    evidence: { ...sourceEvidence, gates: sourceEvidence.gates.slice(0, 6) },
    rollout: sourceRollout,
  },
  {
    name: "missing-tracking-field",
    expectedStatus: 1,
    expectedOutput: "reason=w07-p01-accountableOwner-field-required",
    evidence: (() => {
      const packet = structuredClone(sourceEvidence);
      delete packet.gates[0].accountableOwner;
      return packet;
    })(),
    rollout: sourceRollout,
  },
  {
    name: "no-go-closed-gate",
    expectedStatus: 1,
    expectedOutput: "reason=no-go-requires-open-w07-p01",
    evidence: {
      ...sourceEvidence,
      gates: sourceEvidence.gates.map((gate, index) =>
        index === 0 ? { ...gate, status: "closed" } : gate,
      ),
    },
    rollout: sourceRollout,
  },
  {
    name: "open-gate-missing-actions",
    expectedStatus: 1,
    expectedOutput: "reason=w07-p01-open-gate-needs-remaining-actions",
    evidence: {
      ...sourceEvidence,
      gates: sourceEvidence.gates.map((gate, index) =>
        index === 0 ? { ...gate, remainingActions: [] } : gate,
      ),
    },
    rollout: sourceRollout,
  },
  {
    name: "go-missing-evidence",
    expectedStatus: 1,
    expectedOutput: "reason=w07-p01-closed-gate-needs-evidence",
    evidence: (() => {
      const packet = completeGoPacket();
      packet.gates[0] = { ...packet.gates[0], evidence: [] };
      return packet;
    })(),
    rollout: goRollout(),
    args: ["--require-go"],
  },
  {
    name: "go-invalid-evidence",
    expectedStatus: 1,
    expectedOutput: "reason=w07-p01-evidence-1-owner",
    evidence: (() => {
      const packet = completeGoPacket();
      packet.gates[0].evidence[0].owner = "";
      return packet;
    })(),
    rollout: goRollout(),
    args: ["--require-go"],
  },
  {
    name: "go-missing-context",
    expectedStatus: 1,
    expectedOutput: "reason=w07-p01-evidence-1-configuration-field-required",
    evidence: (() => {
      const packet = completeGoPacket();
      delete packet.gates[0].evidence[0].configuration;
      return packet;
    })(),
    rollout: goRollout(),
    args: ["--require-go"],
  },
  {
    name: "go-invalid-timestamp",
    expectedStatus: 1,
    expectedOutput: "reason=w07-p01-evidence-1-started-at-must-be-iso8601",
    evidence: (() => {
      const packet = completeGoPacket();
      packet.gates[0].evidence[0].timestamps.startedAt = "not-a-time";
      return packet;
    })(),
    rollout: goRollout(),
    args: ["--require-go"],
  },
  {
    name: "go-duplicate-record-id",
    expectedStatus: 1,
    expectedOutput: "reason=w07-p02-evidence-1-record-id-duplicate",
    evidence: (() => {
      const packet = completeGoPacket();
      packet.gates[1].evidence[0].recordId =
        packet.gates[0].evidence[0].recordId;
      return packet;
    })(),
    rollout: goRollout(),
    args: ["--require-go"],
  },
  {
    name: "go-placeholder-evidence",
    expectedStatus: 1,
    expectedOutput: "reason=w07-p01-evidence-1-topology-must-be-concrete",
    evidence: (() => {
      const packet = completeGoPacket();
      packet.gates[0].evidence[0].topology = "TBD";
      return packet;
    })(),
    rollout: goRollout(),
    args: ["--require-go"],
  },
  {
    name: "go-nonterminal-evidence",
    expectedStatus: 1,
    expectedOutput: "reason=w07-p01-evidence-1-terminal-status-not-terminal",
    evidence: (() => {
      const packet = completeGoPacket();
      packet.gates[0].evidence[0].terminalStatus = "pending";
      return packet;
    })(),
    rollout: goRollout(),
    args: ["--require-go"],
  },
  {
    name: "go-invalid-timestamp-order",
    expectedStatus: 1,
    expectedOutput: "reason=w07-p01-evidence-1-timestamps-order-invalid",
    evidence: (() => {
      const packet = completeGoPacket();
      packet.gates[0].evidence[0].timestamps.cleanedUpAt =
        "2026-09-22T00:04:00Z";
      return packet;
    })(),
    rollout: goRollout(),
    args: ["--require-go"],
  },
  {
    name: "complete-go",
    expectedStatus: 0,
    expectedOutput: "W07_PRODUCTION_EVIDENCE_POLICY_PASS decision=GO",
    evidence: completeGoPacket(),
    rollout: goRollout(),
    args: ["--require-go"],
  },
  {
    name: "admission-no-go",
    expectedStatus: 1,
    expectedOutput: "reason=production-admission-requires-go",
    evidence: sourceEvidence,
    rollout: sourceRollout,
    args: ["--require-go"],
  },
];

const sandbox = await mkdtemp(join(tmpdir(), "mount-rs-w07-production-evidence-"));
let failed = false;
try {
  for (const testCase of cases) {
    const caseDirectory = join(sandbox, testCase.name);
    await mkdir(caseDirectory);
    const paths = {
      evidence: join(caseDirectory, "evidence.json"),
      rollout: join(caseDirectory, "rollout.md"),
    };
    await Promise.all([
      writeFile(paths.evidence, `${JSON.stringify(testCase.evidence, null, 2)}\n`),
      writeFile(paths.rollout, testCase.rollout),
    ]);

    const result = spawnSync(
      process.execPath,
      [
        verifierPath,
        ...(testCase.args ?? []),
        paths.evidence,
        paths.rollout,
      ],
      { cwd: repoRoot, encoding: "utf8" },
    );
    const output = `${result.stdout}${result.stderr}`;
    if (
      result.status !== testCase.expectedStatus ||
      !output.includes(testCase.expectedOutput)
    ) {
      console.error(
        `W07_PRODUCTION_EVIDENCE_TEST_FAIL case=${testCase.name} ` +
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
else console.log(`W07_PRODUCTION_EVIDENCE_TEST_PASS cases=${cases.length}`);
