#!/usr/bin/env node

// Credential-free P08 operations/readiness policy. This validates the
// runbook, drill and incident-control contract only; it never injects a
// failure, contacts a provider, pages an operator or records a PASS result.

import { lstat, readFile } from "node:fs/promises";

const configPath = process.argv[2];
const EXPECTED_DRILL_IDS = Array.from({ length: 9 }, (_, index) => `D0${index + 1}`);
const EXPECTED_GATE_REFS = {
  D01: ["P03", "P05", "P08"],
  D02: ["P01", "P03", "P08"],
  D03: ["P03", "P05", "P08"],
  D04: ["P02", "P08"],
  D05: ["P01", "P05", "P08"],
  D06: ["P03", "P08"],
  D07: ["P02", "P07", "P08"],
  D08: ["P03", "P08"],
  D09: ["P04", "P09"],
};
const REQUIRED_EVIDENCE_FIELDS = [
  "record_id",
  "tested_revision",
  "configuration",
  "provider_versions",
  "topology",
  "environment",
  "people",
  "time",
  "result",
];
const REQUIRED_TIMESTAMP_FIELDS = [
  "start",
  "detection",
  "acknowledgement",
  "mitigation",
  "recovery",
  "cleanup",
];
const REQUIRED_FAILURE_DOMAINS = [
  "metadata",
  "block-store",
  "client-transport",
  "credentials-tls",
  "network-partition",
  "capacity",
  "release-artifact",
];
const REQUIRED_INTEGRITY_CHECKS = [
  "fresh-client-readback",
  "lease-fence-state",
  "block-hash-manifest",
  "metadata-revision",
  "cleanup-boundary",
];

