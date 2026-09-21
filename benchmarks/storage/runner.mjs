import { createHash, randomBytes } from "node:crypto"
import { readFile, mkdir, writeFile } from "node:fs/promises"
import { execFile } from "node:child_process"
import { arch, cpus, platform, release } from "node:os"
import { dirname, resolve } from "node:path"
import { fileURLToPath } from "node:url"
import { promisify } from "node:util"

import {
  errorRecord,
  isTimeout,
  usageError,
  waitForLateOperation,
  waitForPromiseSettlement,
  withTimeout,
} from "./errors.mjs"
import {
  loadedNativeAddonPaths,
  mountxSourcePath,
  MOUNTX_PINNED_REVISION,
  providerById,
  providerSummary,
  DEFAULT_CHUNK_SIZE_BYTES,
} from "./providers.mjs"
import { computeStats, round, roundStats } from "./stats.mjs"

export const REFERENCE_REVISION = "92fbbc9ba7739111899121195236acb4fc6a8bb5"
export const FILE_SIZE_MIB = Object.freeze([1, 4, 10, 16])

const SMOKE_PROVIDER_IDS = Object.freeze([
  "mount-rs-memory",
  "mount-rs-sqlite",
  "mountx-memory",
])
const storageDirectory = dirname(fileURLToPath(import.meta.url))
const repoRoot = resolve(storageDirectory, "../..")
const execFileAsync = promisify(execFile)

function integer(value, flag) {
  if (!/^\d+$/.test(value)) throw usageError(`${flag} must be a positive integer: ${value}`)
  const parsed = Number(value)
  if (!Number.isSafeInteger(parsed) || parsed <= 0) {
    throw usageError(`${flag} must be a positive integer: ${value}`)
  }
  return parsed
}

function sizeMiB(value) {
  const match = /^([1-9]\d*)(?:mib|mb)?$/i.exec(value)
  if (!match) throw usageError(`size must be a positive MiB value: ${value}`)
  const parsed = Number(match[1])
  if (!Number.isSafeInteger(parsed) || parsed <= 0) {
    throw usageError(`size must be a positive MiB value: ${value}`)
  }
  return parsed
}

function listValue(value, parser, flag) {
  const values = value
    .split(",")
    .map((item) => item.trim())
    .filter(Boolean)
    .map((item) => parser(item, flag))
  if (values.length === 0) throw usageError(`${flag} must not be empty`)
  return [...new Set(values)]
}

function takeValue(argv, index, flag) {
  const value = argv[index + 1]
  if (!value || value.startsWith("--")) throw usageError(`${flag} requires a value`)
  return value
}

export function parseArgs(argv) {
  let smoke = false
  let sizes
  let iterations
  let concurrency
  let timeoutMs
  let cleanupTimeoutMs
  let chunkSizeBytes = DEFAULT_CHUNK_SIZE_BYTES
  let providers
  let output
  let payloadSeed = "mount-rs-storage-benchmark"
  let networkContext = "not-provided"
  let payloadBytes
  let minIops
  let requireConfigured = false
  let help = false

  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index]
    switch (argument) {
      case "--help":
      case "-h":
        help = true
        break
      case "--smoke":
        smoke = true
        break
      case "--full":
        smoke = false
        break
      case "--sizes":
      case "--size":
        sizes = listValue(takeValue(argv, index++, argument), sizeMiB, argument)
        break
      case "--iterations":
        iterations = integer(takeValue(argv, index++, argument), argument)
        break
      case "--concurrency":
        concurrency = integer(takeValue(argv, index++, argument), argument)
        break
      case "--timeout-ms":
        timeoutMs = integer(takeValue(argv, index++, argument), argument)
        break
      case "--cleanup-timeout-ms":
        cleanupTimeoutMs = integer(takeValue(argv, index++, argument), argument)
        break
      case "--chunk-size-bytes":
        chunkSizeBytes = integer(takeValue(argv, index++, argument), argument)
        break
      case "--payload-bytes":
        payloadBytes = integer(takeValue(argv, index++, argument), argument)
        break
      case "--min-iops":
        minIops = integer(takeValue(argv, index++, argument), argument)
        break
      case "--require-configured":
        requireConfigured = true
        break
      case "--providers":
      case "--provider":
        providers = takeValue(argv, index++, argument)
          .split(",")
          .map((item) => item.trim())
          .filter(Boolean)
        if (providers.length === 0) throw usageError(`${argument} must not be empty`)
        providers = [...new Set(providers)]
        break
      case "--output":
        output = takeValue(argv, index++, argument)
        break
      case "--payload-seed":
        payloadSeed = takeValue(argv, index++, argument)
        break
      case "--network-context":
        networkContext = takeValue(argv, index++, argument)
        break
      default:
        throw usageError(`unknown argument: ${argument}`)
    }
  }

  return {
    help,
    mode: smoke ? "smoke" : "full",
    sizes: sizes || (smoke ? [1] : [...FILE_SIZE_MIB]),
    iterations: iterations || (smoke ? 1 : 2),
    concurrency: concurrency || 1,
    timeoutMs: timeoutMs || 30_000,
    cleanupTimeoutMs: cleanupTimeoutMs || 10_000,
    chunkSizeBytes,
    providers: providers || (smoke ? [...SMOKE_PROVIDER_IDS] : null),
    output,
    payloadSeed,
    networkContext,
    payloadBytes,
    minIops,
    requireConfigured,
  }
}

