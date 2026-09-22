#!/usr/bin/env node

// Assemble the cross-platform W07 qualification boundary. This is an
// evidence-integrity check: it requires terminal Linux qualification plus
// macOS native-feature compilation, but it never promotes either result to
// production support, capacity, signing, or live-cluster acceptance.

import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

export const W07_PLATFORM_EXPECTED_SOAK_ROUNDS = 10;

function failure(reason) {
  throw new Error(`W07_PLATFORM_EVIDENCE_FAIL reason=${reason}`);
}

function requireObject(value, reason) {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    failure(reason);
  }
}

function requireString(value, reason) {
  if (typeof value !== "string" || value.trim() === "") failure(reason);
}

function requireMarker(text, pattern, reason) {
  const line = text.split(/\r?\n/u).find((candidate) => pattern.test(candidate));
  if (!line) failure(reason);
  return line.trim();
}

function parseMacosProvenance(macosLog) {
  const line = requireMarker(
    macosLog,
    /^W07_MACOS_FOUNDATIONDB_COMPILE_PROVENANCE\b/u,
    "macos-provenance-marker-missing",
  );
  const match = line.match(
    /^W07_MACOS_FOUNDATIONDB_COMPILE_PROVENANCE repository=(\S+) workflow=(.+?) ref=(\S+) source_revision=(\S+) run_id=(\S+) run_attempt=(\S+) runner=(.+)$/u,
  );
  if (!match) failure("macos-provenance-marker-malformed");
  const [
    ,
    repository,
    workflow,
    ref,
    sourceRevision,
    runId,
    runAttempt,
    runner,
  ] = match;
  if (!/^[0-9a-f]{40}$/iu.test(sourceRevision)) {
    failure("macos-provenance-source-revision-invalid");
  }
  return {
    repository,
    workflow,
    ref,
    sourceRevision,
    runId,
    runAttempt,
    runner: runner.trim(),
  };
}

export function validateW07PlatformEvidence({
  linuxLog,
  linuxSummary,
  macosLog,
  expectedSoakRounds = W07_PLATFORM_EXPECTED_SOAK_ROUNDS,
}) {
  requireString(linuxLog, "linux-log-required");
  requireString(macosLog, "macos-log-required");
  requireObject(linuxSummary, "linux-summary-must-be-object");
  if (!Number.isSafeInteger(expectedSoakRounds) || expectedSoakRounds <= 0) {
    failure("expected-soak-rounds-invalid");
  }

  if (linuxSummary.schema !== 2) failure("linux-summary-schema-mismatch");
  if (linuxSummary.result !== "qualification-pass") {
    failure("linux-summary-not-qualification-pass");
  }
  if (linuxSummary.expectedSoakRounds !== expectedSoakRounds) {
    failure("linux-summary-soak-rounds-mismatch");
  }

  requireObject(linuxSummary.provenance, "linux-summary-provenance-required");
  for (const field of [
    "repository",
    "workflow",
    "ref",
    "sourceRevision",
    "runId",
    "runAttempt",
    "runner",
  ]) {
    requireString(linuxSummary.provenance[field], `linux-provenance-${field}`);
  }
  if (!/^[0-9a-f]{40}$/iu.test(linuxSummary.provenance.sourceRevision)) {
    failure("linux-provenance-source-revision-invalid");
  }

  requireObject(linuxSummary.markers, "linux-summary-markers-required");
  for (const marker of [
    "config-policy-pass",
    "authority-heartbeat",
    "authority-stats",
    "chunked-composition",
    "napi",
    "cli",
    "foundationdb-workload-profile",
    "soak",
    "test",
  ]) {
    requireString(linuxSummary.markers[marker], `linux-marker-${marker}`);
  }

  const linuxQualification = requireMarker(
    linuxLog,
    new RegExp(
      `^W07_PRODUCTION_QUALIFICATION_EVIDENCE_PASS rounds=${expectedSoakRounds}\\b`,
      "u",
    ),
    "linux-qualification-marker-missing",
  );
  const linuxTest = requireMarker(
    linuxLog,
    new RegExp(
      `^FOUNDATIONDB_TEST_PASS\\b.*platform=linux/amd64\\b.*service_restart=pass\\s+soak_rounds=${expectedSoakRounds}\\b`,
      "u",
    ),
    "linux-durable-test-marker-missing",
  );
  const macosCompile = requireMarker(
    macosLog,
    /^W07_MACOS_FOUNDATIONDB_COMPILE_PASS provider=mount-rs-foundationdb cli=native_lifecycle napi=foundationdb$/u,
    "macos-foundationdb-compile-marker-missing",
  );
  const macosProvenance = parseMacosProvenance(macosLog);
  for (const field of [
    "repository",
    "workflow",
    "ref",
    "sourceRevision",
    "runId",
    "runAttempt",
    "runner",
  ]) {
    if (macosProvenance[field] !== linuxSummary.provenance[field]) {
      failure(`macos-provenance-${field}-mismatch`);
    }
  }

  return {
    expectedSoakRounds,
    sourceRevision: linuxSummary.provenance.sourceRevision,
    linuxQualification,
    linuxTest,
    macosCompile,
    macosProvenance,
  };
}

async function readJson(path, reason) {
  try {
    return JSON.parse(await readFile(path, "utf8"));
  } catch {
    failure(reason);
  }
}

export async function main(argv = process.argv.slice(2)) {
  if (argv.length !== 3 || argv.some((value) => value.length === 0)) {
    console.error(
      "usage: verify-w07-platform-evidence.mjs <linux-log> <linux-summary-json> <macos-log>",
    );
    return 2;
  }

  try {
    const [linuxLogPath, linuxSummaryPath, macosLogPath] = argv;
    const [linuxLog, linuxSummary, macosLog] = await Promise.all([
      readFile(linuxLogPath, "utf8").catch(() => failure("linux-log-unreadable")),
      readJson(linuxSummaryPath, "linux-summary-unreadable-or-invalid-json"),
      readFile(macosLogPath, "utf8").catch(() => failure("macos-log-unreadable")),
    ]);
    const result = validateW07PlatformEvidence({
      linuxLog,
      linuxSummary,
      macosLog,
    });
    console.log(
      `W07_PLATFORM_QUALIFICATION_PASS linux=terminal macos=feature-compile-only ` +
        `expected_soak_rounds=${result.expectedSoakRounds} ` +
        `source_revision=${result.sourceRevision} ` +
        "provenance=bound " +
        `linux_log=${resolve(linuxLogPath)} macos_log=${resolve(macosLogPath)}`,
    );
    return 0;
  } catch (error) {
    const message = error?.message || "validation-failed";
    console.error(
      message.startsWith("W07_PLATFORM_EVIDENCE_FAIL")
        ? message
        : `W07_PLATFORM_EVIDENCE_FAIL reason=${message}`,
    );
    return 1;
  }
}

const invokedPath = process.argv[1] ? resolve(process.argv[1]) : null;
if (invokedPath === resolve(fileURLToPath(import.meta.url))) {
  main().then((code) => {
    process.exitCode = code;
  });
}
