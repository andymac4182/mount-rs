import { Buffer } from "node:buffer"

const MAX_RECEIPT_BYTES = 4096
const MAX_U128 = 340282366920938463463374607431768211455n
const PROBE_DEADLINE_NS = 30000000000n
const SHUTDOWN_DEADLINE_NS = 15000000000n
const messages = Object.freeze({
  NAMESPACE_PRESENCE_API_UNAVAILABLE: "namespace presence inspection is unavailable",
  NAMESPACE_PRESENCE_QUERY_FAILED: "namespace presence query failed",
  NAMESPACE_PRESENCE_INVALID_RECEIPT: "namespace presence receipt is invalid",
  NAMESPACE_PRESENCE_RECEIPT_TOO_LARGE: "namespace presence receipt exceeds its byte limit",
  NAMESPACE_PRESENCE_DEADLINE_EXCEEDED: "namespace presence observation exceeds its native deadline",
  NAMESPACE_PRESENCE_NOT_ABSENT: "namespace presence observations do not establish absence",
})

function failure(code) {
  return Object.assign(new Error(messages[code]), { code })
}

function invalid() {
  throw failure("NAMESPACE_PRESENCE_INVALID_RECEIPT")
}

function closedObject(value, fields) {
  if (!value || typeof value !== "object" || Array.isArray(value) ||
      Object.keys(value).length !== fields.length || !fields.every((field) => Object.hasOwn(value, field))) invalid()
  return value
}

function decimal(value) {
  if (typeof value !== "string") invalid()
  const match = /^(?:0|[1-9][0-9]{0,38})$/u.exec(value)
  if (!match || match[0] !== value) invalid()
  const integer = BigInt(value)
  if (integer > MAX_U128) invalid()
  return integer
}

// JSON.parse validates syntax first. This bounded token walk only rejects
// duplicate members that parsing would otherwise silently overwrite.
function rejectDuplicateMembers(source) {
  const tokens = source.match(/"(?:[^"\\]|\\[\s\S])*"|[{}\[\]:,]|-?(?:0|[1-9][0-9]*)(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?|true|false|null/gu) || []
  const containers = []
  for (let index = 0; index < tokens.length; index++) {
    const token = tokens[index]
    if (token === "{") containers.push(new Set())
    else if (token === "[") containers.push(null)
    else if (token === "}" || token === "]") containers.pop()
    else if (token.startsWith('"') && tokens[index + 1] === ":") {
      const members = containers.at(-1)
      const name = JSON.parse(token)
      if (!(members instanceof Set) || members.has(name)) invalid()
      members.add(name)
    }
  }
}

/** Accept only the fixed native absence receipt; it describes observations,
 * not a reservation, atomic exclusion, or a response-byte guarantee. */