export function helpText() {
  return `Storage benchmark runner (ComputeSDK reference ${REFERENCE_REVISION})

Usage:
  node benchmarks/storage/runner.mjs --smoke
  node benchmarks/storage/runner.mjs --iterations 2 --concurrency 1 \
    --sizes 1,4,10,16 --output storage-results.json

Options:
  --smoke                  1 MiB, one iteration, concurrency one, local providers
  --sizes LIST             MiB values; defaults to 1,4,10,16 in full mode
  --iterations N            lifecycle iterations per size (default 2 full, 1 smoke)
  --concurrency N           concurrent lifecycle workers per provider/size (default 1)
  --providers LIST          provider ids (default all in full mode)
  --timeout-ms N            write/read operation timeout (default 30000)
  --cleanup-timeout-ms N   delete and cleanup timeout (default 10000)
  --chunk-size-bytes N     fixed chunk size for split providers (default 65536)
  --payload-bytes N         override the generated payload size for each lifecycle
  --min-iops N              fail a result below the successful lifecycle IOPS target
  --require-configured      fail when any requested provider is not configured
  --output PATH             also write machine-readable JSON to PATH
  --payload-seed TEXT       deterministic payload seed
  --network-context TEXT    recorded remote network context
  --help                    show this text
`
}

function makeRunId() {
  return `${Date.now().toString(36)}-${randomBytes(6).toString("hex")}`
}

/** Payload generation is deliberately called before any timed operation. */
export function makePayload(sizeBytes, seed) {
  const payload = Buffer.allocUnsafe(sizeBytes)
  const seedDigest = createHash("sha256").update(`${seed}:${sizeBytes}`).digest()
  let state = seedDigest.readUInt32LE(0) || 0x9e3779b9
  for (let index = 0; index < payload.length; index += 1) {
    state ^= state << 13
    state ^= state >>> 17
    state ^= state << 5
    payload[index] = state & 0xff
  }
  return payload
}

function digest(payload) {
  return createHash("sha256").update(payload).digest("hex")
}

function bytes(value) {
  if (value instanceof Uint8Array) return value
  if (value instanceof ArrayBuffer) return new Uint8Array(value)
  throw new TypeError(`readFile returned ${Object.prototype.toString.call(value)}, not bytes`)
}

function sameBytes(left, right) {
  if (left.byteLength !== right.byteLength) return false
  for (let index = 0; index < left.byteLength; index += 1) {
    if (left[index] !== right[index]) return false
  }
  return true
}

function benchmarkPath(runId, providerId, sizeMiBValue, iteration) {
  const safeProvider = providerId.replace(/[^A-Za-z0-9_-]/g, "_")
  return `/.mount-rs-storage-${runId}-${safeProvider}-${sizeMiBValue}MiB-${iteration}.bin`
}

async function runWorkers(items, concurrency, callback) {
  const results = new Array(items.length)
  let cursor = 0
  async function worker(slot) {
    for (;;) {
      const index = cursor
      cursor += 1
      if (index >= items.length) return
      results[index] = await callback(items[index], slot)
    }
  }
  const workerCount = Math.min(concurrency, items.length)
  await Promise.all(Array.from({ length: workerCount }, (_, slot) => worker(slot)))
  return results
}

function emptySample(definition, sizeBytes, iteration, path) {
  return {
    provider: definition.id,
    fileSizeBytes: sizeBytes,
    iteration,
    path,
    status: "failed",
    writeMs: null,
    readMs: null,
    uploadMs: null,
    downloadMs: null,
    deleteMs: null,
    cleanupMs: null,
    throughputMbps: null,
    bytesExpected: sizeBytes,
    bytesReturned: null,
    payloadVerified: null,
    writeSucceeded: false,
    readSucceeded: false,
    deleteSucceeded: false,
    cleanupSucceeded: null,
    cleanupFailure: false,
    timedOut: false,
    timeoutOperations: [],
    lateOperations: [],
    cleanupDeferred: false,
    errors: [],
  }
}

