#!/usr/bin/env node

// CLI consumer checks. The config-validation rows are intentionally mount-free:
// they prove the public schema and provider selection without opening a
// database, resolving credentials, or contacting PGlite/R2. The opt-in
// PGlite row below is the separate configured SDK read/write/reopen exercise.

import { spawn } from "node:child_process";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import {
  cleanupR2Prefix,
  listR2Prefix,
  rustfsConfigFromEnv,
} from "./r2-cleanup.mjs";

const matrixDirectory = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = resolve(matrixDirectory, "../..");
const configFiles = [
  ["memory-config", resolve(matrixDirectory, "config-memory.json")],
  ["sqlite-config", resolve(matrixDirectory, "config-sqlite.json")],
  ["pglite-r2-config", resolve(matrixDirectory, "config-pglite-r2.json")],
  ["tidb-rustfs-config", resolve(matrixDirectory, "config-tidb-rustfs.json")],
];
const failures = [];
let passes = 0;
let skips = 0;
const providerRunId = process.env.MOUNT_RS_PROVIDER_MATRIX_RUN_ID || `pid-${process.pid}`;

function run(command, args, timeoutMs = 120_000) {
  return new Promise((resolveResult) => {
    const child = spawn(command, args, {
      cwd: repositoryRoot,
      env: { ...process.env },
      stdio: ["ignore", "pipe", "pipe"],
    });
    let timedOut = false;
    const timer = setTimeout(() => {
      timedOut = true;
      child.kill("SIGTERM");
    }, timeoutMs);
    child.on("error", () => {
      clearTimeout(timer);
      resolveResult({ ok: false, reason: "spawn" });
    });
    child.on("close", (code) => {
      clearTimeout(timer);
      resolveResult({
        ok: !timedOut && code === 0,
        reason: timedOut ? "timeout" : "exit-" + String(code),
      });
    });
  });
}

async function commandCase(label, command, args) {
  const result = await run(command, args);
  if (result.ok) {
    passes += 1;
    console.log("PASS cli case=" + label);
  } else {
    failures.push(label);
    console.log("FAIL cli case=" + label + " reason=" + result.reason);
  }
}

await commandCase(
  "node-sdk-self-test",
  process.execPath,
  ["examples/node-cli/index.mjs", "--driver", "memory", "--sdk-self-test"],
);

// Run the actual Rust CLI binary through its public-SDK self-test command.
// The command is mount-free, so this remains portable while proving that the
// process-level CLI—not only private runtime tests—constructs a Rust SDK
// filesystem and performs driver I/O.
await commandCase(
  "rust-cli-sdk-self-test",
  "cargo",
  [
    "run",
    "--quiet",
    "--offline",
    "--locked",
    "-p",
    "mount-rs-cli",
    "--",
    "sdk-self-test",
  ],
);

const rustConfigDirectory = await mkdtemp(join(tmpdir(), "mount-rs-rust-cli-provider-"));
const rustConfigPath = join(rustConfigDirectory, "splitstore.json");
await writeFile(
  rustConfigPath,
  `${JSON.stringify({
    version: 1,
    driver: {
      kind: "splitstore",
      storage: {
        metadata: { kind: "sqlite", path: "./metadata.sqlite" },
        blocks: { kind: "sqlite", path: "./blocks.sqlite" },
        chunk_size_bytes: 7,
        owner: `provider-matrix-rust-cli-${providerRunId}`,
      },
    },
  }, null, 2)}\n`,
  { mode: 0o600 },
);
await commandCase(
  "node-cli-sdk-self-test-sqlite-reopen",
  process.execPath,
  [
    "examples/node-cli/index.mjs",
    "--config",
    rustConfigPath,
    "--sdk-self-test",
    "--reopen",
  ],
);
await commandCase(
  "rust-cli-sdk-self-test-sqlite-reopen",
  "cargo",
  [
    "run",
    "--quiet",
    "--offline",
    "--locked",
    "-p",
    "mount-rs-cli",
    "--",
    "sdk-self-test",
    "--config",
    rustConfigPath,
    "--reopen",
  ],
);
await rm(rustConfigDirectory, { recursive: true, force: true });

