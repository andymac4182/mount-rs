#!/usr/bin/env node

// Credential-free P07 security/transport policy. This validates hardening
// contract shape only; it never performs a TLS handshake or contacts a
// provider, network, scanner, identity system or audit sink.

import { lstat, readFile } from "node:fs/promises";

const configPath = process.argv[2];
const REQUIRED_CERT_VALIDATION = ["ca-chain", "hostname"];
const REQUIRED_NETWORK_ZONES = ["metadata", "block", "client"];
const REQUIRED_AUDIT_EVENTS = ["authz-deny", "secret-access", "admin-change", "tenant-boundary"];

function fail(reason) {
  console.error(`W08_PRODUCTION_SECURITY_POLICY_FAIL reason=${reason}`);
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

function requireExactStringArray(value, path, expected) {
  if (!Array.isArray(value) || value.length !== expected.length) {
    fail(`${path}-must-match-required-set`);
  }
  value.forEach((item, index) => requireString(item, `${path}[${index}]`));
  if (new Set(value).size !== value.length || expected.some((item) => !value.includes(item))) {
    fail(`${path}-must-match-required-set`);
  }
  return value;
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
  console.error("usage: verify-w08-production-security.mjs <security-policy.json>");
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
  if (error?.message?.startsWith("W08_PRODUCTION_SECURITY_POLICY_FAIL")) {
    throw error;
  }
  fail("config-unreadable-or-invalid-json");
}

rejectInlineSecrets(config);
const root = requireObject(config, "config");
rejectUnknown(
  root,
  ["version", "environment_class", "transport", "network", "authorization", "supply_chain", "threat_model", "audit", "handshake", "ownership"],
  "config",
);
if (root.version !== 1) fail("config.version-must-be-1");
requireEnum(
  root.environment_class,
  "environment_class",
  ["production-like-staging", "production"],
);

const transport = requireObject(root.transport, "config.transport");
rejectUnknown(
  transport,
  ["tls_required", "min_tls_version", "certificate_validation", "rotation_max_days", "rotation_overlap_required", "revocation_on_failure"],
  "config.transport",
);
requireBoolean(transport.tls_required, "transport.tls_required", true);
requireEnum(transport.min_tls_version, "transport.min_tls_version", ["1.3"]);
requireExactStringArray(transport.certificate_validation, "transport.certificate_validation", REQUIRED_CERT_VALIDATION);
const rotationMaxDays = requireSafeInteger(transport.rotation_max_days, "transport.rotation_max_days", 1);
if (rotationMaxDays > 90) fail("transport.rotation_max_days-must-be-at-most-90");
requireBoolean(transport.rotation_overlap_required, "transport.rotation_overlap_required", true);
requireBoolean(transport.revocation_on_failure, "transport.revocation_on_failure", true);

const network = requireObject(root.network, "config.network");
rejectUnknown(
  network,
  ["private_only", "zones", "ingress_default_deny", "egress_default_deny", "encryption_in_transit"],
  "config.network",
);
requireBoolean(network.private_only, "network.private_only", true);
requireExactStringArray(network.zones, "network.zones", REQUIRED_NETWORK_ZONES);
requireBoolean(network.ingress_default_deny, "network.ingress_default_deny", true);
requireBoolean(network.egress_default_deny, "network.egress_default_deny", true);
requireBoolean(network.encryption_in_transit, "network.encryption_in_transit", true);

const authorization = requireObject(root.authorization, "config.authorization");
rejectUnknown(
  authorization,
  ["identity_mode", "tenant_isolation", "least_privilege", "admin_separation", "break_glass_two_person"],
  "config.authorization",
);
requireEnum(authorization.identity_mode, "authorization.identity_mode", ["workload-identity"]);
requireEnum(authorization.tenant_isolation, "authorization.tenant_isolation", ["resource-and-prefix"]);
requireBoolean(authorization.least_privilege, "authorization.least_privilege", true);
requireBoolean(authorization.admin_separation, "authorization.admin_separation", true);
requireBoolean(authorization.break_glass_two_person, "authorization.break_glass_two_person", true);

const supplyChain = requireObject(root.supply_chain, "config.supply_chain");
rejectUnknown(
  supplyChain,
  ["dependency_scan_required", "image_scan_required", "sbom_required", "provenance_required", "vulnerability_policy", "scan_retention_days"],
  "config.supply_chain",
);
requireBoolean(supplyChain.dependency_scan_required, "supply_chain.dependency_scan_required", true);
requireBoolean(supplyChain.image_scan_required, "supply_chain.image_scan_required", true);
requireBoolean(supplyChain.sbom_required, "supply_chain.sbom_required", true);
requireBoolean(supplyChain.provenance_required, "supply_chain.provenance_required", true);
requireEnum(supplyChain.vulnerability_policy, "supply_chain.vulnerability_policy", ["fail-on-critical"]);
requireSafeInteger(supplyChain.scan_retention_days, "supply_chain.scan_retention_days", 90);

const threatModel = requireObject(root.threat_model, "config.threat_model");
rejectUnknown(
  threatModel,
  ["required", "review_status", "review_ref", "open_findings_need_plan", "owner_ref"],
  "config.threat_model",
);
requireBoolean(threatModel.required, "threat_model.required", true);
requireEnum(threatModel.review_status, "threat_model.review_status", ["required-before-go"]);
const reviewRef = requireString(threatModel.review_ref, "threat_model.review_ref");
if (!reviewRef.startsWith("docs/")) fail("threat_model.review_ref-must-reference-docs");
requireBoolean(threatModel.open_findings_need_plan, "threat_model.open_findings_need_plan", true);
requireString(threatModel.owner_ref, "threat_model.owner_ref");

const audit = requireObject(root.audit, "config.audit");
rejectUnknown(
  audit,
  ["enabled", "events", "retention_days", "redaction_required", "immutable"],
  "config.audit",
);
requireBoolean(audit.enabled, "audit.enabled", true);
requireExactStringArray(audit.events, "audit.events", REQUIRED_AUDIT_EVENTS);
requireSafeInteger(audit.retention_days, "audit.retention_days", 90);
requireBoolean(audit.redaction_required, "audit.redaction_required", true);
requireBoolean(audit.immutable, "audit.immutable", true);

const handshake = requireObject(root.handshake, "config.handshake");
rejectUnknown(
  handshake,
  ["credentialed_required", "endpoint_ref", "ca_ref", "hostname_verification", "cleanup_required"],
  "config.handshake",
);
requireBoolean(handshake.credentialed_required, "handshake.credentialed_required", true);
if (requireString(handshake.endpoint_ref, "handshake.endpoint_ref") !== "MOUNT_RS_TIDB_TLS_URL") {
  fail("handshake.endpoint_ref-must-be-MOUNT_RS_TIDB_TLS_URL");
}
if (requireString(handshake.ca_ref, "handshake.ca_ref") !== "MOUNT_RS_TIDB_CA") {
  fail("handshake.ca_ref-must-be-MOUNT_RS_TIDB_CA");
}
requireBoolean(handshake.hostname_verification, "handshake.hostname_verification", true);
requireBoolean(handshake.cleanup_required, "handshake.cleanup_required", true);

const ownership = requireObject(root.ownership, "config.ownership");
rejectUnknown(
  ownership,
  ["security_owner", "network_owner", "platform_owner"],
  "config.ownership",
);
for (const [key, value] of Object.entries(ownership)) {
  requireString(value, `ownership.${key}`);
}

console.log(
  "W08_PRODUCTION_SECURITY_POLICY_PASS " +
    `tls=${transport.min_tls_version} rotation_max_days=${rotationMaxDays} ` +
    `zones=${network.zones.length} identity=${authorization.identity_mode} ` +
    `scan_retention_days=${supplyChain.scan_retention_days} ` +
    `audit_retention_days=${audit.retention_days} handshake=credentialed`,
);
