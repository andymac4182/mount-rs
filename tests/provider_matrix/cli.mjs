#!/usr/bin/env node

// CLI consumer checks. Config validation is intentionally mount-free: it
// proves the public schema and provider selection without opening a database,
// resolving credentials, or contacting PGlite/R2.

import { spawn } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const matrixDirectory = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = resolve(matrixDirectory, "../..");
const configFiles = [
  ["memory-config", resolve(matrixDirectory, "config-memory.json")],
  ["sqlite-config", resolve(matrixDirectory, "config-sqlite.json")],
  ["pglite-r2-config", resolve(matrixDirectory, "config-pglite-r2.json")],
];
const failures = [];
let passes = 0;
let skips = 0;

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