// Exercise the actual Node CLI's config-to-SDK path when the PGlite harness is
// running. The config deliberately has no mountpoint: this is a portable,
// mount-free consumer test that still opens the configured public SDK driver,
// writes and reads a file, shuts down, reopens, and reads the committed bytes.
if (process.env.PGLITE_DATABASE_URL) {
  const configDirectory = await mkdtemp(join(tmpdir(), "mount-rs-cli-provider-"));
  const configPath = join(configDirectory, "pglite.json");
  const config = {
    version: 1,
    driver: {
      kind: "splitstore",
      storage: {
        metadata: {
          kind: "pglite",
          connection: { env: "PGLITE_DATABASE_URL" },
          volume_key: `provider-matrix-cli-${providerRunId}-metadata`,
          durable: false,
        },
        blocks: {
          kind: "pglite",
          connection: { env: "PGLITE_DATABASE_URL" },
          volume_key: `provider-matrix-cli-${providerRunId}-blocks`,
          durable: false,
        },
        chunk_size_bytes: 7,
        owner: `provider-matrix-cli-${providerRunId}`,
      },
    },
  };
  await writeFile(configPath, `${JSON.stringify(config, null, 2)}\n`, { mode: 0o600 });
  await commandCase(
    "node-cli-pglite-config-reopen",
    process.execPath,
    [
      "examples/node-cli/index.mjs",
      "--config",
      configPath,
      "--sdk-self-test",
      "--reopen",
    ],
  );
  await rm(configDirectory, { recursive: true, force: true });

  // Exercise the matching Rust CLI path against the same live PGlite socket.
  // This is deliberately a process-level SDK consumer check, not merely
  // `validate-config`: the command writes, shuts down both providers, reopens
  // them through the public Rust SDK facade, and verifies the committed bytes.
  const rustPgliteDirectory = await mkdtemp(join(tmpdir(), "mount-rs-rust-cli-pglite-"));
  const rustPgliteConfigPath = join(rustPgliteDirectory, "pglite.json");
  await writeFile(
    rustPgliteConfigPath,
    `${JSON.stringify({
      version: 1,
      driver: {
        kind: "splitstore",
        storage: {
          metadata: {
            kind: "pglite",
            connection: { env: "PGLITE_DATABASE_URL" },
            volume_key: `provider-matrix-rust-cli-${providerRunId}-metadata`,
            durable: false,
          },
          blocks: {
            kind: "pglite",
            connection: { env: "PGLITE_DATABASE_URL" },
            volume_key: `provider-matrix-rust-cli-${providerRunId}-blocks`,
            durable: false,
          },
          chunk_size_bytes: 7,
          owner: `provider-matrix-rust-cli-${providerRunId}`,
        },
      },
    }, null, 2)}\n`,
    { mode: 0o600 },
  );
  await commandCase(
    "rust-cli-pglite-config-reopen",
    "cargo",
    [
      "run",
      "--quiet",
      "--offline",
      "--locked",
      "-p",
      "mount-rs-cli",
      "--",
      "sdk-self-test",
      "--config",
      rustPgliteConfigPath,
      "--reopen",
    ],
  );
  await rm(rustPgliteDirectory, { recursive: true, force: true });
} else {
  skips += 1;
  console.log("SKIP cli case=node-cli-pglite-config-reopen gate=PGLITE_DATABASE_URL");
}

