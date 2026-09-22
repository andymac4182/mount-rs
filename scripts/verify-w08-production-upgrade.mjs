#!/usr/bin/env node

// Credential-free P04 upgrade/compatibility/rollback policy. This validates a
// rehearsal contract only; it never mutates a provider or performs a rollback.

import { lstat, readFile } from "node:fs/promises";

const configPath = process.argv[2];
const MIN_ARTIFACT_RETENTION_DAYS = 30;
const REQUIRED_CLIENT_SURFACES = ["rust-sdk", "cli", "napi", "fuse", "nfs"];
const REQUIRED_SIGNOFF_ROLES = ["service-owner", "release-owner"];

function fail(reason) {
  console.error(`W08_PRODUCTION_UPGRADE_POLICY_FAIL reason=${reason}`);
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

function parseVersion(value, path) {
  const version = requireString(value, path);
  const match = /^v(\d+)\.(\d+)\.(\d+)$/u.exec(version);
  if (!match) fail(`${path}-must-be-pinned-semver`);
  return match.slice(1).map(Number);
}

function compareVersions(left, right) {
  for (let index = 0; index < left.length; index += 1) {
    if (left[index] !== right[index]) return left[index] - right[index];
  }
  return 0;
}

if (!configPath || process.argv.length !== 3) {
  console.error("usage: verify-w08-production-upgrade.mjs <upgrade-policy.json>");
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
  if (error?.message?.startsWith("W08_PRODUCTION_UPGRADE_POLICY_FAIL")) throw error;
  fail("config-unreadable-or-invalid-json");
}

rejectInlineSecrets(config);
const root = requireObject(config, "config");
rejectUnknown(
  root,
  ["version", "environment_class", "identity_mode", "versions", "clients", "migration", "upgrade", "rollback", "signoff"],
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

const versions = requireObject(root.versions, "config.versions");
rejectUnknown(versions, ["current", "previous"], "config.versions");
const currentVersionParts = parseVersion(versions.current, "versions.current");
const previousVersionParts = parseVersion(versions.previous, "versions.previous");
if (compareVersions(currentVersionParts, previousVersionParts) <= 0) {
  fail("versions.current-must-be-newer-than-previous");
}
if (
  currentVersionParts[0] !== previousVersionParts[0] ||
  currentVersionParts[1] !== previousVersionParts[1]
) {
  fail("versions.current-and-previous-must-share-major-minor");
}

const clients = requireObject(root.clients, "config.clients");
rejectUnknown(clients, ["surfaces", "compatibility", "matrix_rehearsed"], "config.clients");
const surfaces = requireStringArray(clients.surfaces, "clients.surfaces");
if (new Set(surfaces).size !== surfaces.length) fail("clients.surfaces-must-be-unique");
for (const surface of REQUIRED_CLIENT_SURFACES) {
  if (!surfaces.includes(surface)) fail(`clients.surfaces-must-contain-${surface}`);
}
if (surfaces.length !== REQUIRED_CLIENT_SURFACES.length) {
  fail("clients.surfaces-must-contain-only-supported-surfaces");
}
requireEnum(clients.compatibility, "clients.compatibility", ["wire-and-data-read"]);
requireBoolean(clients.matrix_rehearsed, "clients.matrix_rehearsed", true);

const migration = requireObject(root.migration, "config.migration");
rejectUnknown(
  migration,
  ["schema_strategy", "config_strategy", "forward_compatible", "backward_compatible", "interrupted_recovery"],
  "config.migration",
);
requireEnum(migration.schema_strategy, "migration.schema_strategy", ["expand-contract"]);
requireEnum(migration.config_strategy, "migration.config_strategy", ["versioned"]);
requireBoolean(migration.forward_compatible, "migration.forward_compatible", true);
requireBoolean(migration.backward_compatible, "migration.backward_compatible", true);
requireBoolean(migration.interrupted_recovery, "migration.interrupted_recovery", true);

const upgrade = requireObject(root.upgrade, "config.upgrade");
rejectUnknown(
  upgrade,
  ["strategy", "quorum_preserved", "health_gate_required", "maintenance_abort"],
  "config.upgrade",
);
requireEnum(upgrade.strategy, "upgrade.strategy", ["rolling"]);
requireBoolean(upgrade.quorum_preserved, "upgrade.quorum_preserved", true);
requireBoolean(upgrade.health_gate_required, "upgrade.health_gate_required", true);
requireBoolean(upgrade.maintenance_abort, "upgrade.maintenance_abort", true);

const rollback = requireObject(root.rollback, "config.rollback");
rejectUnknown(
  rollback,
  ["artifact_retention_days", "config_retained", "data_compatibility_check", "writer_fencing", "fresh_client_readback", "rehearsal_required"],
  "config.rollback",
);
const artifactRetentionDays = requireSafeInteger(
  rollback.artifact_retention_days,
  "rollback.artifact_retention_days",
  MIN_ARTIFACT_RETENTION_DAYS,
);
requireBoolean(rollback.config_retained, "rollback.config_retained", true);
requireBoolean(rollback.data_compatibility_check, "rollback.data_compatibility_check", true);
requireBoolean(rollback.writer_fencing, "rollback.writer_fencing", true);
requireBoolean(rollback.fresh_client_readback, "rollback.fresh_client_readback", true);
requireBoolean(rollback.rehearsal_required, "rollback.rehearsal_required", true);

const signoff = requireObject(root.signoff, "config.signoff");
rejectUnknown(signoff, ["runbook_ref", "roles"], "config.signoff");
const runbookRef = requireString(signoff.runbook_ref, "signoff.runbook_ref");
if (!runbookRef.startsWith("docs/")) fail("signoff.runbook_ref-must-reference-docs");
const roles = requireStringArray(signoff.roles, "signoff.roles");
if (new Set(roles).size !== roles.length) fail("signoff.roles-must-be-unique");
for (const role of REQUIRED_SIGNOFF_ROLES) {
  if (!roles.includes(role)) fail(`signoff.roles-must-contain-${role}`);
}
if (roles.length !== REQUIRED_SIGNOFF_ROLES.length) {
  fail("signoff.roles-must-contain-only-required-roles");
}

console.log(
  "W08_PRODUCTION_UPGRADE_POLICY_PASS " +
    `current=${versions.current} previous=${versions.previous} surfaces=${surfaces.length} ` +
    `migration=expand-contract rollback_retention_days=${artifactRetentionDays} ` +
    "signoff=service-owner,release-owner",
);