function fail(reason) {
  console.error(`W08_PRODUCTION_OPERATIONS_POLICY_FAIL reason=${reason}`);
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
  if (typeof value !== "string" || value.trim() === "") {
    fail(`${path}-must-be-non-empty-string`);
  }
  if (/(?:TBD|TODO|UNKNOWN|PENDING|CHANGEME|<[^>]+>|\{\{)/iu.test(value)) {
    fail(`${path}-contains-placeholder`);
  }
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

function requireStringArray(value, path, minimum) {
  if (!Array.isArray(value) || value.length < minimum) {
    fail(`${path}-must-have-at-least-${minimum}-items`);
  }
  value.forEach((item, index) => requireString(item, `${path}[${index}]`));
  if (new Set(value).size !== value.length) fail(`${path}-must-not-contain-duplicates`);
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
  console.error("usage: verify-w08-production-operations.mjs <operations-policy.json>");
  process.exit(2);
}

let config;
try {
  const metadata = await lstat(configPath);
  if (!metadata.isFile() || metadata.isSymbolicLink()) fail("config-must-be-regular-file");
  config = JSON.parse(await readFile(configPath, "utf8"));
} catch (error) {
  if (error?.message?.startsWith("W08_PRODUCTION_OPERATIONS_POLICY_FAIL")) throw error;
  fail("config-unreadable-or-invalid-json");
}

rejectInlineSecrets(config);
const root = requireObject(config, "config");
rejectUnknown(root, ["version", "environment_class", "runbook", "evidence_record", "incident", "drills", "on_call", "ownership"], "config");
if (root.version !== 1) fail("config.version-must-be-1");
if (root.environment_class !== "production-like-staging") {
  fail("environment_class-must-be-production-like-staging");
}

const runbook = requireObject(root.runbook, "config.runbook");
rejectUnknown(runbook, ["path", "status", "current_decision", "evidence_records_required", "terminal_status_required", "redacted_logs_required"], "config.runbook");
if (requireString(runbook.path, "runbook.path") !== "docs/W08-operations-runbook.md") fail("runbook.path-must-reference-operations-runbook");
if (runbook.status !== "controlled-template") fail("runbook.status-must-be-controlled-template");
if (runbook.current_decision !== "NO-GO") fail("runbook.current_decision-must-be-NO-GO");
requireBoolean(runbook.evidence_records_required, "runbook.evidence_records_required", true);
requireBoolean(runbook.terminal_status_required, "runbook.terminal_status_required", true);
requireBoolean(runbook.redacted_logs_required, "runbook.redacted_logs_required", true);

const evidence = requireObject(root.evidence_record, "config.evidence_record");
rejectUnknown(evidence, ["fields", "timestamp_fields", "terminal_results", "cleanup_outcome_required", "rollback_outcome_required"], "config.evidence_record");
requireExactStringArray(evidence.fields, "evidence_record.fields", REQUIRED_EVIDENCE_FIELDS);
requireExactStringArray(evidence.timestamp_fields, "evidence_record.timestamp_fields", REQUIRED_TIMESTAMP_FIELDS);
requireExactStringArray(evidence.terminal_results, "evidence_record.terminal_results", ["PASS", "FAIL"]);
requireBoolean(evidence.cleanup_outcome_required, "evidence_record.cleanup_outcome_required", true);
requireBoolean(evidence.rollback_outcome_required, "evidence_record.rollback_outcome_required", true);

const incident = requireObject(root.incident, "config.incident");
rejectUnknown(incident, ["incident_commander_required", "stop_writers_required", "preserve_evidence_required", "failure_domains", "integrity_checks", "closure_requires_fresh_client", "closure_requires_named_ack"], "config.incident");
requireBoolean(incident.incident_commander_required, "incident.incident_commander_required", true);
requireBoolean(incident.stop_writers_required, "incident.stop_writers_required", true);
requireBoolean(incident.preserve_evidence_required, "incident.preserve_evidence_required", true);
requireExactStringArray(incident.failure_domains, "incident.failure_domains", REQUIRED_FAILURE_DOMAINS);
requireExactStringArray(incident.integrity_checks, "incident.integrity_checks", REQUIRED_INTEGRITY_CHECKS);
requireBoolean(incident.closure_requires_fresh_client, "incident.closure_requires_fresh_client", true);
requireBoolean(incident.closure_requires_named_ack, "incident.closure_requires_named_ack", true);

if (!Array.isArray(root.drills) || root.drills.length !== EXPECTED_DRILL_IDS.length) {
  fail("drills-must-have-nine-entries");
}
const seenDrills = new Set();
for (const [index, rawDrill] of root.drills.entries()) {
  const drill = requireObject(rawDrill, `drills[${index}]`);
  rejectUnknown(drill, ["id", "failure", "signal", "recovery", "gate_refs", "cleanup_required", "rollback_required", "owner_role"], `drills[${index}]`);
  const id = requireString(drill.id, `drills[${index}].id`);
  if (!EXPECTED_DRILL_IDS.includes(id)) fail(`drills[${index}].id-must-be-known`);
  if (seenDrills.has(id)) fail(`drills.${id}-must-not-be-duplicated`);
  seenDrills.add(id);
  requireString(drill.failure, `drills.${id}.failure`);
  requireStringArray(drill.signal, `drills.${id}.signal`, 2);
  requireStringArray(drill.recovery, `drills.${id}.recovery`, 3);
  requireExactStringArray(drill.gate_refs, `drills.${id}.gate_refs`, EXPECTED_GATE_REFS[id]);
  requireBoolean(drill.cleanup_required, `drills.${id}.cleanup_required`, true);
  requireBoolean(drill.rollback_required, `drills.${id}.rollback_required`, true);
  requireString(drill.owner_role, `drills.${id}.owner_role`);
}
if (seenDrills.size !== EXPECTED_DRILL_IDS.length) fail("drills-must-cover-D01-to-D09");

const onCall = requireObject(root.on_call, "config.on_call");
rejectUnknown(onCall, ["primary_route_ref", "secondary_route_ref", "escalation_policy_ref", "coverage", "max_acknowledgement_minutes", "tabletop_required", "acknowledgement_record_required"], "config.on_call");
requireString(onCall.primary_route_ref, "on_call.primary_route_ref");
requireString(onCall.secondary_route_ref, "on_call.secondary_route_ref");
requireString(onCall.escalation_policy_ref, "on_call.escalation_policy_ref");
if (onCall.coverage !== "24x7") fail("on_call.coverage-must-be-24x7");
const maxAcknowledgement = requireSafeInteger(onCall.max_acknowledgement_minutes, "on_call.max_acknowledgement_minutes", 1);
if (maxAcknowledgement > 15) fail("on_call.max_acknowledgement_minutes-must-be-at-most-15");
requireBoolean(onCall.tabletop_required, "on_call.tabletop_required", true);
requireBoolean(onCall.acknowledgement_record_required, "on_call.acknowledgement_record_required", true);

const ownership = requireObject(root.ownership, "config.ownership");
rejectUnknown(ownership, ["incident_commander", "primary_on_call", "secondary_on_call", "operations_owner", "data_owner", "security_owner", "release_owner"], "config.ownership");
for (const [key, value] of Object.entries(ownership)) requireString(value, `ownership.${key}`);

console.log(
  `W08_PRODUCTION_OPERATIONS_POLICY_PASS drills=${EXPECTED_DRILL_IDS.length} ` +
    `evidence_fields=${REQUIRED_EVIDENCE_FIELDS.length} incident_domains=${REQUIRED_FAILURE_DOMAINS.length} ` +
    `ack_minutes=${maxAcknowledgement} runbook=${runbook.path}`,
);