function firstFailure(sample, operation, error) {
  if (!sample.errors.some((entry) => entry.operation === operation)) {
    sample.errors.push({ operation, error: errorRecord(error) })
  }
  if (!sample.failureOperation) sample.failureOperation = operation
  if (isTimeout(error)) {
    sample.timedOut = true
    if (!sample.timeoutOperations.includes(operation)) sample.timeoutOperations.push(operation)
  }
}

async function attemptCleanup(filesystem, path, timeoutMs, pendingOperations) {
  const started = performance.now()
  try {
    await withTimeout(() => filesystem.unlink(path), timeoutMs, "cleanup")
    return {
      succeeded: true,
      alreadyAbsent: false,
      ms: performance.now() - started,
    }
  } catch (error) {
    if (isTimeout(error)) {
      const late = await waitForLateOperation(error, timeoutMs)
      if (late.status === "pending") {
        pendingOperations?.set(path, { operation: "cleanup", promise: error.lateOperation })
        return {
          succeeded: false,
          alreadyAbsent: false,
          pending: true,
          ms: performance.now() - started,
          error: errorRecord(error),
          timedOut: true,
        }
      }
      if (late.status === "fulfilled") {
        return {
          succeeded: true,
          alreadyAbsent: false,
          ms: performance.now() - started,
          late: true,
        }
      }
      if (late.status === "rejected" && late.error?.code === "ENOENT") {
        return {
          succeeded: true,
          alreadyAbsent: true,
          ms: performance.now() - started,
          late: true,
        }
      }
      return {
        succeeded: false,
        alreadyAbsent: false,
        ms: performance.now() - started,
        error: errorRecord(late.error || error),
        timedOut: true,
      }
    }
    if (error && error.code === "ENOENT") {
      return {
        succeeded: true,
        alreadyAbsent: true,
        ms: performance.now() - started,
      }
    }
    return {
      succeeded: false,
      alreadyAbsent: false,
      ms: performance.now() - started,
      error: errorRecord(error),
      timedOut: isTimeout(error),
    }
  }
}

function deferCleanup(sample, operation) {
  sample.cleanupSucceeded = false
  sample.cleanupFailure = true
  sample.cleanupDeferred = true
  const error = new Error(`cleanup deferred while timed-out ${operation} remains pending`)
  error.code = "CLEANUP_DEFERRED"
  error.operation = operation
  firstFailure(sample, "cleanup", error)
}

async function observeLateOperation(sample, operation, error, options, path, pendingOperations) {
  if (!isTimeout(error)) return true
  const late = await waitForLateOperation(error, options.cleanupTimeoutMs)
  const record = { operation, status: late.status }
  if (late.outcome) record.outcome = late.outcome
  if (late.error) record.error = errorRecord(late.error)
  sample.lateOperations.push(record)
  if (late.status === "pending") {
    pendingOperations.set(path, { operation, promise: error.lateOperation })
    deferCleanup(sample, operation)
    return false
  }
  return true
}

function finishSample(sample, payload) {
  sample.throughputMbps =
    sample.readSucceeded && sample.payloadVerified && sample.readMs > 0
      ? (payload.byteLength * 8) / (sample.readMs / 1000) / 1_000_000
      : null
  sample.status =
    sample.writeSucceeded &&
    sample.readSucceeded &&
    sample.payloadVerified === true &&
    sample.deleteSucceeded
      ? "ok"
      : "failed"
  sample.success = sample.status === "ok"
  sample.timeoutCount = sample.timeoutOperations.length
  return sample
}

