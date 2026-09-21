#!/usr/bin/env node

// Credential-free policy gate for the W08 production deployment contract.
// This validates configuration shape and TLS policy only; it never opens a
// provider connection and never prints values supplied through the environment.

import { lstat, readFile } from "node:fs/promises";
import { URL } from "node:url";

const configPath = process.argv[2];

function fail(reason) {
  console.error(`W08_PRODUCTION_CONFIG_POLICY_FAIL reason=${reason}`);
  process.exit(1);
}

function isObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function requireObject(value, path) {
  if (!isObject(value)) fail(`${path}-must-be-object`);
  return value;
}

function requireString(value, path) {
  if (typeof value !== "string" || value.length === 0) {
    fail(`${path}-must-be-non-empty-string`);
  }
  if (/[<>]/u.test(value)) fail(`${path}-contains-placeholder`);
  return value;
}

function requireBoolean(value, path, expected) {
  if (value !== expected) fail(`${path}-must-be-${expected}`);
}

function requireEnvRef(value, path, expectedName) {
  const reference = requireObject(value, path);
  if (Object.keys(reference).length !== 1 || reference.env !== expectedName) {
    fail(`${path}-must-reference-${expectedName}`);
  }
}

function rejectInlineSecrets(value, path = "config") {
  if (Array.isArray(value)) {
    value.forEach((item, index) => rejectInlineSecrets(item, `${path}[${index}]`));
    return;
  }
  if (!isObject(value)) return;

  for (const [key, child] of Object.entries(value)) {
    if (/(?:password|secret|token|private[_-]?key|credential)/iu.test(key)) {
      if (typeof child === "string") fail(`${path}.${key}-must-not-be-inline`);
    }
    rejectInlineSecrets(child, `${path}.${key}`);
  }
}

if (!configPath || process.argv.length !== 3) {
  console.error("usage: verify-w08-production-config.mjs <config.json>");
  process.exit(2);
}

let config;
try {
  const metadata = await lstat(configPath);
  if (!metadata.isFile() || metadata.isSymbolicLink()) fail("config-must-be-regular-file");
  config = JSON.parse(await readFile(configPath, "utf8"));
} catch (error) {
  if (error?.message?.startsWith("W08_PRODUCTION_CONFIG_POLICY_FAIL")) throw error;
  fail("config-unreadable-or-invalid-json");
}

rejectInlineSecrets(config);
const root = requireObject(config, "config");
if (root.version !== 1) fail("config.version-must-be-1");

const driver = requireObject(root.driver, "config.driver");
if (driver.kind !== "splitstore") fail("config.driver.kind-must-be-splitstore");
const storage = requireObject(driver.storage, "config.driver.storage");

const metadata = requireObject(storage.metadata, "config.driver.storage.metadata");
if (metadata.kind !== "tidb") fail("metadata.kind-must-be-tidb");
requireEnvRef(metadata.connection, "metadata.connection", "MOUNT_RS_TIDB_TLS_URL");
requireString(metadata.volume_key, "metadata.volume_key");
requireBoolean(metadata.durable, "metadata.durable", true);

const blocks = requireObject(storage.blocks, "config.driver.storage.blocks");
if (blocks.kind !== "r2") fail("blocks.kind-must-be-r2");
const endpoint = requireString(blocks.endpoint, "blocks.endpoint");
let endpointUrl;
try {
  endpointUrl = new URL(endpoint);
} catch {
  fail("blocks.endpoint-must-be-valid-https-url");
}
if (endpointUrl.protocol !== "https:" || !endpointUrl.hostname) {
  fail("blocks.endpoint-must-be-valid-https-url");
}
requireString(blocks.bucket, "blocks.bucket");
requireString(blocks.prefix, "blocks.prefix");
requireEnvRef(blocks.access_key_id, "blocks.access_key_id", "R2_ACCESS_KEY_ID");
requireEnvRef(blocks.secret_access_key, "blocks.secret_access_key", "R2_SECRET_ACCESS_KEY");
requireBoolean(blocks.durable, "blocks.durable", true);

if (!Number.isSafeInteger(storage.chunk_size_bytes) || storage.chunk_size_bytes <= 0) {
  fail("storage.chunk_size_bytes-must-be-positive-safe-integer");
}
requireString(storage.owner, "storage.owner");

const tlsUrl = process.env.MOUNT_RS_TIDB_TLS_URL;
if (!tlsUrl) fail("MOUNT_RS_TIDB_TLS_URL-must-be-supplied-out-of-band");
if (/[<>]/u.test(tlsUrl)) fail("MOUNT_RS_TIDB_TLS_URL-contains-placeholder");

let parsedTlsUrl;
try {
  parsedTlsUrl = new URL(tlsUrl);
} catch {
  fail("MOUNT_RS_TIDB_TLS_URL-must-be-valid-mysql-url");
}
if (parsedTlsUrl.protocol !== "mysql:" || !parsedTlsUrl.hostname) {
  fail("MOUNT_RS_TIDB_TLS_URL-must-be-valid-mysql-url");
}
if (parsedTlsUrl.searchParams.get("require_ssl") !== "true") {
  fail("MOUNT_RS_TIDB_TLS_URL-requires-require_ssl=true");
}
for (const option of ["verify_ca", "verify_identity", "built_in_roots"]) {
  const value = parsedTlsUrl.searchParams.get(option);
  if (value === "false" || value === "0") fail(`MOUNT_RS_TIDB_TLS_URL-rejects-${option}`);
}

console.log(
  "W08_PRODUCTION_CONFIG_POLICY_PASS " +
    "config_shape=splitstore durable_metadata=true durable_blocks=true " +
    "blocks_tls=https tidb_tls=require_ssl secrets=external",
);
