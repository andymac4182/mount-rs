#!/usr/bin/env node

// Credential-free P01 topology policy. This validates the shape of a
// production-like deployment contract only; it never contacts TiDB, Docker,
// object storage, a secret manager or a network endpoint.

import { lstat, readFile } from "node:fs/promises";

const configPath = process.argv[2];

function fail(reason) {
  console.error(`W08_PRODUCTION_TOPOLOGY_POLICY_FAIL reason=${reason}`);
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
  if (/\s{2,}|[<>]/u.test(value)) fail(`${path}-contains-placeholder`);
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
    if (/(?:password|secret|token|private[_-]?key|credential)/iu.test(key)) {
      if (typeof child === "string") fail(`${path}.${key}-must-not-be-inline`);
    }
    rejectInlineSecrets(child, `${path}.${key}`);
  }
}

if (!configPath || process.argv.length !== 3) {
  console.error("usage: verify-w08-production-topology.mjs <topology.json>");
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
  if (error?.message?.startsWith("W08_PRODUCTION_TOPOLOGY_POLICY_FAIL")) {
    throw error;
  }
  fail("config-unreadable-or-invalid-json");
}

rejectInlineSecrets(config);
const root = requireObject(config, "config");
rejectUnknown(
  root,
  ["version", "environment", "topology", "versions", "blocks", "network", "resources", "support"],
  "config",
);
if (root.version !== 1) fail("config.version-must-be-1");

const environment = requireObject(root.environment, "config.environment");
rejectUnknown(environment, ["class", "region", "owner"], "config.environment");
requireEnum(
  environment.class,
  "environment.class",
  ["production-like-staging", "production"],
);
requireString(environment.region, "environment.region");
requireString(environment.owner, "environment.owner");

const topology = requireObject(root.topology, "config.topology");
rejectUnknown(
  topology,
  ["class", "pd_replicas", "tikv_replicas", "sql_frontends", "quorum"],
  "config.topology",
);
requireEnum(topology.class, "topology.class", ["replicated-durable"]);
const pdReplicas = requireSafeInteger(topology.pd_replicas, "topology.pd_replicas", 3);
const tikvReplicas = requireSafeInteger(
  topology.tikv_replicas,
  "topology.tikv_replicas",
  3,
);
requireSafeInteger(topology.sql_frontends, "topology.sql_frontends", 2);
const quorum = requireObject(topology.quorum, "topology.quorum");
rejectUnknown(quorum, ["pd_min_available", "tikv_min_available"], "config.topology.quorum");
const pdQuorum = requireSafeInteger(
  quorum.pd_min_available,
  "topology.quorum.pd_min_available",
  2,
);
const tikvQuorum = requireSafeInteger(
  quorum.tikv_min_available,
  "topology.quorum.tikv_min_available",
  2,
);
if (pdQuorum > pdReplicas) fail("topology.quorum.pd_min_available-exceeds-replicas");
if (tikvQuorum > tikvReplicas) fail("topology.quorum.tikv_min_available-exceeds-replicas");

const versions = requireObject(root.versions, "config.versions");
rejectUnknown(versions, ["pd", "tikv", "tidb"], "config.versions");
const versionValues = [
  requireString(versions.pd, "versions.pd"),
  requireString(versions.tikv, "versions.tikv"),
  requireString(versions.tidb, "versions.tidb"),
];
for (const [index, version] of versionValues.entries()) {
  if (!/^v\d+\.\d+\.\d+$/u.test(version)) {
    fail(`versions.${["pd", "tikv", "tidb"][index]}-must-be-pinned-semver`);
  }
}
if (new Set(versionValues).size !== 1) fail("versions-must-be-coherent");

const blocks = requireObject(root.blocks, "config.blocks");
rejectUnknown(blocks, ["provider", "durability", "tls"], "config.blocks");
requireEnum(blocks.provider, "blocks.provider", ["r2"]);
requireEnum(blocks.durability, "blocks.durability", ["provider-managed"]);
requireBoolean(blocks.tls, "blocks.tls", true);

const network = requireObject(root.network, "config.network");
rejectUnknown(network, ["private", "tidb_tls", "blocks_tls"], "config.network");
requireBoolean(network.private, "network.private", true);
requireBoolean(network.tidb_tls, "network.tidb_tls", true);
requireBoolean(network.blocks_tls, "network.blocks_tls", true);

const resources = requireObject(root.resources, "config.resources");
rejectUnknown(resources, ["min_cpu", "min_memory_gib"], "config.resources");
requireSafeInteger(resources.min_cpu, "resources.min_cpu", 4);
requireSafeInteger(resources.min_memory_gib, "resources.min_memory_gib", 10);

const support = requireObject(root.support, "config.support");
rejectUnknown(support, ["platforms", "tenant_isolation"], "config.support");
const platforms = requireStringArray(support.platforms, "support.platforms");
requireBoolean(support.tenant_isolation, "support.tenant_isolation", true);

console.log(
  "W08_PRODUCTION_TOPOLOGY_POLICY_PASS " +
    `class=${topology.class} pd_replicas=${pdReplicas} ` +
    `tikv_replicas=${tikvReplicas} sql_frontends=${topology.sql_frontends} ` +
    `quorum=pd:${pdQuorum},tikv:${tikvQuorum} versions=${versionValues[0]} ` +
    `blocks=${blocks.provider}/${blocks.durability} tls=required ` +
    `platforms=${platforms.length}`,
);