export async function runSample({
  definition,
  filesystem,
  payload,
  path,
  iteration,
  options,
  ownedPaths,
  pendingOperations,
}) {
  const sample = emptySample(definition, payload.byteLength, iteration, path)
  ownedPaths.add(path)

  let writeCompleted = false
  let operationPending = false
  const writeStarted = performance.now()
  try {
    await withTimeout(() => filesystem.writeFile(path, payload), options.timeoutMs, "write")
    sample.writeMs = performance.now() - writeStarted
    sample.uploadMs = sample.writeMs
    sample.writeSucceeded = true
    writeCompleted = true
  } catch (error) {
    sample.writeMs = performance.now() - writeStarted
    sample.uploadMs = sample.writeMs
    firstFailure(sample, "write", error)
    operationPending = !(await observeLateOperation(
      sample,
      "write",
      error,
      options,
      path,
      pendingOperations,
    ))
  }

  if (operationPending) return finishSample(sample, payload)

  if (writeCompleted) {
    const readStarted = performance.now()
    try {
      const returned = bytes(await withTimeout(() => filesystem.readFile(path), options.timeoutMs, "read"))
      sample.readMs = performance.now() - readStarted
      sample.downloadMs = sample.readMs
      sample.readSucceeded = true
      sample.bytesReturned = returned.byteLength

      // This comparison intentionally starts after the download timer stops.
      sample.payloadVerified = sameBytes(returned, payload)
      if (!sample.payloadVerified) {
        const mismatch = new Error(
          `returned byte validation failed: expected ${payload.byteLength}, got ${returned.byteLength}`,
        )
        mismatch.code = "PAYLOAD_MISMATCH"
        firstFailure(sample, "verify", mismatch)
      }
    } catch (error) {
      sample.readMs = performance.now() - readStarted
      sample.downloadMs = sample.readMs
      firstFailure(sample, "read", error)
      operationPending = !(await observeLateOperation(
        sample,
        "read",
        error,
        options,
        path,
        pendingOperations,
      ))
    }

    if (operationPending) return finishSample(sample, payload)

    const deleteStarted = performance.now()
    try {
      await withTimeout(() => filesystem.unlink(path), options.cleanupTimeoutMs, "delete")
      sample.deleteMs = performance.now() - deleteStarted
      sample.deleteSucceeded = true
      ownedPaths.delete(path)
    } catch (error) {
      sample.deleteMs = performance.now() - deleteStarted
      firstFailure(sample, "delete", error)
      operationPending = !(await observeLateOperation(
        sample,
        "delete",
        error,
        options,
        path,
        pendingOperations,
      ))
    }
  }

  if (operationPending) return finishSample(sample, payload)

  if (!sample.deleteSucceeded) {
    const cleanup = await attemptCleanup(
      filesystem,
      path,
      options.cleanupTimeoutMs,
      pendingOperations,
    )
    sample.cleanupMs = cleanup.ms
    sample.cleanupSucceeded = cleanup.succeeded
    if (cleanup.succeeded) {
      ownedPaths.delete(path)
    } else {
      sample.cleanupFailure = true
      firstFailure(sample, "cleanup", cleanup.error || new Error("cleanup failed"))
      if (cleanup.timedOut && !sample.timeoutOperations.includes("cleanup")) {
        sample.timedOut = true
        sample.timeoutOperations.push("cleanup")
      }
    }
  } else {
    sample.cleanupSucceeded = true
  }

  return finishSample(sample, payload)
}

function values(samples, key, predicate = () => true) {
  return samples
    .filter((sample) => predicate(sample) && Number.isFinite(sample[key]))
    .map((sample) => sample[key])
}

