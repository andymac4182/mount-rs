#!/usr/bin/env node

// Validate the customer-owned Ozone production contract without opening a
// provider connection or reading a credential. This is an admission-contract
// check, not evidence that the customer has deployed or operated the topology.

import { lstat, readFile } from "node:fs/promises"

class ContractValidationError extends Error {
  constructor(reason) {
    super(`W26_OZONE_PRODUCTION_ROLLOUT_CONTRACT_FAIL reason=${reason}`)
    this.name = "ContractValidationError"
  }
}

const METADATA_PROVIDERS = Object.freeze(["sqlite", "pglite", "tidb", "foundationdb"])

function fail(reason) {
  throw new ContractValidationError(reason)
}

function isObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value)
}

function requireObject(value, field) {
  if (!isObject(value)) fail(`${field}-must-be-object`)
  return value
}

function requireKeys(value, allowed, field) {
  for (const key of Object.keys(value)) {
    if (!allowed.includes(key)) fail(`${field}.${key}-is-not-supported`)
  }
}

function requireString(value, field) {
  if (typeof value !== "string" || value.length === 0) {
    fail(`${field}-must-be-non-empty-string`)
  }
  if (/[\u0000-\u001f\u007f<>]/u.test(value)) {
    fail(`${field}-contains-control-or-placeholder`)
  }
  return value
}

function requireBoolean(value, field) {
  if (value !== true) fail(`${field}-must-be-true`)
}

function requireExact(value, expected, field) {
  if (value !== expected) fail(`${field}-must-be-${String(expected)}`)
}

function requireIntegerAtLeast(value, minimum, field) {
  if (!Number.isSafeInteger(value) || value < minimum) {
    fail(`${field}-must-be-safe-integer-at-least-${minimum}`)
  }
}

function requireEnvRef(value, field, expectedName) {
  const reference = requireObject(value, field)
  requireKeys(reference, ["env"], field)
  if (reference.env !== expectedName) fail(`${field}-must-reference-${expectedName}`)
}

function requireScopedTenantPrefix(value, field) {
  const prefix = requireString(value, field)
  const segments = prefix.split("/")
  if (
    prefix.startsWith("/") ||
    prefix.includes("\\") ||
    !prefix.includes("{tenant}") ||
    segments.some((segment) => segment === "" || segment === "." || segment === "..")
  ) {
    fail(`${field}-must-be-a-scoped-tenant-prefix`)
  }
}

function rejectInlineSecrets(value, field = "contract") {
  if (Array.isArray(value)) {
    value.forEach((item, index) => rejectInlineSecrets(item, `${field}[${index}]`))
    return
  }
  if (!isObject(value)) return

  for (const [key, child] of Object.entries(value)) {
    if (/(?:password|secret|token|private[_-]?key|credential)/iu.test(key)) {
      if (typeof child === "string") fail(`${field}.${key}-must-not-be-inline`)
    }
    rejectInlineSecrets(child, `${field}.${key}`)
  }
}

function requireExactSet(value, expected, field) {
  if (
    !Array.isArray(value) ||
    value.length !== expected.length ||
    value.some((item) => typeof item !== "string") ||
    [...value].sort().join(",") !== [...expected].sort().join(",")
  ) {
    fail(`${field}-must-match-supported-provider-set`)
  }
}

