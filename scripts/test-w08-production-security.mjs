#!/usr/bin/env node

// Exercise the W08 P07 security/transport policy with temporary fixtures.
// These are credential-free shape tests; they never contact a provider or
// security service.

import { spawnSync } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { tmpdir } from "node:os";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const verifierPath = join(repoRoot, "scripts/verify-w08-production-security.mjs");
const sourcePath = join(repoRoot, "tests/tidb/production-security-policy.json");
const source = JSON.parse(await readFile(sourcePath, "utf8"));

const cases = [
  {
    name: "security-baseline",
    expectedStatus: 0,
    expectedOutput: "W08_PRODUCTION_SECURITY_POLICY_PASS tls=1.3",
    config: source,
  },
  {
    name: "tls-version-required",
    expectedStatus: 1,
    expectedOutput: "reason=transport.min_tls_version-must-be-one-of-1.3",
    config: { ...source, transport: { ...source.transport, min_tls_version: "1.2" } },
  },
  {
    name: "network-default-deny",
    expectedStatus: 1,
    expectedOutput: "reason=network.ingress_default_deny-must-be-true",
    config: { ...source, network: { ...source.network, ingress_default_deny: false } },
  },
  {
    name: "tenant-isolation-required",
    expectedStatus: 1,
    expectedOutput: "reason=authorization.tenant_isolation-must-be-one-of-resource-and-prefix",
    config: { ...source, authorization: { ...source.authorization, tenant_isolation: "network-only" } },
  },
  {
    name: "critical-vulnerability-policy",
    expectedStatus: 1,
    expectedOutput: "reason=supply_chain.vulnerability_policy-must-be-one-of-fail-on-critical",
    config: { ...source, supply_chain: { ...source.supply_chain, vulnerability_policy: "report-only" } },
  },
  {
    name: "threat-review-before-go",
    expectedStatus: 1,
    expectedOutput: "reason=threat_model.review_status-must-be-one-of-required-before-go",
    config: { ...source, threat_model: { ...source.threat_model, review_status: "approved" } },
  },
  {
    name: "audit-redaction-required",
    expectedStatus: 1,
    expectedOutput: "reason=audit.redaction_required-must-be-true",
    config: { ...source, audit: { ...source.audit, redaction_required: false } },
  },
  {
    name: "credentialed-handshake-required",
    expectedStatus: 1,
    expectedOutput: "reason=handshake.hostname_verification-must-be-true",
    config: { ...source, handshake: { ...source.handshake, hostname_verification: false } },
  },
];

const sandbox = await mkdtemp(join(tmpdir(), "mount-rs-w08-production-security-"));
let failed = false;
try {
  for (const testCase of cases) {
    const caseDirectory = join(sandbox, testCase.name);
    await mkdir(caseDirectory);
    const configPath = join(caseDirectory, "security.json");
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
        `W08_PRODUCTION_SECURITY_TEST_FAIL case=${testCase.name} ` +
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
else console.log(`W08_PRODUCTION_SECURITY_TEST_PASS cases=${cases.length}`);
