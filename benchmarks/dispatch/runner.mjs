#!/usr/bin/env node

/**
 * Cross-boundary dispatch baseline runner.
 *
 * The Rust child emits direct-generic and Arc<dyn FsDriver> lanes. This file
 * drives the same small operations through the built N-API package, then
 * combines both raw sample sets without rounding. No adapter optimization is
 * performed here; the generic Rust receiver is intentionally still boxed by
 * async-trait.
 */

import { createHash } from "node:crypto"
import { createRequire } from "node:module"
import { fileURLToPath } from "node:url"
import { dirname, resolve } from "node:path"
import { mkdir, readFile, writeFile } from "node:fs/promises"
import { execFileSync, spawnSync } from "node:child_process"
import { computeStats } from "../storage/stats.mjs"

const runnerDirectory = dirname(fileURLToPath(import.meta.url))
const repoRoot = resolve(runnerDirectory, "../..")
const requireFromBenchmark = createRequire(import.meta.url)
const DEFAULT_RUST_TIMEOUT_MS = 120_000
const DEFAULT_NODE_CLEANUP_TIMEOUT_MS = 10_000

const profiles = {
  smoke: {
    warmup: 100,
    iterations: 500,
    samples: 5,
    concurrency: [1],
    payloadSizes: [32, 4096],
    chunkSizes: [4096],
    operations: ["stat", "open_close", "read", "write"],
  },
  thorough: {
    warmup: 1000,
    iterations: 2000,
    samples: 7,
    concurrency: [1, 4, 16],
    payloadSizes: [32, 4096, 65536],
    chunkSizes: [4096, 65536],
    operations: ["stat", "open_close", "read", "write"],
  },
}

function usage() {
  return [
    "Usage: node benchmarks/dispatch/runner.mjs [options]",
    "",
    "Profiles:",
    "  --smoke                         Practical default profile",
    "  --thorough                      Larger payload/concurrency/chunk sweep",
    "",
    "Overrides:",
    "  --warmup N                      Warmup iterations per worker",
    "  --iterations N                  Timed iterations per worker",
    "  --samples N                     Timed samples",
    "  --concurrency N[,N...]          Worker counts",
    "  --payload-sizes N[,N...]        Payload sizes in bytes",
    "  --chunk-sizes N[,N...]          Fixed chunk sizes in bytes",
    "  --operations A[,B...]           stat,open_close,read,write",
    "  --rust-timeout-ms N             Outer cargo child timeout (default 120000)",
    "  --cleanup-timeout-ms N          N-API shutdown timeout (default 10000)",
    "  --output PATH                   Also write the raw JSON report",
    "  --skip-rust                     Do not run the Rust child",
    "  --skip-node                     Do not run the N-API lane",
    "  --help                          Show this message",
  ].join("\n")
}

function positive(value, name) {
  const parsed = Number(value)
  if (!Number.isInteger(parsed) || parsed <= 0) {
    throw new Error(name + " must be a positive integer: " + value)
  }
  return parsed
}

function timeoutValue(value, name) {
  return positive(value, name)
}

function list(value, name) {
  const values = value.split(",").map((entry) => positive(entry.trim(), name))
  if (values.length === 0) throw new Error(name + " must not be empty")
  return values
}

function operations(value) {
  const allowed = new Set(["stat", "open_close", "read", "write"])
  const values = value.split(",").map((entry) => entry.trim())
  if (values.length === 0 || values.some((entry) => !allowed.has(entry))) {
    throw new Error("operations must contain only stat, open_close, read, write: " + value)
  }
  return values
}