export function validateContract(contract) {
  rejectInlineSecrets(contract)
  const root = requireObject(contract, "contract")
  requireKeys(
    root,
    ["version", "service", "ozone", "metadataProviders", "operations", "recovery"],
    "contract",
  )
  requireExact(root.version, 1, "contract.version")

  const service = requireObject(root.service, "contract.service")
  requireKeys(service, ["tier", "availabilityPercent", "rpoMinutes", "rtoMinutes"], "service")
  requireExact(service.tier, "tier-1", "service.tier")
  if (service.availabilityPercent !== 99.99) {
    fail("service.availabilityPercent-must-be-99.99")
  }
  requireExact(service.rpoMinutes, 5, "service.rpoMinutes")
  requireExact(service.rtoMinutes, 5, "service.rtoMinutes")

  const ozone = requireObject(root.ozone, "contract.ozone")
  requireKeys(
    ozone,
    ["gatewayVersion", "endpointRef", "tls", "auth", "topology", "bucketRef", "prefixTemplate"],
    "ozone",
  )
  requireExact(ozone.gatewayVersion, "2.2.1", "ozone.gatewayVersion")
  requireEnvRef(ozone.endpointRef, "ozone.endpointRef", "MOUNT_RS_OZONE_ENDPOINT")
  requireEnvRef(ozone.bucketRef, "ozone.bucketRef", "MOUNT_RS_OZONE_BUCKET")
  requireScopedTenantPrefix(ozone.prefixTemplate, "ozone.prefixTemplate")

  const tls = requireObject(ozone.tls, "ozone.tls")
  requireKeys(tls, ["required", "verifyPeer", "verifyIdentity", "caBundleRef"], "ozone.tls")
  requireBoolean(tls.required, "ozone.tls.required")
  requireBoolean(tls.verifyPeer, "ozone.tls.verifyPeer")
  requireBoolean(tls.verifyIdentity, "ozone.tls.verifyIdentity")
  requireEnvRef(tls.caBundleRef, "ozone.tls.caBundleRef", "MOUNT_RS_OZONE_CA_BUNDLE")

  const auth = requireObject(ozone.auth, "ozone.auth")
  requireKeys(auth, ["mode", "accessKeyRef", "secretKeyRef"], "ozone.auth")
  requireExact(auth.mode, "sigv4", "ozone.auth.mode")
  requireEnvRef(auth.accessKeyRef, "ozone.auth.accessKeyRef", "R2_ACCESS_KEY_ID")
  requireEnvRef(auth.secretKeyRef, "ozone.auth.secretKeyRef", "R2_SECRET_ACCESS_KEY")

  const topology = requireObject(ozone.topology, "ozone.topology")
  requireKeys(
    topology,
    ["minimumDataNodes", "minimumReplicationFactor", "failureDomains", "powerLossDurable"],
    "ozone.topology",
  )
  requireIntegerAtLeast(topology.minimumDataNodes, 3, "ozone.topology.minimumDataNodes")
  requireIntegerAtLeast(
    topology.minimumReplicationFactor,
    3,
    "ozone.topology.minimumReplicationFactor",
  )
  requireIntegerAtLeast(topology.failureDomains, 3, "ozone.topology.failureDomains")
  requireBoolean(topology.powerLossDurable, "ozone.topology.powerLossDurable")

  requireExactSet(root.metadataProviders, METADATA_PROVIDERS, "contract.metadataProviders")

  const operations = requireObject(root.operations, "contract.operations")
  requireKeys(
    operations,
    ["healthProbe", "metrics", "auditLogging", "leastPrivilege", "tenantIsolation"],
    "operations",
  )
  for (const field of Object.keys(operations)) {
    requireBoolean(operations[field], `operations.${field}`)
  }

  const recovery = requireObject(root.recovery, "contract.recovery")
  requireKeys(
    recovery,
    [
      "backupOwner",
      "restoreOwner",
      "restoreDrillRequired",
      "restoreDrillIntervalDays",
      "runbookRef",
      "rpoRtoEvidenceRequired",
    ],
    "recovery",
  )
  requireExact(recovery.backupOwner, "customer-ozone", "recovery.backupOwner")
  requireExact(recovery.restoreOwner, "customer-ozone", "recovery.restoreOwner")
  requireBoolean(recovery.restoreDrillRequired, "recovery.restoreDrillRequired")
  requireIntegerAtLeast(recovery.restoreDrillIntervalDays, 1, "recovery.restoreDrillIntervalDays")
  if (recovery.restoreDrillIntervalDays > 30) {
    fail("recovery.restoreDrillIntervalDays-must-be-at-most-30")
  }
  requireEnvRef(recovery.runbookRef, "recovery.runbookRef", "MOUNT_RS_OZONE_RECOVERY_RUNBOOK_REF")
  requireBoolean(recovery.rpoRtoEvidenceRequired, "recovery.rpoRtoEvidenceRequired")

  return {
    status: "pass",
    metadataProviders: [...METADATA_PROVIDERS],
    customerOwnedRecovery: true,
  }
}

async function readContract(contractPath) {
  try {
    const metadata = await lstat(contractPath)
    if (!metadata.isFile() || metadata.isSymbolicLink()) {
      fail("contract-must-be-regular-file")
    }
    return JSON.parse(await readFile(contractPath, "utf8"))
  } catch (error) {
    if (error instanceof ContractValidationError) throw error
    fail("contract-unreadable-or-invalid-json")
  }
}

export async function main(argv = process.argv.slice(2)) {
  if (argv.length !== 1 || !argv[0]) {
    console.error("usage: verify-w26-ozone-rollout-contract.mjs <contract.json>")
    return 2
  }
  try {
    const result = validateContract(await readContract(argv[0]))
    console.log(
      "W26_OZONE_PRODUCTION_ROLLOUT_CONTRACT_PASS " +
        `providers=${result.metadataProviders.join(",")} recovery_owner=customer-ozone ` +
        "evidence=declaration-only",
    )
    return 0
  } catch (error) {
    const message = error?.message || "validation-failed"
    console.error(
      message.startsWith("W26_OZONE_PRODUCTION_ROLLOUT_CONTRACT_FAIL")
        ? message
        : "W26_OZONE_PRODUCTION_ROLLOUT_CONTRACT_FAIL reason=validation-failed",
    )
    return 1
  }
}

if (process.argv[1] && process.argv[1].endsWith("verify-w26-ozone-rollout-contract.mjs")) {
  main().then((code) => {
    process.exitCode = code
  })
}
