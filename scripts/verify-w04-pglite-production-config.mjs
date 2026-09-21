#!/usr/bin/env node

// Credential-free policy gate for the W04 PGlite-only launch contract.
// This validates configuration shape only; it never resolves credentials,
// opens PGlite, or probes a provider or filesystem.

import { lstat, readFile } from "node:fs/promises";
import path from "node:path";

const configPath = process.argv[2];
const MAX_LEASE_TTL_MS = 24 * 60 * 60 * 1000;

function fail(reason) {
  console.error(`W04_PGLITE_PRODUCTION_CONFIG_POLICY_FAIL reason=${reason}`);
  process.exit(1);
}

function isObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function requireObject(value, field) {
  if (!isObject(value)) fail(`${field}-must-be-object`);
  return value;
}

function requireString(value, field) {
  if (typeof value !== "string" || value.length === 0) {
    fail(`${field}-must-be-non-empty-string`);
  }
  if (/[\u0000-\u001f\u007f<>]/u.test(value)) {
    fail(`${field}-contains-control-or-placeholder`);
  }
  return value;
}

function requireBoolean(value, field, expected) {
  if (value !== expected) fail(`${field}-must-be-${expected}`);
}

function requireEnvRef(value, field) {
  const reference = requireObject(value, field);
  if (
    Object.keys(reference).length !== 1 ||
    reference.env !== "MOUNT_RS_PGLITE_URL"
  ) {
    fail(`${field}-must-reference-MOUNT_RS_PGLITE_URL`);
  }
}

function requireScopedKey(value, field) {
  const key = requireString(value, field);
  const segments = key.split("/");
  if (
    key.startsWith("/") ||
    key.includes("\\") ||
    segments.some((segment) => segment === "" || segment === "." || segment === "..")
  ) {
    fail(`${field}-must-be-a-non-root-scoped-key`);
  }
}

function requireAbsoluteNormalizedPath(value, field) {
  const resolved = requireString(value, field);
  if (
    !path.isAbsolute(resolved) ||
    resolved.split(path.sep).some((segment) => segment === "..") ||
    path.normalize(resolved) !== resolved
  ) {
    fail(`${field}-must-be-an-absolute-normalized-path`);
  }
}

function rejectUnknown(object, allowed, field) {
  for (const key of Object.keys(object)) {
    if (!allowed.includes(key)) fail(`${field}.${key}-is-not-supported`);
  }
}

function rejectInlineSecrets(value, field = "config") {
  if (Array.isArray(value)) {
    value.forEach((item, index) => rejectInlineSecrets(item, `${field}[${index}]`));
    return;
  }
  if (!isObject(value)) return;

  for (const [key, child] of Object.entries(value)) {
    if (/(?:password|secret|token|private[_-]?key|credential)/iu.test(key)) {
      if (typeof child === "string") fail(`${field}.${key}-must-not-be-inline`);
    }
    rejectInlineSecrets(child, `${field}.${key}`);
  }
}

function validatePgliteProvider(value, field) {
  const provider = requireObject(value, field);
  rejectUnknown(provider, ["kind", "connection", "volume_key", "durable"], field);
  if (provider.kind !== "pglite") fail(`${field}.kind-must-be-pglite`);
  requireEnvRef(provider.connection, `${field}.connection`);
  requireScopedKey(provider.volume_key, `${field}.volume_key`);
  requireBoolean(provider.durable, `${field}.durable`, true);
  return provider.volume_key;
}

if (!configPath || process.argv.length !== 3) {
  console.error(
    "usage: verify-w04-pglite-production-config.mjs <config.json>",
  );
  process.exit(2);
}

let config;
try {
  const metadata = await lstat(configPath);
  if (!metadata.isFile() || metadata.isSymbolicLink()) {
    fail("config-must-be-regular-file");
  }
  config = JSON.parse(await readFile(configPath, "utf8"));
} catch (error) {
  if (error?.message?.startsWith("W04_PGLITE_PRODUCTION_CONFIG_POLICY_FAIL")) {
    throw error;
  }
  fail("config-unreadable-or-invalid-json");
}

rejectInlineSecrets(config);
const root = requireObject(config, "config");
rejectUnknown(
  root,
  ["version", "mountpoint", "transport", "driver", "read_only"],
  "config",
);
if (root.version !== 1) fail("config.version-must-be-1");
requireAbsoluteNormalizedPath(root.mountpoint, "config.mountpoint");
if (root.transport !== undefined && !["auto", "fuse"].includes(root.transport)) {
  fail("config.transport-must-be-auto-or-fuse");
}
if (root.read_only === true) fail("config.read_only-must-be-false-or-omitted");

const driver = requireObject(root.driver, "config.driver");
rejectUnknown(driver, ["kind", "storage"], "config.driver");
if (driver.kind !== "splitstore") {
  fail("config.driver.kind-must-be-splitstore");
}

const storage = requireObject(driver.storage, "config.driver.storage");
rejectUnknown(
  storage,
  ["metadata", "blocks", "chunk_size_bytes", "lease_ttl_ms", "owner"],
  "config.driver.storage",
);
const metadataKey = validatePgliteProvider(
  storage.metadata,
  "config.driver.storage.metadata",
);
const blocksKey = validatePgliteProvider(
  storage.blocks,
  "config.driver.storage.blocks",
);
if (metadataKey === blocksKey) {
  fail("config.driver.storage.metadata.volume_key-must-differ-from-blocks");
}

if (
  !Number.isSafeInteger(storage.chunk_size_bytes) ||
  storage.chunk_size_bytes <= 0 ||
  storage.chunk_size_bytes > 64 * 1024 * 1024
) {
  fail(
    "config.driver.storage.chunk_size_bytes-must-be-positive-safe-integer-at-most-64MiB",
  );
}
if (
  storage.lease_ttl_ms !== undefined &&
  (!Number.isSafeInteger(storage.lease_ttl_ms) ||
    storage.lease_ttl_ms <= 0 ||
    storage.lease_ttl_ms > MAX_LEASE_TTL_MS)
) {
  fail(
    "config.driver.storage.lease_ttl_ms-must-be-positive-safe-integer-at-most-24h",
  );
}
requireString(storage.owner, "config.driver.storage.owner");

console.log(
  "W04_PGLITE_PRODUCTION_CONFIG_POLICY_PASS " +
    "scope=pglite-only metadata=pglite blocks=pglite " +
    "durable_metadata=true durable_blocks=true secrets=external " +
    "mountpoint=absolute chunk_size=bounded lease_ttl=bounded",
);
