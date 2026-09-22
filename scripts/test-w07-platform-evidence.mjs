#!/usr/bin/env node

// Credential-free regression cases for the cross-platform W07 evidence guard.

import assert from "node:assert/strict";
import {
  validateW07PlatformEvidence,
  W07_PLATFORM_EXPECTED_SOAK_ROUNDS,
} from "./verify-w07-platform-evidence.mjs";

const sourceRevision = "a".repeat(40);
const linuxLog = [
  "W07_PRODUCTION_QUALIFICATION_PROVENANCE_PASS repository=andymac4182/mount-rs workflow=W07 FoundationDB production qualification ref=refs/heads/main source_revision=" + sourceRevision + " run_id=123 run_attempt=1 runner=GitHub Actions 1",
  "FOUNDATIONDB_TEST_PASS topology=durable manifests=tests/foundationdb/Cargo.toml+integrations/mount-rs-foundationdb/Cargo.toml platform=linux/amd64 service_restart=pass soak_rounds=10",
  "W07_PRODUCTION_QUALIFICATION_EVIDENCE_PASS rounds=10 workload=composition operations=15",
].join("\n");
const macosLog =
  "W07_MACOS_FOUNDATIONDB_COMPILE_PASS provider=mount-rs-foundationdb cli=native_lifecycle napi=foundationdb\n";
const linuxSummary = {
  schema: 2,
  result: "qualification-pass",
  expectedSoakRounds: W07_PLATFORM_EXPECTED_SOAK_ROUNDS,
  markers: Object.fromEntries(
    [
      "config-policy-pass",
      "authority-heartbeat",
      "authority-stats",
      "chunked-composition",
      "napi",
      "cli",
      "foundationdb-workload-profile",
      "soak",
      "test",
    ].map((key) => [key, `${key}=pass`]),
  ),
  provenance: {
    repository: "andymac4182/mount-rs",
    workflow: "W07 FoundationDB production qualification",
    ref: "refs/heads/main",
    sourceRevision,
    runId: "123",
    runAttempt: "1",
    runner: "GitHub Actions 1",
  },
};

const cases = [
  {
    name: "valid-linux-and-macos-evidence",
    run() {
      const result = validateW07PlatformEvidence({
        linuxLog,
        linuxSummary,
        macosLog,
      });
      assert.equal(result.expectedSoakRounds, 10);
      assert.equal(result.sourceRevision, sourceRevision);
    },
  },
  {
    name: "missing-macos-marker",
    run() {
      assert.throws(
        () =>
          validateW07PlatformEvidence({
            linuxLog,
            linuxSummary,
            macosLog: "cargo check completed\n",
          }),
        /macos-foundationdb-compile-marker-missing/u,
      );
    },
  },
  {
    name: "linux-summary-not-terminal",
    run() {
      assert.throws(
        () =>
          validateW07PlatformEvidence({
            linuxLog,
            linuxSummary: { ...linuxSummary, result: "qualification-failed" },
            macosLog,
          }),
        /linux-summary-not-qualification-pass/u,
      );
    },
  },
  {
    name: "linux-round-count-mismatch",
    run() {
      assert.throws(
        () =>
          validateW07PlatformEvidence({
            linuxLog,
            linuxSummary: { ...linuxSummary, expectedSoakRounds: 5 },
            macosLog,
          }),
        /linux-summary-soak-rounds-mismatch/u,
      );
    },
  },
];

for (const testCase of cases) testCase.run();
console.log(`W07_PLATFORM_EVIDENCE_TEST_PASS cases=${cases.length}`);