async function runSize(
  definition,
  filesystem,
  options,
  context,
  sizeMiBValue,
  payload,
  ownedPaths,
  pendingOperations,
) {
  const measurementStarted = performance.now()
  const sizeBytes = payload.byteLength
  const tasks = Array.from({ length: options.iterations }, (_, index) => ({
    iteration: index + 1,
    path: benchmarkPath(context.runId, definition.id, sizeMiBValue, index + 1),
  }))
  const samples = await runWorkers(options.iterations === 0 ? [] : tasks, options.concurrency, (task, slot) =>
    runSample({
      definition,
      filesystem,
      payload,
      path: task.path,
      iteration: task.iteration,
      options,
      ownedPaths,
      pendingOperations,
    }).then((sample) => ({ ...sample, concurrencySlot: slot })),
  )

  const writeValues = values(samples, "writeMs", (sample) => sample.writeSucceeded)
  const readValues = values(samples, "readMs", (sample) => sample.readSucceeded)
  const throughputValues = values(
    samples,
    "throughputMbps",
    (sample) => sample.readSucceeded && sample.payloadVerified === true,
  )
  const deleteValues = values(samples, "deleteMs", (sample) => sample.deleteSucceeded)
  const successfulIterations = samples.filter((sample) => sample.success).length
  const timeoutCount = samples.reduce((total, sample) => total + sample.timeoutCount, 0)
  const cleanupFailureCount = samples.filter((sample) => sample.cleanupFailure).length
  const elapsedMs = performance.now() - measurementStarted
  const operationsPerLifecycle = 3
  const successfulOperations = successfulIterations * operationsPerLifecycle
  const attemptedOperations = samples.length * operationsPerLifecycle
  const iops = elapsedMs > 0 ? successfulOperations / (elapsedMs / 1000) : 0
  const iopsTargetMet = options.minIops == null ? null : iops >= options.minIops
  const iopsTargetFailure = iopsTargetMet === false
  const summary = {
    writeMs: computeStats(writeValues),
    readMs: computeStats(readValues),
    // Keep the ComputeSDK names alongside the filesystem-operation names.
    uploadMs: computeStats(writeValues),
    downloadMs: computeStats(readValues),
    throughputMbps: computeStats(throughputValues),
    deleteMs: computeStats(deleteValues),
    successRate: successfulIterations / options.iterations,
    successfulIterations,
    failedIterations: samples.length - successfulIterations,
    elapsedMs,
    operationsPerLifecycle,
    successfulOperations,
    attemptedOperations,
    iops,
    iopsTarget: options.minIops ?? null,
    iopsTargetMet,
    timeoutCount,
    cleanupFailureCount,
    operationSuccess: {
      write: writeValues.length,
      read: readValues.length,
      delete: deleteValues.length,
      verifiedReads: throughputValues.length,
    },
    statSampleCounts: {
      writeMs: writeValues.length,
      readMs: readValues.length,
      throughputMbps: throughputValues.length,
      deleteMs: deleteValues.length,
    },
  }

  return {
    ...providerSummary(definition, options.chunkSizeBytes),
    provider: definition.id,
    sizeMiB: sizeMiBValue,
    fileSizeBytes: sizeBytes,
    iterationsRequested: options.iterations,
    concurrency: options.concurrency,
    status: successfulIterations === samples.length && !iopsTargetFailure ? "ok" : "failed",
    payloadSha256: digest(payload),
    payloadPreparation: "excluded-from-timed-operations",
    payloadVerification: "outside-read-timer",
    chunkSizeBytes:
      definition.chunking.algorithm === "fixed-size" ? options.chunkSizeBytes : null,
    ...(iopsTargetFailure
      ? {
          failures: [
            {
              operation: "iops-target",
              error: {
                name: "IopsTargetError",
                message: `successful lifecycle IOPS ${iops.toFixed(2)} was below target ${options.minIops}`,
                code: "IOPS_TARGET_NOT_MET",
                actual: iops,
                target: options.minIops,
              },
            },
          ],
        }
      : {}),
    summary,
    summaryRounded: {
      writeMs: roundStats(summary.writeMs),
      readMs: roundStats(summary.readMs),
      uploadMs: roundStats(summary.uploadMs),
      downloadMs: roundStats(summary.downloadMs),
      throughputMbps: roundStats(summary.throughputMbps),
      deleteMs: roundStats(summary.deleteMs),
      elapsedMs: round(summary.elapsedMs),
      iops: round(summary.iops),
    },
    rawSamples: samples,
  }
}

export async function cleanupOwnedPaths(filesystem, ownedPaths, pendingOperations, timeoutMs) {
  const failures = []
  const attempted = ownedPaths.size
  for (const path of [...ownedPaths]) {
    const pending = pendingOperations.get(path)
    if (pending) {
      const late = await waitForPromiseSettlement(pending.promise, timeoutMs)
      if (late.status === "pending") {
        failures.push({
          path,
          pending: true,
          operation: pending.operation,
          error: {
            name: "CleanupDeferredError",
            message: `cleanup deadline reached while ${pending.operation} was still pending`,
            code: "CLEANUP_DEFERRED",
          },
          timedOut: true,
        })
        continue
      }
      pendingOperations.delete(path)
    }
    const result = await attemptCleanup(filesystem, path, timeoutMs, pendingOperations)
    if (result.succeeded) {
      ownedPaths.delete(path)
    } else {
      failures.push({
        path,
        error: result.error,
        timedOut: result.timedOut,
        ...(result.pending ? { pending: true } : {}),
      })
    }
  }
  return {
    attempted,
    remaining: ownedPaths.size,
    failures,
    pendingOperations: [...pendingOperations].map(([path, pending]) => ({
      path,
      operation: pending.operation,
    })),
  }
}

function skippedSizeResult(definition, sizeMiBValue, options, availability) {
  return {
    ...providerSummary(definition, options.chunkSizeBytes),
    provider: definition.id,
    chunkSizeBytes:
      definition.chunking.algorithm === "fixed-size" ? options.chunkSizeBytes : null,
    sizeMiB: sizeMiBValue,
    fileSizeBytes: options.payloadBytes ?? sizeMiBValue * 1024 * 1024,
    iterationsRequested: options.iterations,
    concurrency: options.concurrency,
    status: "skipped",
    skipped: true,
    skipReason: availability.reason,
    missingConfiguration: availability.missing || [],
    summary: null,
    rawSamples: [],
  }
}

