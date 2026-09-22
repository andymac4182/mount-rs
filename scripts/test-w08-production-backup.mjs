#!/usr/bin/env node

// Exercise the W08 P03 backup/restore/DR policy with temporary fixtures. These
// are credential-free shape tests; they do not contact a provider or region.

import { spawnSync } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { tmpdir } from "node:os";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const verifierPath = join(repoRoot, "scripts/verify-w08-production-backup.mjs");
const sourcePath = join(repoRoot, "tests/tidb/production-backup-policy.json");
const source = JSON.parse(await readFile(sourcePath, "utf8"));

const cases = [
  {
    name: "consistent-backup-baseline",
    expectedStatus: 0,
    expectedOutput: "W08_PRODUCTION_BACKUP_POLICY_PASS metadata=transactional-snapshot",
    config: source,
  },
  {
    name: "metadata-retention-floor",
    expectedStatus: 1,
    expectedOutput: "reason=metadata.retention_days-must-be-safe-integer-at-least-30",
    config: {
      ...source,
      metadata: { ...source.metadata, retention_days: 14 },
    },
  },
  {
    name: "blocks-require-versioning",
    expectedStatus: 1,
    expectedOutput: "reason=blocks.versioning-must-be-true",
    config: {
      ...source,
      blocks: { ...source.blocks, versioning: false },
    },
  },
  {
    name: "revision-capture-required",
    expectedStatus: 1,
    expectedOutput: "reason=metadata.revision_capture-must-be-true",
    config: {
      ...source,
      metadata: { ...source.metadata, revision_capture: false },
    },
  },
  {
    name: "restore-cannot-write-production",
    expectedStatus: 1,
    expectedOutput: "reason=restore.production_writer_access-must-be-false",
    config: {
      ...source,
      restore: { ...source.restore, production_writer_access: true },
    },
  },
  {
    name: "rpo-bound",
    expectedStatus: 1,
    expectedOutput: "reason=restore.rpo_minutes-must-be-at-most-60",
    config: {
      ...source,
      restore: { ...source.restore, rpo_minutes: 120 },
    },
  },
  {
    name: "region-loss-case-required",
    expectedStatus: 1,
    expectedOutput: "reason=restore.region_loss_case-must-be-true",
    config: {
      ...source,
      restore: { ...source.restore, region_loss_case: false },
    },
  },
  {
    name: "two-person-signoff-required",
    expectedStatus: 1,
    expectedOutput: "reason=dr.signoff_roles-must-contain-release-owner",
    config: {
      ...source,
      dr: { ...source.dr, signoff_roles: ["data-owner"] },
    },
  },
];

const sandbox = await mkdtemp(join(tmpdir(), "mount-rs-w08-production-backup-"));
let failed = false;
try {
  for (const testCase of cases) {
    const caseDirectory = join(sandbox, testCase.name);
    await mkdir(caseDirectory);
    const configPath = join(caseDirectory, "backup.json");
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
        `W08_PRODUCTION_BACKUP_TEST_FAIL case=${testCase.name} ` +
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
else console.log(`W08_PRODUCTION_BACKUP_TEST_PASS cases=${cases.length}`);
