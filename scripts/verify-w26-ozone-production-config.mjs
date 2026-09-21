#!/usr/bin/env node

// Credential-free policy gate for the W26 customer-deployed Ozone contract.
// It validates configuration shape and transport/security policy only; it
// never opens a provider connection and never prints supplied secret values.

import { lstat, readFile } from "node:fs/promises";
import path from "node:path";
import { URL } from "node:url";

const configPath = process.argv[2];

function fail(reason) {
  console.error(`W26_OZONE_PRODUCTION_CONFIG_POLICY_FAIL reason=${reason}`);
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

function requireEnvRef(value, field, expectedName) {
  const reference = requireObject(value, field);
  if (Object.keys(reference).length !== 1 || reference.env !== expectedName) {
    fail(`${field}-must-reference-${expectedName}`);
  }
}

function requireAbsolutePath(value, field) {
  const resolved = requireString(value, field);
  if (!path.isAbsolute(resolved) || resolved.split(path.sep).includes("..")) {
    fail(`${field}-must-be-an-absolute-normalized-path`);
  }
}

function requireScopedPrefix(value, field) {
  const prefix = requireString(value, field);
  const segments = prefix.split("/");
  if (
    prefix.startsWith("/") ||
    prefix.includes("\\") ||
    segments.some((segment) => segment === "" || segment === "." || segment === "..")
  ) {
    fail(`${field}-must-be-a-non-root-scoped-prefix`);
  }
}

function requireHttpsEndpoint(value, field) {
  const endpoint = requireString(value, field);
  let parsed;
  try {
    parsed = new URL(endpoint);
  } catch {
    fail(`${field}-must-be-valid-https-url`);
  }
  if (
    parsed.protocol !== "https:" ||
    !parsed.hostname ||
    parsed.username ||
    parsed.password ||
    parsed.search ||
    parsed.hash
  ) {
    fail(`${field}-must-be-valid-https-url`);
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

function requireProviderFields(metadata, allowed, field) {
  for (const key of Object.keys(metadata)) {
    if (!allowed.includes(key)) fail(`${field}.${key}-is-not-supported`);
  }
}

function validateTidbTlsEnvironment() {
  const tlsUrl = process.env.MOUNT_RS_TIDB_TLS_URL;
  if (!tlsUrl) fail("MOUNT_RS_TIDB_TLS_URL-must-be-supplied-out-of-band");
  if (/[\u0000-\u001f\u007f<>]/u.test(tlsUrl)) {
    fail("MOUNT_RS_TIDB_TLS_URL-contains-control-or-placeholder");
  }

  let parsed;
  try {
    parsed = new URL(tlsUrl);
  } catch {
    fail("MOUNT_RS_TIDB_TLS_URL-must-be-valid-mysql-url");
  }
  if (parsed.protocol !== "mysql:" || !parsed.hostname || parsed.password) {
    fail("MOUNT_RS_TIDB_TLS_URL-must-be-valid-mysql-url-without-inline-password");
  }
  if (parsed.searchParams.get("require_ssl") !== "true") {
    fail("MOUNT_RS_TIDB_TLS_URL-requires-require_ssl=true");
  }
  for (const option of ["verify_ca", "verify_identity", "built_in_roots"]) {
    const value = parsed.searchParams.get(option);
    if (value === "false" || value === "0") {
      fail(`MOUNT_RS_TIDB_TLS_URL-rejects-${option}`);
    }
  }
}

if (!configPath || process.argv.length !== 3) {
  console.error("usage: verify-w26-ozone-production-config.mjs <config.json>");
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
  if (error?.message?.startsWith("W26_OZONE_PRODUCTION_CONFIG_POLICY_FAIL")) {
    throw error;
  }
  fail("config-unreadable-or-invalid-json");
}

rejectInlineSecrets(config);
const root = requireObject(config, "config");
if (root.version !== 1) fail("config.version-must-be-1");

const driver = requireObject(root.driver, "config.driver");
if (driver.kind !== "splitstore") fail("config.driver.kind-must-be-splitstore");
const storage = requireObject(driver.storage, "config.driver.storage");
const metadata = requireObject(storage.metadata, "config.driver.storage.metadata");
const metadataKind = requireString(metadata.kind, "metadata.kind");

switch (metadataKind) {
  case "sqlite":
    requireProviderFields(metadata, ["kind", "path"], "metadata");
    requireAbsolutePath(metadata.path, "metadata.path");
    break;
  case "pglite":
    requireProviderFields(
      metadata,
      ["kind", "connection", "volume_key", "durable"],
      "metadata",
    );
    requireEnvRef(metadata.connection, "metadata.connection", "MOUNT_RS_PGLITE_URL");
    requireString(metadata.volume_key, "metadata.volume_key");
    requireBoolean(metadata.durable, "metadata.durable", true);
    break;
  case "tidb":
    requireProviderFields(
      metadata,
      ["kind", "connection", "volume_key", "durable"],
      "metadata",
    );
    requireEnvRef(metadata.connection, "metadata.connection", "MOUNT_RS_TIDB_TLS_URL");
    requireString(metadata.volume_key, "metadata.volume_key");
    requireBoolean(metadata.durable, "metadata.durable", true);
    validateTidbTlsEnvironment();
    break;
  case "foundationdb":
    requireProviderFields(
      metadata,
      [
        "kind",
        "cluster_file",
        "volume_key",
        "durable",
        "lease_authority",
        "authority_prefix",
      ],
      "metadata",
    );
    requireAbsolutePath(metadata.cluster_file, "metadata.cluster_file");
    requireString(metadata.volume_key, "metadata.volume_key");
    requireBoolean(metadata.durable, "metadata.durable", true);
    if (metadata.lease_authority !== "shared-provider") {
      fail("metadata.lease_authority-must-be-shared-provider");
    }
    requireScopedPrefix(metadata.authority_prefix, "metadata.authority_prefix");
    break;
  default:
    fail(`metadata.kind-unsupported-for-production=${metadataKind}`);
}

const blocks = requireObject(storage.blocks, "config.driver.storage.blocks");
requireProviderFields(
  blocks,
  [
    "kind",
    "endpoint",
    "bucket",
    "prefix",
    "access_key_id",
    "secret_access_key",
    "durable",
  ],
  "blocks",
);
if (blocks.kind !== "r2") fail("blocks.kind-must-be-r2");
requireHttpsEndpoint(blocks.endpoint, "blocks.endpoint");
requireString(blocks.bucket, "blocks.bucket");
requireScopedPrefix(blocks.prefix, "blocks.prefix");
requireEnvRef(blocks.access_key_id, "blocks.access_key_id", "R2_ACCESS_KEY_ID");
requireEnvRef(
  blocks.secret_access_key,
  "blocks.secret_access_key",
  "R2_SECRET_ACCESS_KEY",
);
requireBoolean(blocks.durable, "blocks.durable", true);

if (
  !Number.isSafeInteger(storage.chunk_size_bytes) ||
  storage.chunk_size_bytes <= 0 ||
  storage.chunk_size_bytes > 64 * 1024 * 1024
) {
  fail("storage.chunk_size_bytes-must-be-positive-safe-integer-at-most-64MiB");
}
requireString(storage.owner, "storage.owner");

console.log(
  "W26_OZONE_PRODUCTION_CONFIG_POLICY_PASS " +
    `metadata=${metadataKind} blocks=r2 blocks_tls=https ` +
    "durable_blocks=true secrets=external",
);