function failedSizeResult(definition, sizeMiBValue, options, operation, error) {
  return {
    ...providerSummary(definition, options.chunkSizeBytes),
    provider: definition.id,
    chunkSizeBytes:
      definition.chunking.algorithm === "fixed-size" ? options.chunkSizeBytes : null,
    sizeMiB: sizeMiBValue,
    fileSizeBytes: options.payloadBytes ?? sizeMiBValue * 1024 * 1024,
    iterationsRequested: options.iterations,
    concurrency: options.concurrency,
    status: "failed",
    summary: null,
    rawSamples: [],
    failures: [{ operation, error: errorRecord(error) }],
  }
}

async function runProvider(definition, options, context) {
  const availability = definition.availability(context.environment)
  const providerRun = {
    ...providerSummary(definition, options.chunkSizeBytes),
    provider: definition.id,
    chunkSizeBytes:
      definition.chunking.algorithm === "fixed-size" ? options.chunkSizeBytes : null,
    status: availability.configured ? "pending" : "skipped",
    requiredEnvVars: definition.requiredEnvVars || [],
    missingConfiguration: availability.missing || [],
    ...(availability.source ? { oracleSource: availability.source } : {}),
    ...(availability.source
      ? {
          oracleRevision: {
            expected: MOUNTX_PINNED_REVISION,
            tested: availability.sourceRevision || null,
            verified: availability.sourceRevisionVerified === true,
            matchesPinned: availability.revisionMatchesPinned ?? null,
          },
        }
      : {}),
    sizes: [],
    cleanup: {
      pathsAttempted: 0,
      remainingPaths: 0,
      failures: [],
      resource: "not-started",
    },
  }
  const sizeResults = []

  if (availability.revisionMismatch) {
    providerRun.status = "failed"
    providerRun.oracleRevisionMismatch = true
    providerRun.revisionFailure = availability.reason
    providerRun.sizes = options.sizes.map((size) =>
      failedSizeResult(
        definition,
        size,
        options,
        "oracle-revision",
        new Error(availability.reason),
      ),
    )
    return { providerRun, sizeResults: providerRun.sizes }
  }

  if (!availability.configured) {
    providerRun.skipReason = availability.reason || "provider configuration is absent"
    providerRun.sizes = options.sizes.map((size) => skippedSizeResult(definition, size, options, availability))
    providerRun.status = "skipped"
    return { providerRun, sizeResults: providerRun.sizes }
  }

  let opened
  const setupStarted = performance.now()
  try {
    opened = await withTimeout(
      () => definition.create({ ...context, options }),
      options.timeoutMs,
      "provider setup",
    )
    providerRun.setupMs = performance.now() - setupStarted
  } catch (error) {
    providerRun.setupMs = performance.now() - setupStarted
    providerRun.status = "failed"
    providerRun.setupError = errorRecord(error)
    providerRun.sizes = options.sizes.map((size) => failedSizeResult(definition, size, options, "setup", error))
    return { providerRun, sizeResults: providerRun.sizes }
  }

  const ownedPaths = new Set()
  const pendingOperations = new Map()
  try {
    for (const sizeMiBValue of options.sizes) {
      try {
        // Payload allocation and hashing happen before the first timed write.
        const payload = makePayload(
          options.payloadBytes ?? sizeMiBValue * 1024 * 1024,
          options.payloadSeed,
        )
        const result = await runSize(
          definition,
          opened.filesystem,
          options,
          context,
          sizeMiBValue,
          payload,
          ownedPaths,
          pendingOperations,
        )
        sizeResults.push(result)
      } catch (error) {
        sizeResults.push(failedSizeResult(definition, sizeMiBValue, options, "benchmark", error))
      }
    }
  } finally {
    const pathCleanup = await cleanupOwnedPaths(
      opened.filesystem,
      ownedPaths,
      pendingOperations,
      options.cleanupTimeoutMs,
    )
    providerRun.cleanup.pathsAttempted = pathCleanup.attempted
    providerRun.cleanup.remainingPaths = pathCleanup.remaining
    providerRun.cleanup.failures = pathCleanup.failures
    providerRun.cleanup.pendingOperations = pathCleanup.pendingOperations

    const resourceCleanupStarted = performance.now()
    if (pendingOperations.size > 0) {
      providerRun.cleanup.resource = {
        status: "deferred",
        ms: performance.now() - resourceCleanupStarted,
        reason: "provider shutdown deferred while a timed-out native operation remains pending",
        pendingOperations: [...pendingOperations].map(([path, pending]) => ({
          path,
          operation: pending.operation,
        })),
      }
    } else {
      try {
        await withTimeout(opened.cleanup(), options.cleanupTimeoutMs, "provider cleanup")
        providerRun.cleanup.resource = {
          status: "ok",
          ms: performance.now() - resourceCleanupStarted,
        }
      } catch (error) {
        if (isTimeout(error)) {
          const late = await waitForLateOperation(error, options.cleanupTimeoutMs)
          providerRun.cleanup.resource = {
            // Late completion establishes cleanup, but does not erase the
            // configured deadline failure from the run's success criteria.
            status: late.status === "pending" ? "pending" : "failed",
            cleanupCompleted: late.status === "fulfilled",
            ms: performance.now() - resourceCleanupStarted,
            error: errorRecord(error),
            lateOperation: late.status,
          }
        } else {
          providerRun.cleanup.resource = {
            status: "failed",
            ms: performance.now() - resourceCleanupStarted,
            error: errorRecord(error),
          }
        }
      }
    }
  }

  const hasResultFailure = sizeResults.some((result) => result.status === "failed")
  const hasCleanupFailure =
    providerRun.cleanup.remainingPaths > 0 ||
    providerRun.cleanup.failures.length > 0 ||
    providerRun.cleanup.resource.status !== "ok"
  providerRun.status = hasResultFailure || hasCleanupFailure ? "failed" : "ok"
  providerRun.sizes = sizeResults
  return { providerRun, sizeResults }
}

