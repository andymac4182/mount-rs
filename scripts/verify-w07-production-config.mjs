#!/usr/bin/env node

// Credential-free policy gate for the W07 production deployment contract.
// This validates configuration shape and transport/authority policy only; it
// never opens FoundationDB or RustFS and never prints supplied secret values.

import { lstat, readFile } from "node:fs/promises";
import path from "node:path";
import { URL } from "node:url";

const configPath = process.argv[2];
const MAX_LEASE_TTL_MS = 24 * 60 * 60 * 1000;

function fail(reason) {
  console.error(`W07_PRODUCTION_CONFIG_POLICY_FAIL reason=${reason}`);
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
  if (/[<>]/u.test(value)) fail(`${field}-contains-placeholder`);
  return value;
}

function requireBoolean(value, field, expected) {
  if (value !== expected) fail(`${field}-must-be-${expected}`);
}

function requireEnvRef(value, field, expectedName) {
  const reference = requireObject(value, field);
  if (Object.keys(reference).length !== 1 || reference.env !== expectedName) {
    fail(`${field}-must-reference-${expectedName}`);
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

if (!configPath || process.argv.length !== 3) {
  console.error("usage: verify-w07-production-config.mjs <config.json>");
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
  if (error?.message?.startsWith("W07_PRODUCTION_CONFIG_POLICY_FAIL")) throw error;
  fail("config-unreadable-or-invalid-json");
}

rejectInlineSecrets(config);
const root = requireObject(config, "config");
if (root.version !== 1) fail("config.version-must-be-1");

const driver = requireObject(root.driver, "config.driver");
if (driver.kind !== "splitstore") fail("config.driver.kind-must-be-splitstore");
const storage = requireObject(driver.storage, "config.driver.storage");

const metadata = requireObject(storage.metadata, "config.driver.storage.metadata");
if (metadata.kind !== "foundationdb") {
  fail("metadata.kind-must-be-foundationdb");
}
const clusterFile = requireString(
  metadata.cluster_file,
  "metadata.cluster_file",
);
if (!path.isAbsolute(clusterFile)) {
  fail("metadata.cluster_file-must-be-absolute");
}
requireString(metadata.volume_key, "metadata.volume_key");
requireBoolean(metadata.durable, "metadata.durable", true);
if (metadata.lease_authority !== "shared-provider") {
  fail("metadata.lease_authority-must-be-shared-provider");
}
requireString(metadata.authority_prefix, "metadata.authority_prefix");

const blocks = requireObject(storage.blocks, "config.driver.storage.blocks");
if (blocks.kind !== "r2") fail("blocks.kind-must-be-r2");
const endpoint = requireString(blocks.endpoint, "blocks.endpoint");
let endpointUrl;
try {
  endpointUrl = new URL(endpoint);
} catch {
  fail("blocks.endpoint-must-be-valid-https-url");
}
if (
  endpointUrl.protocol !== "https:" ||
  !endpointUrl.hostname ||
  endpointUrl.username ||
  endpointUrl.password ||
  endpointUrl.search ||
  endpointUrl.hash
) {
  fail("blocks.endpoint-must-be-valid-https-url");
}
requireString(blocks.bucket, "blocks.bucket");
requireString(blocks.prefix, "blocks.prefix");
requireEnvRef(blocks.access_key_id, "blocks.access_key_id", "R2_ACCESS_KEY_ID");
requireEnvRef(
  blocks.secret_access_key,
  "blocks.secret_access_key",
  "R2_SECRET_ACCESS_KEY",
);
requireBoolean(blocks.durable, "blocks.durable", true);

if (
  !Number.isSafeInteger(storage.chunk_size_bytes) ||
  storage.chunk_size_bytes <= 0
) {
  fail("storage.chunk_size_bytes-must-be-positive-safe-integer");
}
if (storage.lease_ttl_ms === undefined) {
  fail("storage.lease_ttl_ms-is-required-for-production");
}
if (
  !Number.isSafeInteger(storage.lease_ttl_ms) ||
  storage.lease_ttl_ms <= 0 ||
  storage.lease_ttl_ms > MAX_LEASE_TTL_MS
) {
  fail(
    "storage.lease_ttl_ms-must-be-positive-safe-integer-at-most-24h",
  );
}
requireString(storage.owner, "storage.owner");

console.log(
  "W07_PRODUCTION_CONFIG_POLICY_PASS " +
    "config_shape=splitstore metadata=foundationdb " +
    "durable_metadata=true durable_blocks=true " +
    "lease_authority=shared-provider lease_ttl=explicit-bounded " +
    "blocks_tls=https secrets=external",
);
