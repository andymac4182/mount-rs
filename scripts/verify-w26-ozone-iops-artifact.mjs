#!/usr/bin/env node

// Validate the machine-readable IOPS evidence before a W26 Ozone wrapper emits
// its pass marker. This is intentionally credential-free: it checks only the
// benchmark result shape, requested providers and fixed qualification profile.

import { lstat, readFile } from "node:fs/promises"
import { fileURLToPath } from "node:url"
import { resolve } from "node:path"

export const W26_IOPS_MINIMUM = 1000
export const W26_IOPS_PROFILE = Object.freeze({
  payloadBytes: 4096,
  iterations: 400,
  concurrency: 64,
})

function failure(reason) {
  throw new Error(`W26_OZONE_IOPS_ARTIFACT_FAIL reason=${reason}`)
}

function isObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value)
}

function providerList(value, field) {
  const values = Array.isArray(value) ? value : String(value || "").split(",")
  const providers = values.map((item) => String(item).trim()).filter(Boolean)
  if (providers.length === 0 || new Set(providers).size !== providers.length) {
    failure(`${field}-must-contain-unique-providers`)
  }
  return providers
}

function requireObject(value, field) {
  if (!isObject(value)) failure(`${field}-must-be-object`)
  return value
}

function requireInteger(value, field, minimum = 1) {
  if (!Number.isSafeInteger(value) || value < minimum) {
    failure(`${field}-must-be-safe-integer-at-least-${minimum}`)
  }
  return value
}

function requireEqual(value, expected, field) {
  if (value !== expected) failure(`${field}-must-equal-${expected}`)
}

function sameSet(actual, expected) {
  return (
    actual.length === expected.length &&
    actual.every((provider) => expected.includes(provider))
  )
}

export function validateArtifact(document, options = {}) {
  const root = requireObject(document, "artifact")
  const expectedProviders = providerList(options.providers, "providers")
  const minimumIops = requireInteger(
    options.minimumIops ?? W26_IOPS_MINIMUM,
    "minimum-iops",
    W26_IOPS_MINIMUM,
  )

  requireEqual(root.schemaVersion, "mount-rs.storage-benchmark.v1", "schema-version")
  requireEqual(root.status, "ok", "status")

  const config = requireObject(root.config, "config")
  requireEqual(config.requireConfigured, true, "config.requireConfigured")
  requireInteger(config.minIops, "config.minIops", minimumIops)
  requireEqual(config.payloadBytes, W26_IOPS_PROFILE.payloadBytes, "config.payloadBytes")
  requireEqual(config.iterations, W26_IOPS_PROFILE.iterations, "config.iterations")
  requireEqual(config.concurrency, W26_IOPS_PROFILE.concurrency, "config.concurrency")
  if (
    !Array.isArray(config.sizesMiB) ||
    config.sizesMiB.length === 0 ||
    config.sizesMiB.some((size) => !Number.isSafeInteger(size) || size <= 0)
  ) {
    failure("config.sizesMiB-must-contain-positive-safe-integers")
  }

  const counts = requireObject(root.counts, "counts")
  requireEqual(counts.providersRequested, expectedProviders.length, "counts.providersRequested")
  requireEqual(counts.providersFailed, 0, "counts.providersFailed")
  requireEqual(counts.providersSkipped, 0, "counts.providersSkipped")
  requireEqual(counts.configurationFailures, 0, "counts.configurationFailures")
  requireEqual(counts.sizeResultsFailed, 0, "counts.sizeResultsFailed")
  requireEqual(counts.sizeResultsSkipped, 0, "counts.sizeResultsSkipped")

  if (!Array.isArray(root.configurationFailures) || root.configurationFailures.length !== 0) {
    failure("configurationFailures-must-be-empty")
  }

  if (!Array.isArray(root.providers) || !sameSet(root.providers.map((provider) => provider?.provider), expectedProviders)) {
    failure("providers-do-not-match-requested-provider-set")
  }

  for (const provider of root.providers) {
    requireObject(provider, "provider")
    requireEqual(provider.status, "ok", `${provider.provider}.status`)
    if (provider.missingConfiguration?.length > 0) {
      failure(`${provider.provider}.missing-configuration`)
    }
    const cleanup = requireObject(provider.cleanup, `${provider.provider}.cleanup`)
    requireEqual(cleanup.remainingPaths, 0, `${provider.provider}.cleanup.remainingPaths`)
    if (!Array.isArray(cleanup.failures)) {
      failure(`${provider.provider}.cleanup.failures-must-be-array`)
    }
    requireEqual(cleanup.failures.length, 0, `${provider.provider}.cleanup.failures`)
    if (!isObject(cleanup.resource)) {
      failure(`${provider.provider}.cleanup.resource-must-be-object`)
    }
    requireEqual(cleanup.resource?.status, "ok", `${provider.provider}.cleanup.resource.status`)

    if (!Array.isArray(provider.sizes) || provider.sizes.length !== config.sizesMiB.length) {
      failure(`${provider.provider}.sizes-must-match-requested-sizes`)
    }
    for (const size of provider.sizes) {
      requireObject(size, `${provider.provider}.size`)
      requireEqual(size.status, "ok", `${provider.provider}.size.status`)
      const summary = requireObject(size.summary, `${provider.provider}.size.summary`)
      requireEqual(
        summary.successfulIterations,
        config.iterations,
        `${provider.provider}.size.summary.successfulIterations`,
      )
      requireEqual(
        summary.failedIterations,
        0,
        `${provider.provider}.size.summary.failedIterations`,
      )
      requireEqual(
        summary.successfulOperations,
        config.iterations * 3,
        `${provider.provider}.size.summary.successfulOperations`,
      )
      requireEqual(
        summary.attemptedOperations,
        config.iterations * 3,
        `${provider.provider}.size.summary.attemptedOperations`,
      )
      if (!Number.isFinite(summary.iops) || summary.iops < minimumIops) {
        failure(`${provider.provider}.size.summary.iops-below-${minimumIops}`)
      }
      requireEqual(
        summary.iopsTarget,
        config.minIops,
        `${provider.provider}.size.summary.iopsTarget`,
      )
      requireEqual(size.summary.iopsTargetMet, true, `${provider.provider}.size.summary.iopsTargetMet`)
    }
  }

  return {
    providers: expectedProviders,
    minimumIops,
    sizesMiB: [...config.sizesMiB],
    profile: { ...W26_IOPS_PROFILE },
  }
}