async function gitMetadata(directory) {
  const read = async (arguments_) => {
    try {
      const result = await execFileAsync("git", arguments_, {
        cwd: directory,
        encoding: "utf8",
        maxBuffer: 1024 * 1024,
      })
      return result.stdout.trim()
    } catch {
      return null
    }
  }
  const revision = await read(["rev-parse", "HEAD"])
  const status = await read(["status", "--porcelain=v1", "--untracked-files=all"])
  return {
    path: directory,
    revision,
    revisionVerified: revision !== null,
    dirty: status === null ? null : status.length > 0,
    dirtyEntryCount: status === null ? null : status ? status.split("\n").length : 0,
    statusOutput: "omitted; only dirty flag and entry count are recorded",
  }
}

async function nativeAddonMetadata() {
  let paths
  try {
    paths = loadedNativeAddonPaths()
  } catch (error) {
    return {
      status: "unavailable",
      sourceRevisionIsNotBinaryRevision: true,
      error: errorRecord(error),
      artifacts: [],
    }
  }
  if (paths.length === 0) {
    return {
      status: "not-loaded",
      sourceRevisionIsNotBinaryRevision: true,
      artifacts: [],
    }
  }
  const artifacts = []
  for (const path of paths) {
    try {
      const bytes = await readFile(path)
      artifacts.push({
        path,
        bytes: bytes.byteLength,
        sha256: createHash("sha256").update(bytes).digest("hex"),
      })
    } catch (error) {
      artifacts.push({ path, error: errorRecord(error) })
    }
  }
  return {
    status: artifacts.every((artifact) => artifact.sha256) ? "hashed" : "partial",
    sourceRevisionIsNotBinaryRevision: true,
    artifacts,
  }
}

async function environmentRecord(options, environment) {
  return {
    runtime: "node",
    node: process.version,
    nodeVersions: { ...process.versions },
    platform: platform(),
    arch: arch(),
    osRelease: release(),
    cpuCount: cpus().length,
    cwd: process.cwd(),
    networkContext: options.networkContext,
    remoteRegion: environment.MOUNT_RS_R2_REGION || environment.R2_REGION || null,
    credentialsRecorded: false,
    sourceControl: {
      mountRs: await gitMetadata(repoRoot),
      mountx: {
        expectedPinnedRevision: MOUNTX_PINNED_REVISION,
        checkout: await gitMetadata(mountxSourcePath(environment)),
      },
    },
    nativeAddon: await nativeAddonMetadata(),
  }
}