export function validateSplitNamespacePresenceReceipt(source) {
  if (typeof source !== "string") invalid()
  if (Buffer.byteLength(source, "utf8") > MAX_RECEIPT_BYTES) {
    throw failure("NAMESPACE_PRESENCE_RECEIPT_TOO_LARGE")
  }
  let receipt
  try {
    receipt = JSON.parse(source)
    rejectDuplicateMembers(source)
  } catch { invalid() }
  closedObject(receipt, ["schema", "namespace_absent", "metadata", "blobs", "pool_shutdown", "clock", "consistency", "limits"])
  const metadata = closedObject(receipt.metadata, ["provider", "key_scope", "schema_setup", "row_presence", "observed_at_ns"])
  const rows = closedObject(metadata.row_presence, ["metadata", "inodes", "compact_guards", "compact_members", "compact_dentries", "block_authority", "blocks"])
  const blobs = closedObject(receipt.blobs, ["provider", "scope", "observation", "requested_max_keys", "prefix_absent", "observed_at_ns"])
  const shutdown = closedObject(receipt.pool_shutdown, ["confirmed", "elapsed_ns"])
  const limits = closedObject(receipt.limits, ["probe_deadline_ms", "pool_shutdown_deadline_ms", "list_request_keys", "response_byte_cap", "server_truncation_flag", "deadline_semantics"])
  if (receipt.schema !== "mount-rs.split-namespace-presence.v1" ||
      receipt.clock !== "elapsed_monotonic_since_native_preflight_start" ||
      receipt.consistency !== "separate_observations_no_reservation" ||
      metadata.provider !== "tidb" || metadata.key_scope !== "exact_input_utf8_bytes" ||
      metadata.schema_setup !== "shared_ddl_and_session_configuration" ||
      blobs.provider !== "rustfs" || blobs.scope !== "canonical_ascii_prefix_descendants" ||
      blobs.observation !== "signed_list_page_api" || blobs.requested_max_keys !== 1 ||
      typeof receipt.namespace_absent !== "boolean" || typeof blobs.prefix_absent !== "boolean" ||
      !Object.values(rows).every((value) => typeof value === "boolean") || shutdown.confirmed !== true ||
      limits.probe_deadline_ms !== 30000 || limits.pool_shutdown_deadline_ms !== 15000 ||
      limits.list_request_keys !== 1 || limits.response_byte_cap !== "unavailable" ||
      limits.server_truncation_flag !== "unavailable" ||
      limits.deadline_semantics !== "cooperative_await_with_elapsed_recheck") invalid()
  const metadataNs = decimal(metadata.observed_at_ns)
  const blobsNs = decimal(blobs.observed_at_ns)
  const shutdownNs = decimal(shutdown.elapsed_ns)
  if (metadataNs > PROBE_DEADLINE_NS || blobsNs > PROBE_DEADLINE_NS || shutdownNs > SHUTDOWN_DEADLINE_NS) {
    throw failure("NAMESPACE_PRESENCE_DEADLINE_EXCEEDED")
  }
  if (metadataNs > blobsNs) invalid()
  if (!receipt.namespace_absent || !blobs.prefix_absent || Object.values(rows).some((value) => value)) {
    throw failure("NAMESPACE_PRESENCE_NOT_ABSENT")
  }
  // Build each accepted object explicitly. No native input object or extra
  // projection is retained, and every nested object is independently frozen.
  return Object.freeze({
    schema: "mount-rs.split-namespace-presence.v1",
    namespace_absent: true,
    metadata: Object.freeze({
      provider: "tidb",
      key_scope: "exact_input_utf8_bytes",
      schema_setup: "shared_ddl_and_session_configuration",
      row_presence: Object.freeze({ metadata: false, inodes: false, compact_guards: false, compact_members: false, compact_dentries: false, block_authority: false, blocks: false }),
      observed_at_ns: metadata.observed_at_ns,
    }),
    blobs: Object.freeze({
      provider: "rustfs",
      scope: "canonical_ascii_prefix_descendants",
      observation: "signed_list_page_api",
      requested_max_keys: 1,
      prefix_absent: true,
      observed_at_ns: blobs.observed_at_ns,
    }),
    pool_shutdown: Object.freeze({ confirmed: true, elapsed_ns: shutdown.elapsed_ns }),
    clock: "elapsed_monotonic_since_native_preflight_start",
    consistency: "separate_observations_no_reservation",
    limits: Object.freeze({
      probe_deadline_ms: 30000,
      pool_shutdown_deadline_ms: 15000,
      list_request_keys: 1,
      response_byte_cap: "unavailable",
      server_truncation_flag: "unavailable",
      deadline_semantics: "cooperative_await_with_elapsed_recheck",
    }),
  })
}

/** One native call per observation. The coordinator owns timeout and pending
 * promise handling; this helper creates no filesystem and performs no retry. */
export async function observeSplitNamespacePresence(binding, metadata, blocks) {
  let query
  try { query = binding?.inspectSplitNamespacePresence } catch {
    throw failure("NAMESPACE_PRESENCE_QUERY_FAILED")
  }
  if (typeof query !== "function") throw failure("NAMESPACE_PRESENCE_API_UNAVAILABLE")
  let receipt
  try { receipt = await Reflect.apply(query, binding, [metadata, blocks]) } catch {
    throw failure("NAMESPACE_PRESENCE_QUERY_FAILED")
  }
  return validateSplitNamespacePresenceReceipt(receipt)
}
