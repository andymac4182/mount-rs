import { Buffer } from "node:buffer"

import { validateCompactLayoutReceipt } from "./compact-layout.mjs"
import { observeSplitNamespacePresence } from "./namespace-presence.mjs"

const CANARY_PATH = "/.mount-rs-owned-layout-canary.bin"
const CANARY_BYTES = 4096
const CHUNK_BYTES = 65536
const issuedErrors = new WeakSet()
const messages = Object.freeze({
  OWNED_LAYOUT_ARM_CONFIG_INVALID: "owned layout arm configuration is invalid",
  OWNED_LAYOUT_ARM_API_UNAVAILABLE: "owned layout arm public API is unavailable",
  OWNED_LAYOUT_ARM_STATE_INVALID: "owned layout arm lifecycle transition is invalid",
  OWNED_LAYOUT_ARM_PREFLIGHT_FAILED: "owned layout arm freshness observation failed",
  OWNED_LAYOUT_ARM_CREATE_FAILED: "owned layout arm initial construction failed",
  OWNED_LAYOUT_ARM_REOPEN_FAILED: "owned layout arm fresh construction failed",
  OWNED_LAYOUT_ARM_REOPEN_NOT_FRESH: "owned layout arm construction reused a prior handle",
  OWNED_LAYOUT_ARM_LAYOUT_PROOF_FAILED: "owned layout arm layout observation failed",
  OWNED_LAYOUT_ARM_GENERATION_CONTINUITY_FAILED: "owned layout arm layout continuity failed",
  OWNED_LAYOUT_ARM_CANARY_WRITE_FAILED: "owned layout arm canary write failed",
  OWNED_LAYOUT_ARM_CANARY_READ_FAILED: "owned layout arm canary byte verification failed",
  OWNED_LAYOUT_ARM_CANARY_EOF_FAILED: "owned layout arm canary EOF verification failed",
  OWNED_LAYOUT_ARM_CANARY_REMOVE_FAILED: "owned layout arm canary removal failed",
  OWNED_LAYOUT_ARM_EMPTY_PROOF_FAILED: "owned layout arm logical root is not verified empty",
  OWNED_LAYOUT_ARM_HANDLE_CLOSE_FAILED: "owned layout arm canary handle close failed",
  OWNED_LAYOUT_ARM_SHUTDOWN_FAILED: "owned layout arm filesystem shutdown failed",
})

function failure(code, cleanupCodes = []) {
  const error = Object.assign(new Error(messages[code]), { code })
  if (cleanupCodes.length) error.cleanupCodes = Object.freeze([...cleanupCodes])
  issuedErrors.add(error)
  return error
}

function ownFailure(error, fallback) {
  return issuedErrors.has(error) ? error : failure(fallback)
}

function withCleanupFailure(first, cleanup) {
  if (!first) return cleanup
  return failure(first.code, [...(first.cleanupCodes || []), cleanup.code, ...(cleanup.cleanupCodes || [])])
}

function fixedRecord(source, fields, code = "OWNED_LAYOUT_ARM_CONFIG_INVALID") {
  let descriptors
  try {
    if (!source || typeof source !== "object" || Array.isArray(source)) throw failure(code)
    descriptors = Object.getOwnPropertyDescriptors(source)
    if (Reflect.ownKeys(descriptors).length !== fields.length ||
        !fields.every((field) => Object.hasOwn(descriptors, field) && Object.hasOwn(descriptors[field], "value"))) throw failure(code)
  } catch { throw failure(code) }
  return Object.freeze(Object.fromEntries(fields.map((field) => [field, descriptors[field].value])))
}

function text(value, maximum) {
  return typeof value === "string" && value.length > 0 && !value.includes("\0") && Buffer.byteLength(value, "utf8") <= maximum
}

function scope(value, maximum) {
  if (!text(value, maximum)) return false
  const match = /^[A-Za-z0-9_.\/-]+$/u.exec(value)
  return Boolean(match && match[0] === value && !value.split("/").some((part) => ["", ".", ".."].includes(part)))
}

