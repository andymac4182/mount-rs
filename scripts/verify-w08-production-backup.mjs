#!/usr/bin/env node

// Credential-free P03 backup/restore/DR policy. This validates the shape of a
// recovery contract only; it never contacts TiDB, an object provider, a key
// manager or a second region.

import { lstat, readFile } from "node:fs/promises";

const configPath = process.argv[2];
const MIN_RETENTION_DAYS = 30;
const MAX_RPO_MINUTES = 60;
const MAX_RTO_MINUTES = 240;
const REQUIRED_SIGNOFF_ROLES = ["data-owner", "release-owner"];

function fail(reason) {
  console.error(`W08_PRODUCTION_BACKUP_POLICY_FAIL reason=${reason}`);
  process.exit(1);
}

function isObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function requireObject(value, path) {
  if (!isObject(value)) fail(`${path}-must-be-object`);
  return value;
}

function rejectUnknown(object, allowed, path) {
  for (const key of Object.keys(object)) {
    if (!allowed.includes(key)) fail(`${path}.${key}-is-unknown`);
  }
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

function requireSafeInteger(value, path, minimum) {
  if (!Number.isSafeInteger(value) || value < minimum) {
    fail(`${path}-must-be-safe-integer-at-least-${minimum}`);
  }
  return value;
}

function requireEnum(value, path, allowed) {
  if (!allowed.includes(value)) {
    fail(`${path}-must-be-one-of-${allowed.join("-")}`);
  }
  return value;
}

function requireStringArray(value, path) {
  if (!Array.isArray(value) || value.length === 0) {
    fail(`${path}-must-be-non-empty-array`);
  }
  value.forEach((item, index) => requireString(item, `${path}[${index}]`));
  return value;
}

function rejectInlineSecrets(value, path = "config") {
  if (Array.isArray(value)) {
    value.forEach((item, index) => rejectInlineSecrets(item, `${path}[${index}]`));
    return;
  }
  if (!isObject(value)) return;

  for (const [key, child] of Object.entries(value)) {
    if (/(?:password|secret|token|private[_-]?key|credential|value)/iu.test(key)) {
      if (typeof child === "string") fail(`${path}.${key}-must-not-be-inline`);
    }
    rejectInlineSecrets(child, `${path}.${key}`);
  }
}

if (!configPath || process.argv.length !== 3) {
  console.error("usage: verify-w08-production-backup.mjs <backup-policy.json>");
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
  if (error?.message?.startsWith("W08_PRODUCTION_BACKUP_POLICY_FAIL")) throw error;
  fail("config-unreadable-or-invalid-json");
}

rejectInlineSecrets(config);
const root = requireObject(config, "config");
rejectUnknown(
  root,
  ["version", "environment_class", "identity_mode", "metadata", "blocks", "restore", "dr"],
  "config",
);
if (root.version !== 1) fail("config.version-must-be-1");
requireEnum(
  root.environment_class,
  "environment_class",
  ["production-like-staging", "production"],
);
requireEnum(
  root.identity_mode,
  "identity_mode",
  ["workload-identity", "managed-identity"],
);

const metadata = requireObject(root.metadata, "config.metadata");
rejectUnknown(
  metadata,
  ["method", "consistency", "revision_capture", "retention_days", "encryption", "key_source"],
  "config.metadata",
);
requireEnum(metadata.method, "metadata.method", ["tidb-br"]);
requireEnum(
  metadata.consistency,
  "metadata.consistency",
  ["transactional-snapshot"],
);
requireBoolean(metadata.revision_capture, "metadata.revision_capture", true);
const metadataRetentionDays = requireSafeInteger(
  metadata.retention_days,
  "metadata.retention_days",
  MIN_RETENTION_DAYS,
);
requireEnum(metadata.encryption, "metadata.encryption", ["customer-managed-key"]);
requireEnum(
  metadata.key_source,
  "metadata.key_source",
  ["external-secret-manager"],
);

const blocks = requireObject(root.blocks, "config.blocks");
rejectUnknown(
  blocks,
  ["versioning", "immutability", "retention_days", "encryption", "integrity", "prefix_isolation"],
  "config.blocks",
);
requireBoolean(blocks.versioning, "blocks.versioning", true);
requireEnum(blocks.immutability, "blocks.immutability", ["object-lock"]);
const blocksRetentionDays = requireSafeInteger(
  blocks.retention_days,
  "blocks.retention_days",
  MIN_RETENTION_DAYS,
);
if (blocksRetentionDays < metadataRetentionDays) {
  fail("blocks.retention_days-must-cover-metadata-retention");
}
requireEnum(blocks.encryption, "blocks.encryption", ["customer-managed-key"]);
requireEnum(blocks.integrity, "blocks.integrity", ["sha256-manifest"]);
requireEnum(blocks.prefix_isolation, "blocks.prefix_isolation", ["tenant-volume"]);

const restore = requireObject(root.restore, "config.restore");
rejectUnknown(
  restore,
  [
    "isolated_environment",
    "separate_identity",
    "production_writer_access",
    "metadata_block_consistency",
    "fresh_client_readback",
    "corruption_case",
    "partial_object_case",
    "region_loss_case",
    "rpo_minutes",
    "rto_minutes",
  ],
  "config.restore",
);
requireBoolean(restore.isolated_environment, "restore.isolated_environment", true);
requireBoolean(restore.separate_identity, "restore.separate_identity", true);
requireBoolean(restore.production_writer_access, "restore.production_writer_access", false);
requireBoolean(restore.metadata_block_consistency, "restore.metadata_block_consistency", true);
requireBoolean(restore.fresh_client_readback, "restore.fresh_client_readback", true);
requireBoolean(restore.corruption_case, "restore.corruption_case", true);
requireBoolean(restore.partial_object_case, "restore.partial_object_case", true);
requireBoolean(restore.region_loss_case, "restore.region_loss_case", true);
const rpoMinutes = requireSafeInteger(restore.rpo_minutes, "restore.rpo_minutes", 1);
if (rpoMinutes > MAX_RPO_MINUTES) {
  fail(`restore.rpo_minutes-must-be-at-most-${MAX_RPO_MINUTES}`);
}
const rtoMinutes = requireSafeInteger(restore.rto_minutes, "restore.rto_minutes", 1);
if (rtoMinutes > MAX_RTO_MINUTES) {
  fail(`restore.rto_minutes-must-be-at-most-${MAX_RTO_MINUTES}`);
}
if (rtoMinutes < rpoMinutes) fail("restore.rto_minutes-must-cover-rpo");

const dr = requireObject(root.dr, "config.dr");
rejectUnknown(dr, ["second_region", "runbook_ref", "signoff_roles"], "config.dr");
requireBoolean(dr.second_region, "dr.second_region", true);
const runbookRef = requireString(dr.runbook_ref, "dr.runbook_ref");
if (!runbookRef.startsWith("docs/")) {
  fail("dr.runbook_ref-must-reference-docs");
}
const signoffRoles = requireStringArray(dr.signoff_roles, "dr.signoff_roles");
if (new Set(signoffRoles).size !== signoffRoles.length) {
  fail("dr.signoff_roles-must-be-unique");
}
for (const role of REQUIRED_SIGNOFF_ROLES) {
  if (!signoffRoles.includes(role)) {
    fail(`dr.signoff_roles-must-contain-${role}`);
  }
}
if (signoffRoles.length !== REQUIRED_SIGNOFF_ROLES.length) {
  fail("dr.signoff_roles-must-contain-only-required-roles");
}

console.log(
  "W08_PRODUCTION_BACKUP_POLICY_PASS " +
    `metadata=transactional-snapshot blocks=immutable-versioned ` +
    `retention_days=${blocksRetentionDays} rpo_minutes=${rpoMinutes} ` +
    `rto_minutes=${rtoMinutes} regions=2 integrity=sha256-manifest restore_cases=3`,
);