const tidbUrl = process.env.MOUNT_RS_TIDB_URL;
const rustfs = rustfsConfigFromEnv();
const tidbRustfsReady = Boolean(tidbUrl) && rustfs.missing.length === 0;
if (tidbRustfsReady) {
  const configDirectory = await mkdtemp(join(tmpdir(), "mount-rs-cli-tidb-rustfs-"));
  const configPath = join(configDirectory, "tidb-rustfs.json");
  const prefix = `mount-rs-provider-matrix/${providerRunId}/cli-tidb-rustfs`;
  const volumeKey = `mount-rs-provider-matrix/${providerRunId}/cli-tidb-rustfs-metadata`;
  const protectedKeys = await listR2Prefix(rustfs.config, prefix);
  const config = {
    version: 1,
    driver: {
      kind: "splitstore",
      storage: {
        metadata: {
          kind: "tidb",
          connection: { env: "MOUNT_RS_TIDB_URL" },
          volume_key: volumeKey,
          durable: true,
        },
        blocks: {
          kind: "r2",
          endpoint: rustfs.config.endpoint,
          bucket: rustfs.config.bucket,
          prefix,
          access_key_id: { env: "R2_ACCESS_KEY_ID" },
          secret_access_key: { env: "R2_SECRET_ACCESS_KEY" },
          durable: true,
        },
        chunk_size_bytes: 7,
        owner: `provider-matrix-cli-${providerRunId}-tidb-rustfs`,
      },
    },
  };
  try {
    await writeFile(configPath, `${JSON.stringify(config, null, 2)}\n`, { mode: 0o600 });
    await commandCase(
      "node-cli-tidb-rustfs-config-reopen-partial-truncate",
      process.execPath,
      ["examples/node-cli/index.mjs", "--config", configPath, "--sdk-self-test", "--reopen"],
    );
    await commandCase(
      "rust-cli-tidb-rustfs-config-reopen-partial-truncate",
      "cargo",
      [
        "run",
        "--quiet",
        "--offline",
        "--locked",
        "-p",
        "mount-rs-cli",
        "--",
        "sdk-self-test",
        "--config",
        configPath,
        "--reopen",
      ],
    );
  } finally {
    try {
      await cleanupR2Prefix(rustfs.config, prefix, protectedKeys);
    } catch (error) {
      failures.push("tidb-rustfs-prefix-cleanup");
      console.log("FAIL cli case=tidb-rustfs-prefix-cleanup reason=" + error.message);
    }
    await rm(configDirectory, { recursive: true, force: true });
  }
} else {
  skips += 1;
  const gate = [
    tidbUrl ? undefined : "MOUNT_RS_TIDB_URL",
    ...rustfs.missing,
  ].filter(Boolean);
  console.log(
    "SKIP cli case=tidb-rustfs-config-reopen-partial-truncate gate=" +
      gate.join("|") +
      " reason=requires_actual_tidb_and_loopback_rustfs",
  );
}

for (const [label, path] of configFiles) {
  await commandCase(
    label,
    "cargo",
    [
      "run",
      "--quiet",
      "--offline",
      "--locked",
      "-p",
      "mount-rs-cli",
      "--",
      "validate-config",
      "--config",
      path,
    ],
  );
}

// These are the existing focused CLI runtime tests. They keep this matrix
// bounded while proving that the CLI's memory and SQLite provider consumers
// can open, write, read, and shut down a composed filesystem offline.
await commandCase(
  "memory-runtime",
  "cargo",
  [
    "test",
    "--quiet",
    "--offline",
    "--locked",
    "-p",
    "mount-rs-cli",
    "structured_memory_storage_is_a_live_provider_composition",
    "--",
    "--nocapture",
  ],
);
await commandCase(
  "sqlite-runtime",
  "cargo",
  [
    "test",
    "--quiet",
    "--offline",
    "--locked",
    "-p",
    "mount-rs-cli",
    "structured_sqlite_storage_is_a_live_provider_composition",
    "--",
    "--nocapture",
  ],
);

const hasPglite = Boolean(process.env.PGLITE_DATABASE_URL);
const r2Required = [
  "R2_ENDPOINT",
  "R2_BUCKET",
  "R2_ACCESS_KEY_ID",
  "R2_SECRET_ACCESS_KEY",
];
const missingR2 = r2Required.filter((name) => !process.env[name]);
skips += 1;
const cliGates = [];
if (!hasPglite) cliGates.push("PGLITE_DATABASE_URL");
if (missingR2.length > 0) cliGates.push(missingR2.join("|"));
if (cliGates.length === 0) cliGates.push("explicit-live-cli-opt-in");
console.log(
  "SKIP cli case=pglite-r2-runtime gate=" +
    cliGates.join("+") +
    " reason=matrix-keeps-cli-runtime-mount-free",
);

console.log(
  "SUMMARY cli pass=" +
    passes +
    " skip=" +
    skips +
    " fail=" +
    failures.length,
);
if (failures.length > 0) process.exit(1);
