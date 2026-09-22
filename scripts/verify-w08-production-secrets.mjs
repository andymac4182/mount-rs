#!/usr/bin/env node

// Credential-free P02 secret/IAM policy. This validates references, rotation
// and audit requirements only; it never reads, resolves or prints secrets.

import { lstat, readFile } from "node:fs/promises";

const configPath = process.argv[2];
const REQUIRED_REFERENCES = [
  "MOUNT_RS_TIDB_TLS_URL",
  "R2_ACCESS_KEY_ID",
  "R2_SECRET_ACCESS_KEY",
];
const MAX_ROTATION_DAYS = 90;

function fail(reason) {
  console.error(`W08_PRODUCTION_SECRETS_POLICY_FAIL reason=${reason}`);
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

function rejectInlineSecretValues(value, path = "config") {
  if (Array.isArray(value)) {
    value.forEach((item, index) => rejectInlineSecretValues(item, `${path}[${index}]`));
    return;
  }
  if (!isObject(value)) return;

  for (const [key, child] of Object.entries(value)) {
    if (/(?:password|secret|token|private[_-]?key|credential|value)/iu.test(key)) {
      if (typeof child === "string") fail(`${path}.${key}-must-not-be-inline`);
    }
    rejectInlineSecretValues(child, `${path}.${key}`);
  }
}

if (!configPath || process.argv.length !== 3) {
  console.error("usage: verify-w08-production-secrets.mjs <secrets.json>");
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
  if (error?.message?.startsWith("W08_PRODUCTION_SECRETS_POLICY_FAIL")) throw error;
  fail("config-unreadable-or-invalid-json");
}

rejectInlineSecretValues(config);
const root = requireObject(config, "config");
rejectUnknown(root, ["version", "environment_class", "secret_manager"], "config");
if (root.version !== 1) fail("config.version-must-be-1");
requireEnum(
  root.environment_class,
  "environment_class",
  ["production-like-staging", "production"],
);

const manager = requireObject(root.secret_manager, "config.secret_manager");
rejectUnknown(
  manager,
  ["kind", "identity_mode", "secret_refs", "rotation", "audit", "break_glass"],
  "config.secret_manager",
);
requireEnum(manager.kind, "secret_manager.kind", ["external"]);
requireEnum(
  manager.identity_mode,
  "secret_manager.identity_mode",
  ["workload-identity", "managed-identity"],
);

if (!Array.isArray(manager.secret_refs)) {
  fail("secret_manager.secret_refs-must-be-array");
}
const names = manager.secret_refs.map((reference, index) => {
  const object = requireObject(reference, `secret_manager.secret_refs[${index}]`);
  rejectUnknown(
    object,
    ["name", "source"],
    `secret_manager.secret_refs[${index}]`,
  );
  const name = requireString(object.name, `secret_manager.secret_refs[${index}].name`);
  requireEnum(
    object.source,
    `secret_manager.secret_refs[${index}].source`,
    ["external-secret-manager"],
  );
  return name;
});
if (new Set(names).size !== names.length) {
  fail("secret_manager.secret_refs-must-be-unique");
}
for (const required of REQUIRED_REFERENCES) {
  if (!names.includes(required)) {
    fail(`secret_manager.secret_refs-must-contain-${required}`);
  }
}
if (names.length !== REQUIRED_REFERENCES.length) {
  fail("secret_manager.secret_refs-must-contain-exact-production-references");
}

const rotation = requireObject(manager.rotation, "secret_manager.rotation");
rejectUnknown(
  rotation,
  ["max_age_days", "overlap_days", "revoke_on_failure"],
  "secret_manager.rotation",
);
const maxAgeDays = requireSafeInteger(
  rotation.max_age_days,
  "secret_manager.rotation.max_age_days",
  1,
);
if (maxAgeDays > MAX_ROTATION_DAYS) {
  fail(`secret_manager.rotation.max_age_days-must-be-at-most-${MAX_ROTATION_DAYS}`);
}
const overlapDays = requireSafeInteger(
  rotation.overlap_days,
  "secret_manager.rotation.overlap_days",
  1,
);
if (overlapDays >= maxAgeDays) {
  fail("secret_manager.rotation.overlap_days-must-be-less-than-max-age");
}
requireBoolean(rotation.revoke_on_failure, "secret_manager.rotation.revoke_on_failure", true);

const audit = requireObject(manager.audit, "secret_manager.audit");
rejectUnknown(
  audit,
  ["redaction_required", "access_log_required", "retention_days"],
  "secret_manager.audit",
);
requireBoolean(audit.redaction_required, "secret_manager.audit.redaction_required", true);
requireBoolean(audit.access_log_required, "secret_manager.audit.access_log_required", true);
requireSafeInteger(audit.retention_days, "secret_manager.audit.retention_days", 30);

const breakGlass = requireObject(manager.break_glass, "secret_manager.break_glass");
rejectUnknown(breakGlass, ["procedure_ref", "two_person"], "secret_manager.break_glass");
const procedureRef = requireString(
  breakGlass.procedure_ref,
  "secret_manager.break_glass.procedure_ref",
);
if (!procedureRef.startsWith("docs/")) {
  fail("secret_manager.break_glass.procedure_ref-must-reference-docs");
}
requireBoolean(breakGlass.two_person, "secret_manager.break_glass.two_person", true);

console.log(
  "W08_PRODUCTION_SECRETS_POLICY_PASS " +
    `kind=${manager.kind} identity=${manager.identity_mode} refs=${names.length} ` +
    `rotation_max_days=${maxAgeDays} audit=redacted retention_days=${audit.retention_days} ` +
    "break_glass=two-person",
);