function parseArgs(argv) {
  let profileName = "smoke"
  let output
  let skipRust = false
  let skipNode = false
  let rustTimeoutMs = DEFAULT_RUST_TIMEOUT_MS
  let cleanupTimeoutMs = DEFAULT_NODE_CLEANUP_TIMEOUT_MS
  const overrides = new Map()

  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index]
    if (argument === "--help") {
      console.log(usage())
      process.exit(0)
    }
    if (argument === "--smoke" || argument === "--thorough") {
      profileName = argument.slice(2)
      continue
    }
    if (argument === "--skip-rust") {
      skipRust = true
      continue
    }
    if (argument === "--skip-node") {
      skipNode = true
      continue
    }
    if (argument === "--rust-timeout-ms" || argument === "--cleanup-timeout-ms") {
      const value = argv[++index]
      if (!value) throw new Error(argument + " requires a value")
      if (argument === "--rust-timeout-ms") rustTimeoutMs = timeoutValue(value, argument)
      else cleanupTimeoutMs = timeoutValue(value, argument)
      continue
    }
    if (argument === "--output") {
      output = argv[++index]
      if (!output) throw new Error("--output requires a path")
      continue
    }
    const match =
      /^(--warmup|--iterations|--samples|--concurrency|--payload-sizes|--chunk-sizes|--operations|--rust-timeout-ms|--cleanup-timeout-ms)=(.*)$/.exec(
        argument,
      )
    if (match) {
      overrides.set(match[1], match[2])
      continue
    }
    if (/^--(warmup|iterations|samples|concurrency|payload-sizes|chunk-sizes|operations)$/.test(argument)) {
      const value = argv[++index]
      if (!value) throw new Error(argument + " requires a value")
      overrides.set(argument, value)
      continue
    }
    throw new Error("unknown argument " + argument + "; use --help")
  }

  const profile = profiles[profileName]
  if (!profile) throw new Error("unknown profile " + profileName)
  const config = {
    warmup: profile.warmup,
    iterations: profile.iterations,
    samples: profile.samples,
    concurrency: [...profile.concurrency],
    payloadSizes: [...profile.payloadSizes],
    chunkSizes: [...profile.chunkSizes],
    operations: [...profile.operations],
  }
  for (const [key, value] of overrides) {
    if (key === "--warmup") config.warmup = positive(value, "warmup")
    if (key === "--iterations") config.iterations = positive(value, "iterations")
    if (key === "--samples") config.samples = positive(value, "samples")
    if (key === "--concurrency") config.concurrency = list(value, "concurrency")
    if (key === "--payload-sizes") config.payloadSizes = list(value, "payload-sizes")
    if (key === "--chunk-sizes") config.chunkSizes = list(value, "chunk-sizes")
    if (key === "--operations") config.operations = operations(value)
    if (key === "--rust-timeout-ms") rustTimeoutMs = timeoutValue(value, key)
    if (key === "--cleanup-timeout-ms") cleanupTimeoutMs = timeoutValue(value, key)
  }
  return {
    profileName,
    config,
    output,
    skipRust,
    skipNode,
    rustTimeoutMs,
    cleanupTimeoutMs,
  }
}

function makeError(message, fields = {}) {
  const error = new Error(message)
  Object.assign(error, fields)
  return error
}

function errorRecord(error) {
  const record = {
    name: error && typeof error.name === "string" ? error.name : "Error",
    message: error && typeof error.message === "string" ? error.message : String(error),
  }
  for (const key of ["code", "operation", "timeoutMs", "signal", "status"]) {
    if (error && error[key] !== undefined && error[key] !== null) record[key] = error[key]
  }
  if (error && error.errors) record.errors = error.errors
  if (error && error.cleanupError) record.cleanupError = error.cleanupError
  return record
}

function gitMetadata(directory) {
  const read = (arguments_) => {
    try {
      return (
        execFileSync("git", arguments_, {
          cwd: directory,
          encoding: "utf8",
          stdio: ["ignore", "pipe", "ignore"],
          timeout: 5_000,
        }).trim() || null
      )
    } catch {
      return null
    }
  }
  const gitHead = read(["rev-parse", "HEAD"])
  const status = read(["status", "--porcelain=v1", "--untracked-files=all"])
  return {
    path: directory,
    gitHead,
    gitHeadVerified: gitHead !== null,
    dirty: status === null ? null : status.length > 0,
    dirtyEntryCount: status === null ? null : status ? status.split("\n").length : 0,
    statusOutput: "omitted; only gitHead, dirty, and dirtyEntryCount are recorded",
  }
}

function loadedNativeAddonPaths() {
  return Object.keys(requireFromBenchmark.cache)
    .filter((path) => path.endsWith(".node"))
    .sort()
}