export async function runBenchmark(options, environment = process.env) {
  const definitions = providerById(environment)
  const selectedIds = options.providers || [...definitions.keys()]
  const unknown = selectedIds.filter((id) => !definitions.has(id))
  if (unknown.length > 0) {
    throw usageError(`unknown provider(s): ${unknown.join(", ")}`)
  }

  const runId = makeRunId()
  const context = {
    runId,
    environment,
    chunkSizeBytes: options.chunkSizeBytes,
  }
  const providerRuns = []
  const results = []
  for (const id of selectedIds) {
    const outcome = await runProvider(definitions.get(id), options, context)
    providerRuns.push(outcome.providerRun)
    results.push(...outcome.sizeResults)
  }

  const failedProviders = providerRuns.filter((providerRun) => providerRun.status === "failed")
  const skippedProviders = providerRuns.filter((providerRun) => providerRun.status === "skipped")
  const configurationFailures = options.requireConfigured
    ? skippedProviders.map((providerRun) => ({
        provider: providerRun.provider,
        missingConfiguration: providerRun.missingConfiguration,
        reason: providerRun.skipReason,
      }))
    : []
  return {
    schemaVersion: "mount-rs.storage-benchmark.v1",
    status: failedProviders.length > 0 || configurationFailures.length > 0 ? "failed" : "ok",
    generatedAt: new Date().toISOString(),
    runId,
    reference: {
      suite: "computesdk/benchmarks/storage",
      revision: REFERENCE_REVISION,
      url: `https://github.com/computesdk/benchmarks/tree/${REFERENCE_REVISION}/benchmarks/storage`,
      lifecycle: "upload/write -> full-byte download/read -> delete/unlink",
      statistics: "median/p95/p99 after the reference five-percent trim",
    },
    mode: options.mode,
    config: {
      sizesMiB: options.sizes,
      payloadSizesBytes: options.sizes.map(
        (size) => options.payloadBytes ?? size * 1024 * 1024,
      ),
      iterations: options.iterations,
      concurrency: options.concurrency,
      timeoutMs: options.timeoutMs,
      cleanupTimeoutMs: options.cleanupTimeoutMs,
      chunkSizeBytes: options.chunkSizeBytes,
      payloadBytes: options.payloadBytes ?? null,
      minIops: options.minIops ?? null,
      requireConfigured: options.requireConfigured,
      iopsDefinition: "successful write+read+delete lifecycle operations divided by measured lifecycle wall time",
      payloadSeed: options.payloadSeed,
      setupExcludedFromTimings: true,
      payloadVerificationExcludedFromReadTimings: true,
    },
    environment: await environmentRecord(options, environment),
    measurementSurfaces: {
      directApi: "measured",
      nodeCaller: "measured",
      nativeAddon: "provider-dependent; see each executionSurface",
      directRust: "not-run",
      mountedPath: "not-run",
      mountedPathReason: "This dependency-light runner does not require privileged OS mount setup.",
    },
    snapshotFork: {
      status: "deferred",
      reason: "The reference snapshot/fork workload is tracked for future copy-on-write support; this runner does not emulate it with full copies.",
      referenceRevision: REFERENCE_REVISION,
    },
    counts: {
      providersRequested: providerRuns.length,
      providersFailed: failedProviders.length,
      providersSkipped: skippedProviders.length,
      configurationFailures: configurationFailures.length,
      sizeResults: results.length,
      sizeResultsFailed: results.filter((result) => result.status === "failed").length,
      sizeResultsSkipped: results.filter((result) => result.status === "skipped").length,
    },
    providers: providerRuns,
    configurationFailures,
    results,
  }
}

async function writeOutput(output, outputPath) {
  const text = `${JSON.stringify(output, null, 2)}\n`
  if (!outputPath) {
    process.stdout.write(text)
    return
  }
  const resolved = resolve(process.cwd(), outputPath)
  await mkdir(dirname(resolved), { recursive: true })
  await writeFile(resolved, text, "utf8")
  process.stdout.write(text)
  process.stderr.write(`storage benchmark JSON written to ${resolved}\n`)
}

export async function main(argv = process.argv.slice(2)) {
  let options
  try {
    options = parseArgs(argv)
  } catch (error) {
    process.stderr.write(`${error.message}\n\n${helpText()}`)
    return 2
  }
  if (options.help) {
    process.stdout.write(helpText())
    return 0
  }

  try {
    const output = await runBenchmark(options)
    await writeOutput(output, options.output)
    return output.status === "failed" ? 1 : 0
  } catch (error) {
    process.stderr.write(`storage benchmark failed: ${errorRecord(error).message}\n`)
    return error && error.code === "BENCHMARK_USAGE" ? 2 : 1
  }
}

const invokedPath = process.argv[1] ? resolve(process.argv[1]) : null
if (invokedPath && invokedPath === fileURLToPath(import.meta.url)) {
  main().then((code) => {
    process.exitCode = code
  })
}
