#!/usr/bin/env node

// Exercise the W08 P02 secret/IAM policy with temporary fixtures. These are
// credential-free shape tests; they do not contact a secret manager or provider.

import { spawnSync } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { tmpdir } from "node:os";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const verifierPath = join(repoRoot, "scripts/verify-w08-production-secrets.mjs");
const sourcePath = join(repoRoot, "tests/tidb/production-secrets-policy.json");
const source = JSON.parse(await readFile(sourcePath, "utf8"));

const cases = [
  {
    name: "external-rotation-baseline",
    expectedStatus: 0,
    expectedOutput: "W08_PRODUCTION_SECRETS_POLICY_PASS kind=external",
    config: source,
  },
  {
    name: "inline-secret-value",
    expectedStatus: 1,
    expectedOutput: "reason=config.secret_manager.secret_refs[0].value-must-not-be-inline",
    config: {
      ...source,
      secret_manager: {
        ...source.secret_manager,
        secret_refs: [
          { ...source.secret_manager.secret_refs[0], value: "do-not-commit" },
          ...source.secret_manager.secret_refs.slice(1),
        ],
      },
    },
  },
  {
    name: "missing-tidb-reference",
    expectedStatus: 1,
    expectedOutput: "reason=secret_manager.secret_refs-must-contain-MOUNT_RS_TIDB_TLS_URL",
    config: {
      ...source,
      secret_manager: {
        ...source.secret_manager,
        secret_refs: source.secret_manager.secret_refs.slice(1),
      },
    },
  },
  {
    name: "duplicate-reference",
    expectedStatus: 1,
    expectedOutput: "reason=secret_manager.secret_refs-must-be-unique",
    config: {
      ...source,
      secret_manager: {
        ...source.secret_manager,
        secret_refs: [
          ...source.secret_manager.secret_refs,
          { ...source.secret_manager.secret_refs[0] },
        ],
      },
    },
  },
  {
    name: "static-identity-is-rejected",
    expectedStatus: 1,
    expectedOutput: "reason=secret_manager.identity_mode-must-be-one-of-workload-identity-managed-identity",
    config: {
      ...source,
      secret_manager: { ...source.secret_manager, identity_mode: "static-key" },
    },
  },
  {
    name: "rotation-too-old",
    expectedStatus: 1,
    expectedOutput: "reason=secret_manager.rotation.max_age_days-must-be-at-most-90",
    config: {
      ...source,
      secret_manager: {
        ...source.secret_manager,
        rotation: { ...source.secret_manager.rotation, max_age_days: 120 },
      },
    },
  },
  {
    name: "rotation-overlap-invalid",
    expectedStatus: 1,
    expectedOutput: "reason=secret_manager.rotation.overlap_days-must-be-less-than-max-age",
    config: {
      ...source,
      secret_manager: {
        ...source.secret_manager,
        rotation: { ...source.secret_manager.rotation, overlap_days: 90 },
      },
    },
  },
  {
    name: "audit-redaction-required",
    expectedStatus: 1,
    expectedOutput: "reason=secret_manager.audit.redaction_required-must-be-true",
    config: {
      ...source,
      secret_manager: {
        ...source.secret_manager,
        audit: { ...source.secret_manager.audit, redaction_required: false },
      },
    },
  },
];

const sandbox = await mkdtemp(join(tmpdir(), "mount-rs-w08-production-secrets-"));
let failed = false;
try {
  for (const testCase of cases) {
    const caseDirectory = join(sandbox, testCase.name);
    await mkdir(caseDirectory);
    const configPath = join(caseDirectory, "secrets.json");
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
        `W08_PRODUCTION_SECRETS_TEST_FAIL case=${testCase.name} ` +
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
else console.log(`W08_PRODUCTION_SECRETS_TEST_PASS cases=${cases.length}`);