async function nativeAddonMetadata(napi) {
  const bindingTarget = napi ? napi.__napiBindingTarget || "unknown" : "not-loaded"
  if (!napi) {
    return {
      status: "not-loaded",
      bindingTarget,
      actualAddonSha256: null,
      sourceRevisionIsNotBinaryRevision: true,
      artifacts: [],
    }
  }

  const paths = loadedNativeAddonPaths()
  if (paths.length === 0) {
    if (bindingTarget === "native") {
      return {
        status: "error",
        bindingTarget,
        actualAddonSha256: null,
        sourceRevisionIsNotBinaryRevision: true,
        artifacts: [],
        error: {
          name: "AddonHashUnavailable",
          message: "native N-API binding loaded but no loaded .node artifact was found",
        },
      }
    }
    return {
      status: "not-applicable",
      bindingTarget,
      actualAddonSha256: null,
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
  const failed = artifacts.some((artifact) => !artifact.sha256)
  return {
    status: failed ? "error" : "hashed",
    bindingTarget,
    actualAddonSha256:
      !failed && artifacts.length === 1 ? artifacts[0].sha256 : null,
    sourceRevisionIsNotBinaryRevision: true,
    artifacts,
  }
}

async function collectProvenance(napi) {
  const sourceControl = gitMetadata(repoRoot)
  if (!sourceControl.gitHeadVerified || sourceControl.dirty === null) {
    throw makeError("git provenance could not be captured", {
      code: "PROVENANCE_INCOMPLETE",
      sourceControl,
    })
  }
  const nativeAddon = await nativeAddonMetadata(napi)
  if (nativeAddon.status === "error") {
    throw makeError("actual N-API addon SHA-256 could not be captured", {
      code: "PROVENANCE_INCOMPLETE",
      nativeAddon,
    })
  }
  return {
    status: "complete",
    capturedBeforeMeasurements: true,
    sourceControl,
    nativeAddon,
  }
}

function withTimeout(operation, timeoutMs, operationName) {
  const started = Promise.resolve().then(() =>
    typeof operation === "function" ? operation() : operation,
  )
  const observed = started.then(
    (value) => ({ status: "fulfilled", value }),
    (error) => ({ status: "rejected", error }),
  )
  return new Promise((resolve, reject) => {
    let timedOut = false
    const timer = setTimeout(() => {
      timedOut = true
      reject(
        makeError(operationName + " timed out after " + timeoutMs + " ms", {
          code: "BENCHMARK_TIMEOUT",
          operation: operationName,
          timeoutMs,
        }),
      )
    }, timeoutMs)
    observed.then((result) => {
      if (timedOut) return
      clearTimeout(timer)
      if (result.status === "fulfilled") resolve(result.value)
      else reject(result.error)
    })
  })
}

function rustArgs(config) {
  return [
    "run",
    "--locked",
    "--release",
    "--quiet",
    "--example",
    "dispatch_baseline",
    "--",
    "--warmup",
    String(config.warmup),
    "--iterations",
    String(config.iterations),
    "--samples",
    String(config.samples),
    "--concurrency",
    config.concurrency.join(","),
    "--payload-sizes",
    config.payloadSizes.join(","),
    "--chunk-sizes",
    config.chunkSizes.join(","),
    "--operations",
    config.operations.join(","),
  ]
}

function runRust(config, rustTimeoutMs) {
  const child = spawnSync("cargo", rustArgs(config), {
    cwd: repoRoot,
    encoding: "utf8",
    maxBuffer: 128 * 1024 * 1024,
    timeout: rustTimeoutMs,
    killSignal: "SIGTERM",
  })
  if (child.error) {
    if (child.error.code === "ETIMEDOUT") {
      throw makeError("Rust cargo child timed out", {
        code: "BENCHMARK_TIMEOUT",
        operation: "cargo run --locked --example dispatch_baseline",
        timeoutMs: rustTimeoutMs,
        signal: child.signal || "SIGTERM",
      })
    }
    throw makeError("could not start cargo: " + child.error.message, {
      code: child.error.code || "RUST_SPAWN_FAILED",
    })
  }
  if (child.signal) {
    throw makeError("Rust cargo child terminated by " + child.signal, {
      code: "RUST_CHILD_SIGNAL",
      signal: child.signal,
      status: child.status,
    })
  }
  if (child.status !== 0) {
    const stderr = (child.stderr || "").trim()
    throw makeError(
      "Rust dispatch baseline failed (exit " +
        child.status +
        ")" +
        (stderr ? ":\n" + stderr : ""),
      {
        code: "RUST_BASELINE_FAILED",
        status: child.status,
      },
    )
  }
  try {
    return JSON.parse(child.stdout)
  } catch (error) {
    throw new Error("Rust dispatch baseline emitted invalid JSON: " + error.message)
  }
}

function loadNapi() {
  try {
    return createRequire(import.meta.url)(resolve(repoRoot, "integrations/mount-rs-napi/index.js"))
  } catch (error) {
    throw new Error(
      "could not load integrations/mount-rs-napi/index.js; build the existing N-API artifact first: " +
        error.message,
    )
  }
}

async function validateNodeFile(filesystem, path, payload) {
  const contents = Buffer.from(await filesystem.readFile(path))
  if (contents.length !== payload.length || !contents.equals(payload)) {
    throw new Error(
      "N-API validation mismatch for " +
        path +
        ": got " +
        contents.length +
        ", expected " +
        payload.length,
    )
  }
}

async function closeNodeHandles(handles) {
  await Promise.all(handles.map((handle) => handle.close()))
}

async function nodeWorker({ filesystem, operation, path, payload, handle, buffer, iterations }) {
  for (let iteration = 0; iteration < iterations; iteration += 1) {
    if (operation === "stat") {
      await filesystem.stat(path)
    } else if (operation === "open_close") {
      const opened = await filesystem.open(path, "r")
      await opened.close()
    } else if (operation === "read") {
      const result = await handle.read(buffer, 0, payload.length, 0)
      buffer = result.buffer
      if (result.bytesRead !== payload.length) {
        throw new Error("N-API read returned " + result.bytesRead + ", expected " + payload.length)
      }
    } else if (operation === "write") {
      const result = await handle.write(payload, 0, payload.length, 0)
      buffer = result.buffer
      if (result.bytesWritten !== payload.length) {
        throw new Error(
          "N-API write returned " + result.bytesWritten + ", expected " + payload.length,
        )
      }
    } else {
      throw new Error("unsupported operation " + operation)
    }
  }
  return buffer
}

async function runNodeBatch({ filesystem, operation, path, payload, handles, iterations, concurrency }) {
  const buffers = Array.from(
    { length: concurrency },
    () =>
      operation === "read" || operation === "write"
        ? Buffer.alloc(payload.length)
        : Buffer.alloc(0),
  )
  const started = process.hrtime.bigint()
  if (concurrency === 1) {
    await nodeWorker({
      filesystem,
      operation,
      path,
      payload,
      handle: handles[0],
      buffer: buffers[0],
      iterations,
    })
  } else {
    await Promise.all(
      Array.from({ length: concurrency }, (_, index) =>
        nodeWorker({
          filesystem,
          operation,
          path,
          payload,
          handle: handles[index],
          buffer: buffers[index],
          iterations,
        }),
      ),
    )
  }
  const durationNs = Number(process.hrtime.bigint() - started)
  return {
    durationNs,
    operations: iterations * concurrency,
  }
}

function nodeSample({ operation, payloadBytes, concurrency, sampleIndex, iterations, batch }) {
  const nsPerOperation = batch.durationNs === 0 ? 0 : batch.durationNs / batch.operations
  const bytesPerSecond =
    batch.durationNs > 0 && (operation === "read" || operation === "write")
      ? (payloadBytes * batch.operations * 1e9) / batch.durationNs
      : null
  return {
    operation,
    payloadBytes,
    concurrency,
    sampleIndex,
    iterationsPerWorker: iterations,
    operations: batch.operations,
    durationNs: batch.durationNs,
    nsPerOperation,
    operationsPerSecond: batch.durationNs === 0 ? 0 : (batch.operations * 1e9) / batch.durationNs,
    bytesPerSecond,
    allocationCount: null,
    deallocationCount: null,
    reallocationCount: null,
    allocatedBytes: null,
    deallocatedBytes: null,
    allocationMeasurement: "unavailable-js-v8",
  }
}

async function runNodeLane({ filesystem, backend, chunkSize, config }) {
  const samples = []
  for (const payloadSize of config.payloadSizes) {
    const path = "/dispatch-baseline-" + payloadSize
    const payload = Buffer.alloc(payloadSize, 0x5a)
    await filesystem.writeFile(path, payload)

    for (const operation of config.operations) {
      for (const concurrency of config.concurrency) {
        const handles =
          operation === "read" || operation === "write"
            ? await Promise.all(
                Array.from({ length: concurrency }, () =>
                  filesystem.open(path, operation === "read" ? "r" : "r+"),
                ),
              )
            : []
        try {
          if (config.warmup > 0) {
            await runNodeBatch({
              filesystem,
              operation,
              path,
              payload,
              handles,
              iterations: config.warmup,
              concurrency,
            })
          }
          for (let sampleIndex = 0; sampleIndex < config.samples; sampleIndex += 1) {
            const batch = await runNodeBatch({
              filesystem,
              operation,
              path,
              payload,
              handles,
              iterations: config.iterations,
              concurrency,
            })
            samples.push(
              nodeSample({
                operation,
                payloadBytes: payload.length,
                concurrency,
                sampleIndex,
                iterations: config.iterations,
                batch,
              }),
            )
          }
        } finally {
          await closeNodeHandles(handles)
        }
      }
    }
    await validateNodeFile(filesystem, path, payload)
  }

  return {
    surface: "node",
    backend,
    dispatch: "napi-node",
    futureRepresentation: "N-API Promise over erased FsDriver",
    genericReceiverStatus: "not applicable",
    handleErasure: "N-API FileHandle wrapper over Arc<dyn FileHandle>",
    chunkSizeBytes: chunkSize,
    allocationMeasurement: "unavailable-js-v8",
    samples,
  }
}

async function shutdownNodeFilesystems(filesystems, timeoutMs) {
  const errors = []
  for (const filesystem of filesystems) {
    try {
      await withTimeout(
        () => filesystem.shutdown(),
        timeoutMs,
        "N-API adapter cleanup",
      )
    } catch (error) {
      errors.push(error)
    }
  }
  if (errors.length > 0) {
    throw makeError("N-API adapter cleanup failed", {
      code: "ADAPTER_CLEANUP_FAILED",
      errors: errors.map(errorRecord),
    })
  }
}

async function runNode(config, napi, cleanupTimeoutMs) {
  const { Filesystem, createChunkedDriver } = napi
  const lanes = []
  const filesystems = []
  const ownerSuffix = process.pid + "-" + Date.now()

  const memory = Filesystem.memory()
  filesystems.push(memory)
  try {
    lanes.push(await runNodeLane({ filesystem: memory, backend: "memory", chunkSize: null, config }))
    for (const chunkSize of config.chunkSizes) {
      const filesystem = await createChunkedDriver({
        metadata: { kind: "memory" },
        blocks: { kind: "memory" },
        chunkSize,
        owner: "dispatch-baseline-" + ownerSuffix + "-" + chunkSize,
        ttlMs: 30000,
      })
      filesystems.push(filesystem)
      lanes.push(
        await runNodeLane({
          filesystem,
          backend: "chunked-memory",
          chunkSize,
          config,
        }),
      )
    }
  } catch (error) {
    try {
      await shutdownNodeFilesystems(filesystems, cleanupTimeoutMs)
    } catch (cleanupError) {
      error.cleanupError = errorRecord(cleanupError)
    }
    throw error
  }
  let cleaned = false
  return {
    lanes,
    cleanup: async () => {
      if (cleaned) return
      cleaned = true
      await shutdownNodeFilesystems(filesystems, cleanupTimeoutMs)
    },
  }
}

function addSummaries(report) {
  const summaries = []
  for (const lane of report.lanes) {
    const groups = new Map()
    for (const sample of lane.samples) {
      const key = sample.operation + "|" + sample.payloadBytes + "|" + sample.concurrency
      const group = groups.get(key) || []
      group.push(sample.nsPerOperation)
      groups.set(key, group)
    }
    for (const [key, values] of groups) {
      const [operation, payloadBytes, concurrency] = key.split("|")
      summaries.push({
        surface: lane.surface,
        backend: lane.backend,
        dispatch: lane.dispatch,
        chunkSizeBytes: lane.chunkSizeBytes,
        operation,
        payloadBytes: Number(payloadBytes),
        concurrency: Number(concurrency),
        samples: values.length,
        ...computeStats(values),
      })
    }
  }
  return { ...report, summaries }
}

function buildReport({
  options,
  provenance,
  rust,
  nodeLanes,
  status,
  cleanup,
  error,
}) {
  const report = {
    schemaVersion: 1,
    producer: "benchmarks/dispatch/runner.mjs",
    status,
    profile: options.profileName,
    provenance,
    execution: {
      cargoLocked: true,
      rustTimeoutMs: options.rustTimeoutMs,
      cleanupTimeoutMs: options.cleanupTimeoutMs,
      measurementsSavedBeforeAdapterCleanup: Boolean(cleanup && options.output),
    },
    environment: {
      node: options.skipNode
        ? null
        : {
            nodeVersion: process.version,
            platform: process.platform,
            arch: process.arch,
            napiBindingTarget: provenance.nativeAddon.bindingTarget,
          },
      rust: rust ? rust.environment : null,
    },
    config: options.config,
    lanes: [...(rust ? rust.lanes : []), ...(nodeLanes || [])],
    notes: [
      "Raw duration and allocation fields are retained; summary percentiles are derived without rounding.",
      "Rust generic lanes use a concrete receiver but remain boxed by the async-trait contract.",
      "Node/V8 allocation counts are intentionally unavailable; no unsafe N-API buffer borrowing is used.",
    ],
  }
  if (cleanup) report.cleanup = cleanup
  if (error) report.error = errorRecord(error)
  return addSummaries(report)
}

function buildFailureReport(options, provenance, error) {
  return {
    schemaVersion: 1,
    producer: "benchmarks/dispatch/runner.mjs",
    status: "error",
    profile: options ? options.profileName : null,
    provenance: provenance || null,
    execution: options
      ? {
          cargoLocked: true,
          rustTimeoutMs: options.rustTimeoutMs,
          cleanupTimeoutMs: options.cleanupTimeoutMs,
          measurementsSavedBeforeAdapterCleanup: false,
        }
      : null,
    config: options ? options.config : null,
    measurements: "not-recorded",
    lanes: [],
    summaries: [],
    error: errorRecord(error),
    notes: [
      "No benchmark measurements are claimed because setup, execution, or cleanup failed.",
      "A failed child or timeout is reported as an error and is never converted into a timing sample.",
    ],
  }
}

async function saveReport(report, outputPath) {
  const text = JSON.stringify(report, null, 2)
  if (outputPath) {
    const resolved = resolve(process.cwd(), outputPath)
    await mkdir(dirname(resolved), { recursive: true })
    await writeFile(resolved, text + "\n", "utf8")
  }
  return text
}

function resultOutput(report, outputPath) {
  if (!outputPath) return JSON.stringify(report, null, 2)
  return JSON.stringify({
    status: report.status,
    output: resolve(process.cwd(), outputPath),
    cleanup: report.cleanup ? report.cleanup.status : null,
  })
}

async function main() {
  let options
  try {
    options = parseArgs(process.argv.slice(2))
  } catch (error) {
    const report = buildFailureReport(null, null, error)
    const text = await saveReport(report, undefined)
    process.stderr.write(text + "\n")
    process.exitCode = 2
    return
  }

  let napi = null
  let provenance = null
  let nodeRun = null
  try {
    if (!options.skipNode) napi = loadNapi()
    provenance = await collectProvenance(napi)
    const rust = options.skipRust ? null : runRust(options.config, options.rustTimeoutMs)
    nodeRun = options.skipNode
      ? null
      : await runNode(options.config, napi, options.cleanupTimeoutMs)

    const measuredReport = buildReport({
      options,
      provenance,
      rust,
      nodeLanes: nodeRun ? nodeRun.lanes : [],
      status: "measurements-complete-cleanup-pending",
      cleanup: { status: "pending" },
    })
    if (nodeRun) {
      // Persist the real samples while the adapter instances are still alive.
      // A later cleanup failure is therefore never mistaken for a missing run.
      await saveReport(measuredReport, options.output)
      await nodeRun.cleanup()
    }

    const finalReport = buildReport({
      options,
      provenance,
      rust,
      nodeLanes: nodeRun ? nodeRun.lanes : [],
      status: "ok",
      cleanup: { status: nodeRun ? "complete" : "not-required" },
    })
    const text = await saveReport(finalReport, options.output)
    process.stdout.write(
      (options.output ? resultOutput(finalReport, options.output) : text) + "\n",
    )
  } catch (error) {
    if (nodeRun) {
      try {
        await nodeRun.cleanup()
      } catch (cleanupError) {
        error.cleanupError = errorRecord(cleanupError)
      }
    }
    const failure = buildFailureReport(options, provenance, error)
    try {
      const text = await saveReport(failure, options && options.output)
      if (options && options.output) {
        process.stdout.write(resultOutput(failure, options.output) + "\n")
      } else {
        process.stderr.write(text + "\n")
      }
    } catch (saveError) {
      process.stderr.write(
        "dispatch baseline failed and error report could not be saved: " +
          (saveError.message || String(saveError)) +
        "\n",
      )
    }
    process.stderr.write(
      "dispatch baseline failed: " +
        (error.message || String(error)) +
        "\n",
    )
    process.exitCode = 1
  }
}

main().catch((error) => {
  console.error(error.stack || error.message || String(error))
  process.exitCode = 1
})
