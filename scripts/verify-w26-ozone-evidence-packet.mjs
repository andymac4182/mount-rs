#!/usr/bin/env node

// Validate the complete W26 Apache Ozone CI evidence packet. The provider
// benchmark artifacts are necessary but not sufficient: this gate also
// requires the policy, recovery, integration, provider acceptance, and
// cleanup markers from the same source revision.

import { lstat, readFile } from "node:fs/promises"
import { fileURLToPath } from "node:url"
import { resolve } from "node:path"

import { validateArtifact, W26_IOPS_MINIMUM } from "./verify-w26-ozone-iops-artifact.mjs"

const PACKET_SPECS = Object.freeze([
  {
    id: "ozone-compositions",
    artifactKey: "compositionsArtifact",
    logKey: "compositionsLog",
    providers: ["mount-rs-split-sqlite-r2", "mount-rs-split-pglite-r2"],
    markers: [
      "OZONE_SQLITE_CHUNKED_COMPOSITION_PASS ",
      "OZONE_PGLITE_CHUNKED_COMPOSITION_PASS ",
      "OZONE_CHUNKED_BOUNDED_READDIR_PASS provider_owner=ozone-sqlite-seed ",
      "OZONE_CHUNKED_BOUNDED_READDIR_PASS provider_owner=ozone-pglite-seed ",
      "test actual_binary_runs_live_ozone_split_provider_self_test ... ok",
      "SUMMARY node-sdk pass=7 skip=1 fail=0",
      "OZONE_NODE_CLI_PASS ",
      "OZONE_CLI_REMOTE_HTTP_PASS ",
      "OZONE_COMPOSITION_PGLITE_READY ",
      "OZONE_IOPS_PASS providers=mount-rs-split-sqlite-r2,mount-rs-split-pglite-r2 target=1000 ",
      "OZONE_INTEGRATION_PASS ",
      "OZONE_CLEANUP_PASS ",
      "OZONE_COMPOSITION_CLEANUP_PASS",
    ],
  },
  {
    id: "ozone-tidb",
    artifactKey: "tidbArtifact",
    logKey: "tidbLog",
    providers: ["mount-rs-split-tidb-r2"],
    markers: [
      "TIDB_RUSTFS_CHUNKED_BOUNDED_READDIR_PASS phase=seed ",
      "TIDB_CHUNKED_RUSTFS_SEED_PASS",
      "TIDB_NAPI_BOUNDED_READDIR_PASS phase=seed ",
      "TIDB_NAPI_BOUNDED_READDIR_PASS phase=reopen ",
      "TIDB_ACCEPTANCE ",
      "TIDB_OZONE_IOPS_PASS provider=tidb-r2 target=1000 ",
      "OZONE_INTEGRATION_PASS ",
      "OZONE_CLEANUP_PASS ",
    ],
  },
  {
    id: "ozone-foundationdb",
    artifactKey: "foundationdbArtifact",
    logKey: "foundationdbLog",
    providers: ["mount-rs-split-foundationdb-r2"],
    markers: [
      "FOUNDATIONDB_RUSTFS_CHUNKED_BOUNDED_READDIR_PASS phase=seed ",
      "FOUNDATIONDB_RUSTFS_CHUNKED_PASS ",
      "FOUNDATIONDB_RUSTFS_SERVICE_RESTART_PASS ",
      "FOUNDATIONDB_NAPI_BOUNDED_READDIR_PASS phase=seed ",
      "FOUNDATIONDB_NAPI_BOUNDED_READDIR_PASS phase=reopen ",
      "FOUNDATIONDB_SERVICE_RESTART_READY ",
      "FOUNDATIONDB_NAPI_PASS image=",
      "FOUNDATIONDB_OZONE_IOPS_PASS provider=foundationdb-r2 target=1000 ",
      "FOUNDATIONDB_TEST_PASS ",
      "OZONE_INTEGRATION_PASS ",
      "OZONE_CLEANUP_PASS ",
    ],
  },
])

