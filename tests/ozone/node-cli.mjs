#!/usr/bin/env node

// Live Node CLI coverage for the Ozone split-store configuration. The parent
// harness supplies the real Ozone endpoint and disk-backed PGlite server.

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { cleanupR2Prefix, r2ConfigFromEnv } from "../provider_matrix/r2-cleanup.mjs";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const cliPath = resolve(repositoryRoot, "examples/node-cli/index.mjs");
const fixturePath = resolve(
  repositoryRoot,
  "apps/mount-rs-cli/examples/config-pglite-ozone.json",
);
const required = [
  "PGLITE_DATABASE_URL",
  "R2_ENDPOINT",
  "R2_BUCKET",
  "R2_ACCESS_KEY_ID",
  "R2_SECRET_ACCESS_KEY",
];
for (const name of required) {
  assert.ok(process.env[name], `${name} is required for live Ozone Node CLI coverage`);
}

const r2 = r2ConfigFromEnv();
assert.equal(r2.missing.length, 0, `missing Ozone S3 configuration: ${r2.missing.join(",")}`);
const runId = process.env.MOUNT_RS_PROVIDER_MATRIX_RUN_ID ?? String(process.pid);
assert.match(runId, /^[A-Za-z0-9._-]+$/);
const prefix = `mount-rs-provider-matrix/${runId}/node-cli-ozone`;
const metadataKey = `${prefix}/metadata`;

function runCli(args) {
  return new Promise((resolvePromise, rejectPromise) => {
    const child = spawn(process.execPath, [cliPath, ...args], {
      cwd: repositoryRoot,
      env: process.env,
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk) => { stdout += chunk; });
    child.stderr.on("data", (chunk) => { stderr += chunk; });
    child.once("error", rejectPromise);
    child.once("close", (code, signal) => resolvePromise({ code, signal, stdout, stderr }));
  });
}

const configDirectory = await mkdtemp(join(tmpdir(), "mount-rs-ozone-node-cli-"));
const configPath = join(configDirectory, "config.json");
try {
  const config = JSON.parse(await readFile(fixturePath, "utf8"));
  config.driver.storage.metadata.connection = { env: "PGLITE_DATABASE_URL" };
  config.driver.storage.metadata.volume_key = metadataKey;
  config.driver.storage.owner = `ozone-node-cli-${process.pid}`;
  config.driver.storage.blocks.endpoint = r2.config.endpoint;
  config.driver.storage.blocks.bucket = r2.config.bucket;
  config.driver.storage.blocks.prefix = prefix;
  config.driver.storage.blocks.access_key_id = { env: "R2_ACCESS_KEY_ID" };
  config.driver.storage.blocks.secret_access_key = { env: "R2_SECRET_ACCESS_KEY" };
  await writeFile(configPath, `${JSON.stringify(config, null, 2)}\n`, { mode: 0o600 });

  const result = await runCli(["--config", configPath, "--sdk-self-test", "--reopen"]);
  const output = `${result.stdout}\n${result.stderr}`;
  assert.equal(result.code, 0, output);
  assert.equal(result.signal, null, output);
  assert.match(
    result.stdout,
    /sdk self-test passed: Node SDK wrote, shut down, reopened, and read/,
  );
  assert.doesNotMatch(output, /missing environment variable/);
  console.log(`OZONE_NODE_CLI_PASS prefix=${prefix}`);
} finally {
  await cleanupR2Prefix(r2.config, prefix);
  await rm(configDirectory, { recursive: true, force: true });
}
