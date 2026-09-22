#!/usr/bin/env node

// Exercise the W08 P04 upgrade/compatibility/rollback policy with temporary
// fixtures. These are credential-free shape tests; they do not mutate a provider.

import { spawnSync } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { tmpdir } from "node:os";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const verifierPath = join(repoRoot, "scripts/verify-w08-production-upgrade.mjs");
const sourcePath = join(repoRoot, "tests/tidb/production-upgrade-policy.json");
const source = JSON.parse(await readFile(sourcePath, "utf8"));

const cases = [
  {
    name: "rolling-upgrade-baseline",
    expectedStatus: 0,
    expectedOutput: "W08_PRODUCTION_UPGRADE_POLICY_PASS current=v8.5.7",
    config: source,
  },
  {
    name: "current-version-must-be-pinned",
    expectedStatus: 1,
    expectedOutput: "reason=versions.current-must-be-pinned-semver",
    config: {
      ...source,
      versions: { ...source.versions, current: "v8.5" },
    },
  },
  {
    name: "current-version-must-be-newer",
    expectedStatus: 1,
    expectedOutput: "reason=versions.current-must-be-newer-than-previous",
    config: {
      ...source,
      versions: { ...source.versions, current: "v8.5.6" },
    },
  },
  {
    name: "all-client-surfaces-required",
    expectedStatus: 1,
    expectedOutput: "reason=clients.surfaces-must-contain-nfs",
    config: {
      ...source,
      clients: { ...source.clients, surfaces: source.clients.surfaces.filter((surface) => surface !== "nfs") },
    },
  },
  {
    name: "expand-contract-required",
    expectedStatus: 1,
    expectedOutput: "reason=migration.schema_strategy-must-be-one-of-expand-contract",
    config: {
      ...source,
      migration: { ...source.migration, schema_strategy: "destructive" },
    },
  },
  {
    name: "backward-compatibility-required",
    expectedStatus: 1,
    expectedOutput: "reason=migration.backward_compatible-must-be-true",
    config: {
      ...source,
      migration: { ...source.migration, backward_compatible: false },
    },
  },
  {
    name: "quorum-preservation-required",
    expectedStatus: 1,
    expectedOutput: "reason=upgrade.quorum_preserved-must-be-true",
    config: {
      ...source,
      upgrade: { ...source.upgrade, quorum_preserved: false },
    },
  },
  {
    name: "rollback-retention-floor",
    expectedStatus: 1,
    expectedOutput: "reason=rollback.artifact_retention_days-must-be-safe-integer-at-least-30",
    config: {
      ...source,
      rollback: { ...source.rollback, artifact_retention_days: 7 },
    },
  },
];

const sandbox = await mkdtemp(join(tmpdir(), "mount-rs-w08-production-upgrade-"));
let failed = false;
try {
  for (const testCase of cases) {
    const caseDirectory = join(sandbox, testCase.name);
    await mkdir(caseDirectory);
    const configPath = join(caseDirectory, "upgrade.json");
    await writeFile(configPath, `${JSON.stringify(testCase.config, null, 2)}\n`);

    const result = spawnSync(process.execPath, [verifierPath, configPath], {
      cwd: repoRoot,
      encoding: "utf8",
    });
    const output = `${result.stdout}${result.stderr}`;
    if (
      result.status !== testCase.expectedStatus ||
      !output.includes(testCase.expectedOutput)
    ) {
      console.error(
        `W08_PRODUCTION_UPGRADE_TEST_FAIL case=${testCase.name} ` +
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
else console.log(`W08_PRODUCTION_UPGRADE_TEST_PASS cases=${cases.length}`);