const POLICY_MARKERS = Object.freeze([
  "W26_OZONE_PRODUCTION_CONFIG_POLICY_PASS metadata=sqlite ",
  "W26_OZONE_PRODUCTION_CONFIG_POLICY_PASS metadata=pglite ",
  "W26_OZONE_PRODUCTION_CONFIG_POLICY_PASS metadata=tidb ",
  "W26_OZONE_PRODUCTION_CONFIG_POLICY_PASS metadata=foundationdb ",
  "W26_OZONE_PRODUCTION_CONFIG_POLICY_NEGATIVE_PASS case=insecure-blocks",
  "W26_OZONE_PRODUCTION_CONFIG_POLICY_NEGATIVE_PASS case=inline-secret",
  "W26_OZONE_PRODUCTION_CONFIG_POLICY_NEGATIVE_PASS case=foundationdb-unsafe",
  "W26_OZONE_PRODUCTION_CONFIG_POLICY_NEGATIVE_PASS case=tidb-tls-weak",
  "W26_OZONE_PRODUCTION_ROLLOUT_CONTRACT_PASS ",
  "W26_OZONE_PRODUCTION_ROLLOUT_CONTRACT_NEGATIVE_PASS case=slo",
  "W26_OZONE_PRODUCTION_ROLLOUT_CONTRACT_NEGATIVE_PASS case=inline-secret",
])

function failure(reason) {
  throw new Error(`W26_OZONE_EVIDENCE_PACKET_FAIL reason=${reason}`)
}

function requireString(value, field) {
  if (typeof value !== "string" || value.length === 0) {
    failure(`${field}-must-be-non-empty-string`)
  }
  return value
}

function requireRevision(value, field) {
  const revision = requireString(value, field)
  if (!/^[0-9a-f]{40,64}$/u.test(revision)) {
    failure(`${field}-must-be-a-full-git-revision`)
  }
  return revision
}

function requireMarker(log, marker, field) {
  if (!log.includes(marker)) {
    failure(`${field}-missing-marker=${marker}`)
  }
}

function requireSourceRevision(artifact, field, expectedRevision) {
  const source = artifact?.environment?.sourceControl?.mountRs
  if (!source || typeof source !== "object") {
    failure(`${field}-source-control-metadata-missing`)
  }
  if (source.revisionVerified !== true) {
    failure(`${field}-source-revision-unverified`)
  }
  if (source.dirty !== false || source.dirtyEntryCount !== 0) {
    failure(`${field}-source-checkout-dirty`)
  }
  if (source.revision !== expectedRevision) {
    failure(`${field}-source-revision-does-not-match-packet`)
  }
}

export function validateEvidencePacket(packet) {
  const expectedRevision = requireRevision(packet?.expectedRevision, "expected-revision")
  const policyLog = requireString(packet?.policyLog, "policy-log")
  const baseLog = requireString(packet?.baseLog, "base-log")

  for (const marker of POLICY_MARKERS) {
    requireMarker(policyLog, marker, "policy-log")
  }
  for (const marker of [
    "OZONE_HEALTHY ",
    "OZONE_READY ",
    "OZONE_FAULT_WINDOW_PASS ",
    "OZONE_RESTART_READY ",
    "OZONE_INTEGRATION_PASS ",
    "OZONE_CLEANUP_PASS ",
  ]) {
    requireMarker(baseLog, marker, "base-log")
  }

  const artifacts = []
  for (const spec of PACKET_SPECS) {
    const log = requireString(packet?.[spec.logKey], `${spec.id}-log`)
    for (const marker of spec.markers) {
      requireMarker(log, marker, `${spec.id}-log`)
    }
    const artifact = packet?.[spec.artifactKey]
    try {
      validateArtifact(artifact, {
        providers: spec.providers,
        minimumIops: W26_IOPS_MINIMUM,
      })
    } catch (error) {
      failure(`${spec.id}-artifact-invalid`)
    }
    requireSourceRevision(artifact, `${spec.id}-artifact`, expectedRevision)
    artifacts.push(spec.id)
  }

  return {
    revision: expectedRevision,
    artifacts,
    policyMarkers: POLICY_MARKERS.length,
  }
}

function takeValue(argv, index, flag) {
  const value = argv[index + 1]
  if (!value || value.startsWith("--")) failure(`${flag}-requires-a-value`)
  return value
}