function callable(receiver, name) {
  let method
  try { method = receiver?.[name] } catch { throw failure("OWNED_LAYOUT_ARM_API_UNAVAILABLE") }
  if (typeof method !== "function") throw failure("OWNED_LAYOUT_ARM_API_UNAVAILABLE")
  return method
}

/** An inert private arm. Its cohort/configuration stay pinned; the coordinator
 * owns call deadlines, pending-operation quarantine and timed-runner outcomes. */
export function createOwnedLayoutArm(binding, input) {
  const source = fixedRecord(input, ["cohort", "metadata", "blocks"])
  const cohort = fixedRecord(source.cohort, ["id", "layout", "metadataKey", "blockPrefix", "owner"])
  const metadata = fixedRecord(source.metadata, ["kind", "uri", "key", "durable"])
  const blocks = fixedRecord(source.blocks, ["kind", "key", "endpoint", "bucket", "region", "accessKeyId", "secretAccessKey", "durable"])
  if (!scope(cohort.id, 64) || !scope(cohort.owner, 128) || !scope(cohort.metadataKey, 255) || !scope(cohort.blockPrefix, 512) ||
      !["legacy", "compact"].includes(cohort.layout) || metadata.kind !== "tidb" || blocks.kind !== "rustfs" ||
      metadata.key !== cohort.metadataKey || blocks.key !== cohort.blockPrefix || typeof metadata.durable !== "boolean" || typeof blocks.durable !== "boolean" ||
      !text(metadata.uri, 4096) || ![blocks.endpoint, blocks.bucket, blocks.region, blocks.accessKeyId, blocks.secretAccessKey].every((value) => text(value, 4096))) {
    throw failure("OWNED_LAYOUT_ARM_CONFIG_INVALID")
  }
  const compact = cohort.layout === "compact"
  const flags = Object.freeze({ concurrentWrites: compact, inodeUpdates: compact, compactInodeUpdates: compact })
  const payload = Uint8Array.from({ length: CANARY_BYTES }, (_, index) => (index * 31 + 17) & 255)
  const seen = new WeakSet()
  const observations = { before_timed: null, drained_original: null, reopened: null, removed_drained: null, empty_reopened: null }
  const counts = { preflight_calls: 0, initial_create_calls: 0, reopen_create_calls: 0, original_shutdown_calls: 0, reopen_shutdown_calls: 0,
    canary_write_calls: 0, canary_bytes_written: 0, canary_sync_calls: 0, canary_write_close_calls: 0,
    canary_read_calls: 0, canary_bytes_read: 0, canary_eof_read_calls: 0, canary_eof_bytes_read: null,
    canary_read_close_calls: 0, canary_remove_calls: 0, initial_empty_calls: 0, final_empty_calls: 0, layout_inspection_calls: 0 }
  const canary = { payload_bytes: CANARY_BYTES, created: false, full_bytes_verified: false, eof_verified: false, remove_confirmed: false, logical_root_empty: false }
  let phase = "idle", preflight = null, original = null, factory = null, cleanupPromise = null, nativeShutdownPromise = null
  let originalShutdownConfirmed = false, reopenShutdownConfirmed = 0, failed = null, unconfirmedReopen = null

  function snapshot() {
    return Object.freeze({
      schema: "mount-rs.owned-layout-arm.v1", layout: cohort.layout, phase, complete: phase === "verified",
      construction: Object.freeze({ metadata_provider: "tidb", block_provider: "rustfs", chunk_size_bytes: CHUNK_BYTES,
        metadata_durable: metadata.durable, blocks_durable: blocks.durable, flags }),
      preflight, original_shutdown_confirmed: originalShutdownConfirmed, reopen_shutdown_confirmed_count: reopenShutdownConfirmed,
      layout_observations: Object.freeze({ ...observations }), canary: Object.freeze({ ...canary }), counts: Object.freeze({ ...counts }),
      counter_scope: "untimed_helper_invocations_and_public_api_dispatches; not_backend_or_timed_workload_proof",
      failure_code: failed?.code || null, cleanup_codes: Object.freeze([...(failed?.cleanupCodes || [])]),
    })
  }

  function recordFailure(error) {
    failed = error
    phase = "failed"
    return error
  }

  async function invoke(receiver, method, args, code, counter) {
    if (typeof method !== "function") throw failure("OWNED_LAYOUT_ARM_API_UNAVAILABLE")
    if (counter) counts[counter]++
    try { return await Reflect.apply(method, receiver, args) } catch { throw failure(code) }
  }

  function pin(lease) {
    lease.shutdown = callable(lease.filesystem, "shutdown")
    for (const name of ["open", "readFile", "unlink", "readdirBounded", "inspectCompactLayout"]) lease.methods[name] = callable(lease.filesystem, name)
  }

  function constructorOptions() {
    return Object.freeze({ metadata: Object.freeze({ ...metadata }), blocks: Object.freeze({ ...blocks }), chunkSize: CHUNK_BYTES, ...flags, owner: cohort.owner })
  }

  async function construct(initial) {
    const code = initial ? "OWNED_LAYOUT_ARM_CREATE_FAILED" : "OWNED_LAYOUT_ARM_REOPEN_FAILED"
    const filesystem = await invoke(binding, factory, [constructorOptions()], code, initial ? "initial_create_calls" : "reopen_create_calls")
    if (!filesystem || typeof filesystem !== "object") throw failure(code)
    if (seen.has(filesystem)) throw failure("OWNED_LAYOUT_ARM_REOPEN_NOT_FRESH")
    seen.add(filesystem)
    return { filesystem, shutdown: null, methods: {} }
  }

  async function layout(lease) {
    const raw = await invoke(lease.filesystem, lease.methods.inspectCompactLayout, [], "OWNED_LAYOUT_ARM_LAYOUT_PROOF_FAILED", "layout_inspection_calls")
    if (!compact) {
      if (raw !== null) throw failure("OWNED_LAYOUT_ARM_LAYOUT_PROOF_FAILED")
      return Object.freeze({ kind: "recognized_non_mrc5", exact_legacy_marker: false, fresh_preflight_required: true, constructor_flags: flags })
    }
    let receipt
    try {
      receipt = validateCompactLayoutReceipt(raw)
      if (/^[0-9a-f]{32}$/u.exec(receipt.backingId)?.[0] !== receipt.backingId ||
          /^(?:0|[1-9][0-9]{0,19})$/u.exec(receipt.structuralGeneration)?.[0] !== receipt.structuralGeneration) throw failure("OWNED_LAYOUT_ARM_LAYOUT_PROOF_FAILED")
    } catch { throw failure("OWNED_LAYOUT_ARM_LAYOUT_PROOF_FAILED") }
    return Object.freeze({ kind: "MRC5", receipt })
  }

  function continuity(next, prior, exact) {
    if (!compact) return
    const left = next.receipt, right = prior.receipt
    if (left.backingId !== right.backingId || (exact ? left.structuralGeneration !== right.structuralGeneration : BigInt(left.structuralGeneration) < BigInt(right.structuralGeneration))) {
      throw failure("OWNED_LAYOUT_ARM_GENERATION_CONTINUITY_FAILED")
    }
  }

  async function closeOriginal() {
    if (nativeShutdownPromise) return nativeShutdownPromise
    const acquired = original
    nativeShutdownPromise = (async () => {
      await invoke(acquired.filesystem, acquired.shutdown, [], "OWNED_LAYOUT_ARM_SHUTDOWN_FAILED", "original_shutdown_calls")
      originalShutdownConfirmed = true
      original = null
    })()
    return nativeShutdownPromise
  }

  async function withHandle(lease, flagsValue, operation, code, closeCounter) {
    const handle = await invoke(lease.filesystem, lease.methods.open, [CANARY_PATH, flagsValue], code)
    let first = null, result, close = null
    try {
      close = callable(handle, "close")
      result = await operation(handle)
    } catch (error) { first = ownFailure(error, code) }
    try { await invoke(handle, close, [], "OWNED_LAYOUT_ARM_HANDLE_CLOSE_FAILED", closeCounter) }
    catch (error) { first = withCleanupFailure(first, ownFailure(error, "OWNED_LAYOUT_ARM_HANDLE_CLOSE_FAILED")) }
    if (first) throw first
    return result
  }

  async function empty(lease, counter) {
    const entries = await invoke(lease.filesystem, lease.methods.readdirBounded, ["/", 1], "OWNED_LAYOUT_ARM_EMPTY_PROOF_FAILED", counter)
    try { if (!Array.isArray(entries) || entries.length !== 0) throw failure("OWNED_LAYOUT_ARM_EMPTY_PROOF_FAILED") }
    catch { throw failure("OWNED_LAYOUT_ARM_EMPTY_PROOF_FAILED") }
  }

  async function prepare() {
    if (phase !== "idle") throw failure("OWNED_LAYOUT_ARM_STATE_INVALID")
    phase = "preparing"
    try {
      factory = callable(binding, "createChunkedDriver")
      counts.preflight_calls++
      try { preflight = await observeSplitNamespacePresence(binding, metadata, blocks) }
      catch { throw failure("OWNED_LAYOUT_ARM_PREFLIGHT_FAILED") }
      original = await construct(true)
      pin(original)
      await empty(original, "initial_empty_calls")
      await withHandle(original, "wx", async (handle) => {
        const write = callable(handle, "write"), sync = callable(handle, "sync")
        const raw = await invoke(handle, write, [Uint8Array.from(payload), 0, CANARY_BYTES, 0], "OWNED_LAYOUT_ARM_CANARY_WRITE_FAILED", "canary_write_calls")
        const result = fixedRecord(raw, ["bytesWritten", "buffer"], "OWNED_LAYOUT_ARM_CANARY_WRITE_FAILED")
        try {
          if (result.bytesWritten !== CANARY_BYTES || !(result.buffer instanceof Uint8Array) || result.buffer.byteLength !== CANARY_BYTES ||
              !payload.every((value, index) => result.buffer[index] === value)) throw failure("OWNED_LAYOUT_ARM_CANARY_WRITE_FAILED")
        } catch { throw failure("OWNED_LAYOUT_ARM_CANARY_WRITE_FAILED") }
        counts.canary_bytes_written = CANARY_BYTES
        await invoke(handle, sync, [], "OWNED_LAYOUT_ARM_CANARY_WRITE_FAILED", "canary_sync_calls")
      }, "OWNED_LAYOUT_ARM_CANARY_WRITE_FAILED", "canary_write_close_calls")
      canary.created = true
      observations.before_timed = await layout(original)
      phase = "prepared"
      return snapshot()
    } catch (error) {
      let first = ownFailure(error, "OWNED_LAYOUT_ARM_CREATE_FAILED")
      if (original) {
        try { await closeOriginal() } catch (cleanup) { first = withCleanupFailure(first, ownFailure(cleanup, "OWNED_LAYOUT_ARM_SHUTDOWN_FAILED")) }
      }
      throw recordFailure(first)
    }
  }

  function cleanupOriginal() {
    if (cleanupPromise) return cleanupPromise
    if (!["prepared", "handed_off"].includes(phase)) return Promise.reject(failure("OWNED_LAYOUT_ARM_STATE_INVALID"))
    phase = "closing_original"
    cleanupPromise = (async () => {
      let first = null
      try {
        const drained = await layout(original)
        continuity(drained, observations.before_timed, false)
        observations.drained_original = drained
      } catch (error) { first = ownFailure(error, "OWNED_LAYOUT_ARM_LAYOUT_PROOF_FAILED") }
      try { await closeOriginal() } catch (error) { first = withCleanupFailure(first, ownFailure(error, "OWNED_LAYOUT_ARM_SHUTDOWN_FAILED")) }
      if (first) throw recordFailure(first)
      phase = "original_closed"
      return snapshot()
    })()
    return cleanupPromise
  }

  function takeForRunner() {
    if (phase !== "prepared") throw failure("OWNED_LAYOUT_ARM_STATE_INVALID")
    phase = "handed_off"
    return Object.freeze({ filesystem: original.filesystem, cleanup: cleanupOriginal })
  }

  async function withFresh(operation) {
    let acquired = await construct(false), first = null, result
    try { pin(acquired); result = await operation(acquired) }
    catch (error) { first = ownFailure(error, "OWNED_LAYOUT_ARM_REOPEN_FAILED") }
    try {
      await invoke(acquired.filesystem, acquired.shutdown, [], "OWNED_LAYOUT_ARM_SHUTDOWN_FAILED", "reopen_shutdown_calls")
      reopenShutdownConfirmed++
      acquired = null
    } catch (error) {
      unconfirmedReopen = acquired
      first = withCleanupFailure(first, ownFailure(error, "OWNED_LAYOUT_ARM_SHUTDOWN_FAILED"))
    }
    if (first) throw first
    return result
  }

  async function verifyPersistence() {
    if (phase !== "original_closed" || !originalShutdownConfirmed || failed || unconfirmedReopen) throw failure("OWNED_LAYOUT_ARM_STATE_INVALID")
    phase = "verifying"
    try {
      await withFresh(async (lease) => {
        const reopened = await layout(lease)
        continuity(reopened, observations.drained_original, true)
        observations.reopened = reopened
        const bytes = await invoke(lease.filesystem, lease.methods.readFile, [CANARY_PATH], "OWNED_LAYOUT_ARM_CANARY_READ_FAILED", "canary_read_calls")
        try {
          if (!(bytes instanceof Uint8Array) || bytes.byteLength !== CANARY_BYTES || !payload.every((value, index) => bytes[index] === value)) throw failure("OWNED_LAYOUT_ARM_CANARY_READ_FAILED")
        } catch { throw failure("OWNED_LAYOUT_ARM_CANARY_READ_FAILED") }
        counts.canary_bytes_read = CANARY_BYTES
        canary.full_bytes_verified = true
        await withHandle(lease, "r", async (handle) => {
          const buffer = Uint8Array.of(165), read = callable(handle, "read")
          const raw = await invoke(handle, read, [buffer, 0, 1, CANARY_BYTES], "OWNED_LAYOUT_ARM_CANARY_EOF_FAILED", "canary_eof_read_calls")
          const result = fixedRecord(raw, ["bytesRead", "buffer"], "OWNED_LAYOUT_ARM_CANARY_EOF_FAILED")
          try {
            if (result.bytesRead !== 0 || !(result.buffer instanceof Uint8Array) || result.buffer.byteLength !== 1 || result.buffer[0] !== 165 || buffer[0] !== 165) throw failure("OWNED_LAYOUT_ARM_CANARY_EOF_FAILED")
          } catch { throw failure("OWNED_LAYOUT_ARM_CANARY_EOF_FAILED") }
          counts.canary_eof_bytes_read = 0
          canary.eof_verified = true
        }, "OWNED_LAYOUT_ARM_CANARY_EOF_FAILED", "canary_read_close_calls")
        await invoke(lease.filesystem, lease.methods.unlink, [CANARY_PATH], "OWNED_LAYOUT_ARM_CANARY_REMOVE_FAILED", "canary_remove_calls")
        canary.remove_confirmed = true
        const removed = await layout(lease)
        continuity(removed, reopened, false)
        observations.removed_drained = removed
      })
      await withFresh(async (lease) => {
        const reopened = await layout(lease)
        continuity(reopened, observations.removed_drained, true)
        observations.empty_reopened = reopened
        await empty(lease, "final_empty_calls")
        canary.logical_root_empty = true
      })
      phase = "verified"
      return snapshot()
    } catch (error) { throw recordFailure(ownFailure(error, "OWNED_LAYOUT_ARM_REOPEN_FAILED")) }
  }

  const capability = { prepare, takeForRunner, cleanupOriginal, verifyPersistence, snapshot }
  Object.defineProperty(capability, "cohort", { value: cohort, enumerable: false })
  return Object.freeze(capability)
}