function takeValue(argv, index, flag) {
  const value = argv[index + 1]
  if (!value || value.startsWith("--")) failure(`${flag}-requires-a-value`)
  return value
}

export function parseArgs(argv) {
  let output
  let providers
  let minimumIops = W26_IOPS_MINIMUM
  let help = false
  for (let index = 0; index < argv.length; index += 1) {
    switch (argv[index]) {
      case "--help":
      case "-h":
        help = true
        break
      case "--output":
        output = takeValue(argv, index++, "--output")
        break
      case "--providers":
        providers = providerList(takeValue(argv, index++, "--providers"), "providers")
        break
      case "--minimum-iops":
        minimumIops = Number(takeValue(argv, index++, "--minimum-iops"))
        requireInteger(minimumIops, "minimum-iops", W26_IOPS_MINIMUM)
        break
      default:
        failure(`unknown-argument=${argv[index]}`)
    }
  }
  if (!help) {
    if (!output) failure("output-is-required")
    if (!providers) failure("providers-are-required")
  }
  return { help, output, providers, minimumIops }
}

export function helpText() {
  return `Validate a W26 Apache Ozone IOPS artifact

Usage:
  node scripts/verify-w26-ozone-iops-artifact.mjs \
    --output artifacts/ozone-iops.json \
    --providers mount-rs-split-sqlite-r2,mount-rs-split-pglite-r2 \
    --minimum-iops 1000
`
}

async function readArtifact(output) {
  try {
    const metadata = await lstat(output)
    if (!metadata.isFile() || metadata.isSymbolicLink()) {
      failure("output-must-be-regular-file")
    }
    return JSON.parse(await readFile(output, "utf8"))
  } catch (error) {
    if (error?.message?.startsWith("W26_OZONE_IOPS_ARTIFACT_FAIL")) throw error
    failure("output-unreadable-or-invalid-json")
  }
}

export async function main(argv = process.argv.slice(2)) {
  try {
    const options = parseArgs(argv)
    if (options.help) {
      process.stdout.write(helpText())
      return 0
    }
    const artifact = await readArtifact(options.output)
    const result = validateArtifact(artifact, options)
    console.log(
      `W26_OZONE_IOPS_ARTIFACT_PASS providers=${result.providers.join(",")} ` +
        `target=${result.minimumIops} output=${resolve(options.output)}`,
    )
    return 0
  } catch (error) {
    const message = error?.message || "validation-failed"
    console.error(
      message.startsWith("W26_OZONE_IOPS_ARTIFACT_FAIL")
        ? message
        : "W26_OZONE_IOPS_ARTIFACT_FAIL reason=validation-failed",
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