export function parseArgs(argv) {
  const options = {}
  let help = false
  const flags = {
    "--revision": "expectedRevision",
    "--policy-log": "policyLogPath",
    "--base-log": "baseLogPath",
    "--compositions-log": "compositionsLogPath",
    "--compositions-artifact": "compositionsArtifactPath",
    "--tidb-log": "tidbLogPath",
    "--tidb-artifact": "tidbArtifactPath",
    "--foundationdb-log": "foundationdbLogPath",
    "--foundationdb-artifact": "foundationdbArtifactPath",
  }
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index]
    if (argument === "--help" || argument === "-h") {
      help = true
      continue
    }
    const field = flags[argument]
    if (!field) failure(`unknown-argument=${argument}`)
    options[field] = takeValue(argv, index++, argument)
  }
  if (!help) {
    for (const field of Object.values(flags)) {
      if (!options[field]) failure(`${field}-is-required`)
    }
  }
  return { ...options, help }
}

export function helpText() {
  return `Validate the complete W26 Apache Ozone CI evidence packet

Usage:
  node scripts/verify-w26-ozone-evidence-packet.mjs \
    --revision <git-revision> \
    --policy-log evidence/policy/ozone-policy.log \
    --base-log evidence/base/ozone-base.log \
    --compositions-log evidence/compositions/ozone-compositions.log \
    --compositions-artifact evidence/compositions/ozone-iops.json \
    --tidb-log evidence/tidb/ozone-tidb.log \
    --tidb-artifact evidence/tidb/ozone-tidb-iops.json \
    --foundationdb-log evidence/foundationdb/ozone-foundationdb.log \
    --foundationdb-artifact evidence/foundationdb/ozone-foundationdb-iops.json
`
}

async function readRegularFile(filePath, field) {
  try {
    const metadata = await lstat(filePath)
    if (!metadata.isFile() || metadata.isSymbolicLink()) failure(`${field}-must-be-regular-file`)
    return await readFile(filePath, "utf8")
  } catch (error) {
    if (error?.message?.startsWith("W26_OZONE_EVIDENCE_PACKET_FAIL")) throw error
    failure(`${field}-unreadable`)
  }
}

async function readJson(filePath, field) {
  const text = await readRegularFile(filePath, field)
  try {
    return JSON.parse(text)
  } catch {
    failure(`${field}-invalid-json`)
  }
}

async function packetFromOptions(options) {
  return {
    expectedRevision: options.expectedRevision,
    policyLog: await readRegularFile(options.policyLogPath, "policy-log"),
    baseLog: await readRegularFile(options.baseLogPath, "base-log"),
    compositionsLog: await readRegularFile(options.compositionsLogPath, "compositions-log"),
    compositionsArtifact: await readJson(options.compositionsArtifactPath, "compositions-artifact"),
    tidbLog: await readRegularFile(options.tidbLogPath, "tidb-log"),
    tidbArtifact: await readJson(options.tidbArtifactPath, "tidb-artifact"),
    foundationdbLog: await readRegularFile(options.foundationdbLogPath, "foundationdb-log"),
    foundationdbArtifact: await readJson(options.foundationdbArtifactPath, "foundationdb-artifact"),
  }
}

export async function main(argv = process.argv.slice(2)) {
  try {
    const options = parseArgs(argv)
    if (options.help) {
      process.stdout.write(helpText())
      return 0
    }
    const result = validateEvidencePacket(await packetFromOptions(options))
    console.log(
      `W26_OZONE_EVIDENCE_PACKET_PASS revision=${result.revision} ` +
        `artifacts=${result.artifacts.join(",")} policy_markers=${result.policyMarkers}`,
    )
    return 0
  } catch (error) {
    const message = error?.message || "validation-failed"
    console.error(
      message.startsWith("W26_OZONE_EVIDENCE_PACKET_FAIL")
        ? message
        : "W26_OZONE_EVIDENCE_PACKET_FAIL reason=validation-failed",
    )
    return 1
  }
}

const invokedPath = process.argv[1] ? resolve(process.argv[1]) : null
if (invokedPath === resolve(fileURLToPath(import.meta.url))) {
  main().then((code) => {
    process.exitCode = code
  })
}
