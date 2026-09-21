#!/usr/bin/env node

// Exercise the W08 production evidence packet shape with temporary copies.
// These are tracking-policy tests only; they do not contact providers or
// authorize production rollout.

import { spawnSync } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { tmpdir } from "node:os";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const verifierPath = join(repoRoot, "scripts/verify-w08-production-evidence.mjs");
const evidencePath = join(repoRoot, "docs/W08-production-evidence.json");
const rolloutPath = join(repoRoot, "docs/W08-production-rollout.md");
const sourceEvidence = JSON.parse(await readFile(evidencePath, "utf8"));
const sourceRollout = await readFile(rolloutPath, "utf8");

function completeGoPacket() {
  const packet = structuredClone(sourceEvidence);
  packet.decision = "GO";
  packet.sourceRevision = "a".repeat(40);
  packet.gates = packet.gates.map((gate) => ({
    ...gate,
    status: "closed",
    remainingActions: [],
    evidence: [
      {
        revision: "b".repeat(40),
        providerVersions: ["provider-image@sha256:fixture"],
        topology: "production-like fixture topology",
        environment: "fixture-environment",
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
    expectedOutput: "W08_PRODUCTION_EVIDENCE_POLICY_PASS decision=NO-GO",
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
    expectedOutput: "reason=evidence-packet-must-have-nine-gates",
    evidence: { ...sourceEvidence, gates: sourceEvidence.gates.slice(0, 8) },
    rollout: sourceRollout,
  },
  {
    name: "no-go-closed-gate",
    expectedStatus: 1,
    expectedOutput: "reason=no-go-requires-open-p01",
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
    expectedOutput: "reason=p01-open-gate-needs-remaining-actions",
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
    expectedOutput: "reason=p01-closed-gate-needs-evidence",
    evidence: (() => {
      const packet = completeGoPacket();
      packet.gates[0] = { ...packet.gates[0], evidence: [] };
      return packet;
    })(),
    rollout: sourceRollout.replace(
      "| Production rollout | **NO-GO** |",
      "| Production rollout | **GO** |",
    ),
    args: ["--require-go"],
  },
  {
    name: "go-invalid-evidence",
    expectedStatus: 1,
    expectedOutput: "reason=p01-evidence-1-owner",
    evidence: (() => {
      const packet = completeGoPacket();
      packet.gates[0].evidence[0].owner = "";
      return packet;
    })(),
    rollout: sourceRollout.replace(
      "| Production rollout | **NO-GO** |",
      "| Production rollout | **GO** |",
    ),
    args: ["--require-go"],
  },
  {
    name: "go-placeholder-evidence",
    expectedStatus: 1,
    expectedOutput: "reason=p01-evidence-1-topology-must-be-concrete",
    evidence: (() => {
      const packet = completeGoPacket();
      packet.gates[0].evidence[0].topology = "TBD";
      return packet;
    })(),
    rollout: sourceRollout.replace(
      "| Production rollout | **NO-GO** |",
      "| Production rollout | **GO** |",
    ),
    args: ["--require-go"],
  },
  {
    name: "go-nonterminal-evidence",
    expectedStatus: 1,
    expectedOutput: "reason=p01-evidence-1-terminal-status-not-terminal",
    evidence: (() => {
      const packet = completeGoPacket();
      packet.gates[0].evidence[0].terminalStatus = "pending";
      return packet;
    })(),
    rollout: sourceRollout.replace(
      "| Production rollout | **NO-GO** |",
      "| Production rollout | **GO** |",
    ),
    args: ["--require-go"],
  },
  {
    name: "complete-go",
    expectedStatus: 0,
    expectedOutput: "W08_PRODUCTION_EVIDENCE_POLICY_PASS decision=GO",
    evidence: completeGoPacket(),
    rollout: sourceRollout.replace(
      "| Production rollout | **NO-GO** |",
      "| Production rollout | **GO** |",
    ),
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

const sandbox = await mkdtemp(join(tmpdir(), "mount-rs-w08-production-evidence-"));
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
        `W08_PRODUCTION_EVIDENCE_TEST_FAIL case=${testCase.name} ` +
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
else console.log(`W08_PRODUCTION_EVIDENCE_TEST_PASS cases=${cases.length}`);
