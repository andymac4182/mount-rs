#!/usr/bin/env node

// Exercise the W08 P01 topology policy with temporary fixtures. These are
// credential-free shape tests; they do not contact providers or Docker.

import { spawnSync } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { tmpdir } from "node:os";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const verifierPath = join(repoRoot, "scripts/verify-w08-production-topology.mjs");
const sourcePath = join(repoRoot, "tests/tidb/production-topology-policy.json");
const source = JSON.parse(await readFile(sourcePath, "utf8"));

const cases = [
  {
    name: "replicated-durable-baseline",
    expectedStatus: 0,
    expectedOutput: "W08_PRODUCTION_TOPOLOGY_POLICY_PASS class=replicated-durable",
    config: source,
  },
  {
    name: "single-node-is-not-production",
    expectedStatus: 1,
    expectedOutput: "reason=topology.class-must-be-one-of-replicated-durable",
    config: { ...source, topology: { ...source.topology, class: "single-node-smoke" } },
  },
  {
    name: "pd-replica-floor",
    expectedStatus: 1,
    expectedOutput: "reason=topology.pd_replicas-must-be-safe-integer-at-least-3",
    config: { ...source, topology: { ...source.topology, pd_replicas: 1 } },
  },
  {
    name: "quorum-cannot-be-single-node",
    expectedStatus: 1,
    expectedOutput: "reason=topology.quorum.pd_min_available-must-be-safe-integer-at-least-2",
    config: {
      ...source,
      topology: {
        ...source.topology,
        quorum: { ...source.topology.quorum, pd_min_available: 1 },
      },
    },
  },
  {
    name: "blocks-require-tls",
    expectedStatus: 1,
    expectedOutput: "reason=blocks.tls-must-be-true",
    config: { ...source, blocks: { ...source.blocks, tls: false } },
  },
  {
    name: "durable-capacity-floor",
    expectedStatus: 1,
    expectedOutput: "reason=resources.min_memory_gib-must-be-safe-integer-at-least-10",
    config: { ...source, resources: { ...source.resources, min_memory_gib: 8 } },
  },
  {
    name: "versions-must-be-coherent",
    expectedStatus: 1,
    expectedOutput: "reason=versions-must-be-coherent",
    config: { ...source, versions: { ...source.versions, tikv: "v8.5.6" } },
  },
  {
    name: "unknown-policy-field",
    expectedStatus: 1,
    expectedOutput: "reason=config.topology.unreviewed-is-unknown",
    config: {
      ...source,
      topology: { ...source.topology, unreviewed: true },
    },
  },
];

const sandbox = await mkdtemp(join(tmpdir(), "mount-rs-w08-production-topology-"));
let failed = false;
try {
  for (const testCase of cases) {
    const caseDirectory = join(sandbox, testCase.name);
    await mkdir(caseDirectory);
    const configPath = join(caseDirectory, "topology.json");
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
        `W08_PRODUCTION_TOPOLOGY_TEST_FAIL case=${testCase.name} ` +
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
else console.log(`W08_PRODUCTION_TOPOLOGY_TEST_PASS cases=${cases.length}`);
