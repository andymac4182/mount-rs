import { Buffer } from "node:buffer"
import { createHash } from "node:crypto"
import { isDeepStrictEqual, types } from "node:util"

// Fixed reviewed seams, not a full native build dependency closure. This codec
// only joins provided boundary observations; it never builds or loads an addon.
const SOURCE_PATHS = [
  "Cargo.toml", "Cargo.lock", "bindings/mount-rs-napi/Cargo.toml", "bindings/mount-rs-napi/index.js",
  "bindings/mount-rs-napi/src/lib.rs", "bindings/mount-rs-napi/src/namespace_presence.rs", "src/diagnostics/storage.rs", "src/diagnostics/profile.rs",
  "scripts/test-tidb.sh", "scripts/test-rustfs.sh", "scripts/rustfs-combo-runner.py", "scripts/rustfs-bounded-docker.py",
  ".github/workflows/remote-drives.yml", "benchmarks/storage/errors.mjs", "benchmarks/storage/stats.mjs", "benchmarks/storage/runner.mjs",
  "benchmarks/storage/providers.mjs", "benchmarks/storage/diagnostics.mjs", "benchmarks/storage/backing-engine-transport.mjs",
  "benchmarks/storage/backing-observer.mjs", "benchmarks/storage/compact-layout.mjs", "benchmarks/storage/namespace-presence.mjs",
  "benchmarks/storage/owned-backing-pilot.mjs", "benchmarks/storage/owned-layout-arm.mjs", "benchmarks/storage/owned-layout-outcome.mjs",
  "benchmarks/storage/owned-layout-metrics.mjs", "benchmarks/storage/owned-layout-comparison.mjs", "benchmarks/storage/owned-layout-entry.mjs",
]
const PRODUCER_PATHS = ["scripts/owned-layout-handoff.mjs", "scripts/owned_layout_process.py", "scripts/test-tidb.sh", "scripts/test-rustfs.sh",
  "scripts/rustfs-bounded-docker.py", "scripts/rustfs-combo-runner.py", ".github/workflows/remote-drives.yml"]
const ROLES = ["pd-1", "pd-2", "pd-3", "tikv-1", "tikv-2", "tikv-3", "tidb", "rustfs-service"]
const ACTION_IDS = new Set(["outer.comparison", "engine.capacity", "engine.info", "image.inspect", "image.pull", "container.port", "container.logs",
  "container.create", "container.inspect", "container.remove", "container.absence", "container.exec", "volume.create", "volume.inspect",
  "volume.remove", "volume.absence", "network.create", "network.inspect", "network.remove", "network.absence", "bind.cleanup"])
const PROCESS_KEYS = ["schema", "action_id", "pid", "pgid", "raw_returncode", "normalized_exit", "supervisor_signal", "child_return_signal",
  "wait_unavailable_observed", "deadline_exceeded", "group_leak_observed", "group_probe_unavailable_observed", "retirement_signal_unavailable_observed",
  "forced_process_retirement", "retirement_signals", "child_reaped", "group_state", "sticky_failure", "failure_code"]
const FLAGS = ["wait_unavailable_observed", "deadline_exceeded", "group_leak_observed", "group_probe_unavailable_observed",
  "retirement_signal_unavailable_observed", "forced_process_retirement", "child_reaped", "sticky_failure"]
const INPUT_CAPS = { projection: 33554432, originals: 33554432, fixtures: 16384, controller: 16384,
  engine: 16384, build: 65536, native: 134217728, capacity: 16384 }
// The largest valid input is the terminal envelope: all artifact caps, seven
// producer sources/receipt,1024 output+process actions,1024 group receipts and
// one outer receipt. Bound copying before field-specific rejection as well.
const SNAPSHOT_BYTE_CAP = Object.values(INPUT_CAPS).reduce((total, cap) => total + cap, 0) + 7 * 2097152 + 65536 + 1024 * (16384 + 4096) + 1024 * 4096 + 4096
const failures = new WeakMap(), sha = (value) => createHash("sha256").update(value).digest("hex")
const code = (suffix) => `OWNED_LAYOUT_HANDOFF_${suffix}`
function fail(suffix) { const token = Object.create(null); failures.set(token, suffix); throw token }
function need(condition, suffix) { if (!condition) fail(suffix) }
const text = (value) => typeof value === "string" && value.length > 0 && !value.includes("\0") && Buffer.byteLength(value) <= 4096
const owner = (value) => text(value) && /^[A-Za-z0-9_][A-Za-z0-9_.-]{0,119}$/u.exec(value)?.[0] === value && value !== "." && value !== ".."
const hex = (value, size = 64) => typeof value === "string" && new RegExp(`^[a-f0-9]{${size}}$`, "u").exec(value)?.[0] === value
const integer = (value, maximum = Number.MAX_SAFE_INTEGER) => Number.isSafeInteger(value) && value >= 0 && value <= maximum
const bool = (value) => typeof value === "boolean"
const typedPrototype = Object.getPrototypeOf(Uint8Array.prototype)
const typedLength = Object.getOwnPropertyDescriptor(typedPrototype, "byteLength").get
const typedBuffer = Object.getOwnPropertyDescriptor(typedPrototype, "buffer").get
const typedSet = Uint8Array.prototype.set

function buffer(value, maximum, empty = false) {
  need(!types.isProxy(value) && Buffer.isBuffer(value) && Object.getPrototypeOf(value) === Buffer.prototype, "INPUT_INVALID")
  for (const key of ["length", "byteLength", "buffer", "byteOffset", "constructor", "toJSON", Symbol.iterator]) need(!Object.hasOwn(value, key), "INPUT_INVALID")
  need(!types.isSharedArrayBuffer(typedBuffer.call(value)), "INPUT_INVALID")
  const length = typedLength.call(value)
  need((empty || length > 0) && length <= maximum, "INPUT_CAP")
  const copy = Buffer.alloc(length); typedSet.call(copy, value); return copy
}

// Copy only data descriptors. The Buffer brand/copy path avoids enumerating a
//128MiB addon and never calls caller methods, iterators or toJSON hooks.
function snapshot(value, state = { count: 0, bytes: 0, seen: new Set() }, depth = 0) {
  need(++state.count <= 100000 && depth <= 48, "INPUT_CAP")
  if (value === null || typeof value === "boolean") return value
  if (typeof value === "string") { need(Buffer.byteLength(value) <= 33554432, "INPUT_CAP"); return value }
  if (typeof value === "number") { need(Number.isFinite(value), "INPUT_INVALID"); return value }
  need(typeof value === "object" && !types.isProxy(value), "INPUT_INVALID")
  if (Buffer.isBuffer(value)) {
    const copy = buffer(value, Math.min(134217728, SNAPSHOT_BYTE_CAP - state.bytes), true)
    state.bytes += copy.length; return copy
  }
  need(!state.seen.has(value), "INPUT_INVALID"); state.seen.add(value)
  const array = Array.isArray(value), prototype = Object.getPrototypeOf(value)
  need(array ? prototype === Array.prototype : prototype === Object.prototype || prototype === null, "INPUT_INVALID")
  const descriptors = Object.getOwnPropertyDescriptors(value), keys = Reflect.ownKeys(descriptors)
  need(keys.every((key) => typeof key === "string" && Object.hasOwn(descriptors[key], "value")), "INPUT_INVALID")
  let result
  if (array) {
    const length = descriptors.length?.value
    need(integer(length, 1024), "INPUT_CAP")
    need(keys.length === length + 1 && Array.from({ length }, (_, index) => String(index)).every((key) => Object.hasOwn(descriptors, key)), "INPUT_INVALID")
    result = Array.from({ length }, (_, index) => snapshot(descriptors[String(index)].value, state, depth + 1))
  } else {
    result = Object.create(null)
    for (const key of keys) result[key] = snapshot(descriptors[key].value, state, depth + 1)
  }
  state.seen.delete(value); return result
}
function record(value, keys, suffix) {
  need(value !== null && typeof value === "object" && !Array.isArray(value) && !Buffer.isBuffer(value), suffix)
  need(Object.keys(value).length === keys.length && keys.every((key) => Object.hasOwn(value, key)), suffix)
  return value
}
function array(value, length, suffix) { need(Array.isArray(value) && value.length === length, suffix); return value }
function freeze(value) {
  if (value !== null && typeof value === "object") { for (const child of Object.values(value)) freeze(child); Object.freeze(value) }
  return value
}

// A bounded fatal UTF8 parser rejects decoded duplicate keys before a later
// object assignment could erase them. Values are fresh, dense data records.
function parse(source) {
  try {
    const input = new TextDecoder("utf-8", { fatal: true }).decode(source)
    let at = 0, nodes = 0
    const whitespace = () => { while (/[\t\r\n ]/u.test(input[at] ?? "x")) at++ }
    function string() {
      need(input[at] === '"', "JSON_INVALID"); const start = at++
      while (at < input.length) {
        const char = input[at++]
        if (char === '"') return JSON.parse(input.slice(start, at))
        if (char === "\\") at++
      }
      fail("JSON_INVALID")
    }
    function value(depth) {
      need(++nodes <= 100000 && depth <= 48, "JSON_INVALID"); whitespace()
      const char = input[at]
      if (char === '"') return string()
      if (char === "{" || char === "[") {
        at++; whitespace(); const object = char === "{", result = object ? Object.create(null) : [], seen = new Set(), end = object ? "}" : "]"
        if (input[at] === end) { at++; return result }
        while (true) {
          if (object) {
            whitespace(); const key = string(); need(!seen.has(key), "JSON_INVALID"); seen.add(key)
            whitespace(); need(input[at++] === ":", "JSON_INVALID"); result[key] = value(depth + 1)
          } else result.push(value(depth + 1))
          whitespace(); const next = input[at++]; if (next === end) break
          need(next === ",", "JSON_INVALID")
        }
        return result
      }
      for (const [literal, result] of [["true", true], ["false", false], ["null", null]]) if (input.startsWith(literal, at)) { at += literal.length; return result }
      const match = /^-?(?:0|[1-9][0-9]*)(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?/u.exec(input.slice(at))
      need(match !== null, "JSON_INVALID"); at += match[0].length
      const result = Number(match[0]); need(Number.isFinite(result), "JSON_INVALID"); return result
    }
    const result = value(0); whitespace(); need(at === input.length, "JSON_INVALID"); return result
  } catch (error) { if (failures.has(error)) throw error; fail("JSON_INVALID") }
}
function json(value, maximum) { return parse(buffer(value, maximum)) }
function result(kind, receipt = null, { status = "ready", count = 0, uncertainty = false, suffix = null, maximum = 16384 } = {}) {
  const encoded = receipt === null ? null : `${JSON.stringify(receipt)}\n`
  // Required observations are never truncated to fit an envelope.
  if (encoded !== null && Buffer.byteLength(encoded) > maximum) return result(kind, null, { status: "incomplete", uncertainty, suffix: "INPUT_CAP" })
  return freeze({ schema: "mount-rs.owned-layout-producer-result.v1", kind, status, private_json: encoded,
    public: { schema: "mount-rs.owned-layout-producer-public.v1", kind, status, hosted_qualified: false,
      receipt_schema: receipt?.schema ?? null, sha256: encoded === null ? null : sha(Buffer.from(encoded)), bytes: encoded === null ? null : Buffer.byteLength(encoded),
      count, complete: status === "ready", native_uncertainty: uncertainty, failure_code: suffix === null ? null : code(suffix),
      evidence_scope: "provided_boundary_observations; not_live_qualification" } })
}
function boundary(kind, input, operation) {
  try { return operation(snapshot(input)) }
  catch (error) { return result(kind, null, { status: "rejected", uncertainty: null, suffix: failures.get(error) ?? "INPUT_INVALID" }) }
}
function engine(value) { record(value, ["socket_path"], "ENGINE_INVALID"); need(value.socket_path === "/var/run/docker.sock", "ENGINE_INVALID"); return value }
const labelsFor = (kind, value) => kind === "tidb" ? { "mount-rs.tidb.run": value } : { "com.mount-rs.rustfs-test": "mount-rs-rustfs-test", "com.mount-rs.rustfs-test-run": value }
function same(value, expected, suffix) { need(isDeepStrictEqual(snapshot(value), snapshot(expected)), suffix) }
function fixtures(value) {
  record(value, ["schema", "tidb_owner", "rustfs_owner", "generation", "entries"], "CONTROLLER_INVALID")
  need(value.schema === "mount-rs.owned-backing-cids.v1" && value.generation === "1" && owner(value.tidb_owner) && owner(value.rustfs_owner), "CONTROLLER_INVALID")
  array(value.entries, 8, "CONTROLLER_INVALID"); const ids = new Set()
  value.entries.forEach((entry, index) => {
    record(entry, ["role", "cid", "labels"], "CONTROLLER_INVALID")
    need(entry.role === ROLES[index] && hex(entry.cid) && !ids.has(entry.cid), "CONTROLLER_INVALID"); ids.add(entry.cid)
    same(entry.labels, labelsFor(index === 7 ? "rustfs" : "tidb", index === 7 ? value.rustfs_owner : value.tidb_owner), "CONTROLLER_INVALID")
  }); return value
}
function endpoint(value, kind) {
  need(text(value), "CONTROLLER_INVALID")
  try {
    const parsed = new URL(value)
    need(parsed.protocol === (kind === "tidb" ? "mysql:" : "http:") && parsed.hostname === "127.0.0.1" && /^[1-9][0-9]{0,4}$/u.test(parsed.port) && Number(parsed.port) <= 65535 && !parsed.search && !parsed.hash, "CONTROLLER_INVALID")
    if (kind === "rustfs") need(!parsed.username && !parsed.password && parsed.pathname === "/", "CONTROLLER_INVALID")
  } catch (error) { if (failures.has(error)) throw error; fail("CONTROLLER_INVALID") }
}
function controller(value, fixture, fixtureHash, engineHash) {
  record(value, ["schema", "generation", "tidb_owner", "rustfs_owner", "fixture_sha256", "engine_capability_sha256", "tidb_url", "rustfs_endpoint", "rustfs_bucket", "scope"], "CONTROLLER_INVALID")
  need(value.schema === "mount-rs.owned-layout-controller.v1" && value.generation === "1" && owner(value.tidb_owner) && owner(value.rustfs_owner), "CONTROLLER_INVALID")
  endpoint(value.tidb_url, "tidb"); endpoint(value.rustfs_endpoint, "rustfs"); need(text(value.rustfs_bucket), "CONTROLLER_INVALID")
  need(hex(value.fixture_sha256) && hex(value.engine_capability_sha256) && (fixtureHash === null || value.fixture_sha256 === fixtureHash) && (engineHash === null || value.engine_capability_sha256 === engineHash), "CONTROLLER_INVALID")
  if (fixture !== null) need(value.tidb_owner === fixture.tidb_owner && value.rustfs_owner === fixture.rustfs_owner, "CONTROLLER_INVALID")
  record(value.scope, ["id", "owner", "metadataPrefix", "blockPrefix"], "SCOPE_INVALID")
  need(/^layout-[a-f0-9]{32}$/u.exec(value.scope.id)?.[0] === value.scope.id, "SCOPE_INVALID")
  same(value.scope, { id: value.scope.id, owner: value.tidb_owner, metadataPrefix: `mount-rs-owned-layout/${value.tidb_owner}/${value.scope.id}`, blockPrefix: `mount-rs-owned-layout/${value.rustfs_owner}/${value.scope.id}` }, "SCOPE_INVALID")
  return value
}
export function buildOwnedLayoutHandoff(input) {
  return boundary("controller", input, (source) => {
    record(source, ["fixtures", "engine", "created", "inherited", "scope_nonce"], "INPUT_INVALID")
    const fixtureBytes = buffer(source.fixtures, 16384), engineBytes = buffer(source.engine, 16384), fixture = fixtures(parse(fixtureBytes)); engine(parse(engineBytes))
    const created = record(source.created, ["tidb_owner", "rustfs_owner", "tidb_url", "rustfs_endpoint", "rustfs_bucket"], "CONTROLLER_INVALID")
    need(owner(created.tidb_owner) && owner(created.rustfs_owner) && created.tidb_owner === fixture.tidb_owner && created.rustfs_owner === fixture.rustfs_owner, "CONTROLLER_INVALID")
    endpoint(created.tidb_url, "tidb"); endpoint(created.rustfs_endpoint, "rustfs"); need(text(created.rustfs_bucket), "CONTROLLER_INVALID")
    const inherited = source.inherited
    need(inherited !== null && typeof inherited === "object" && !Array.isArray(inherited) && !Buffer.isBuffer(inherited), "INPUT_INVALID")
    const mapping = { MOUNT_RS_TIDB_URL: "tidb_url", TIDB_URL: "tidb_url", MOUNT_RS_RUSTFS_ENDPOINT: "rustfs_endpoint", MOUNT_RS_RUSTFS_BUCKET: "rustfs_bucket",
      MOUNT_RS_BACKING_EXPECT_TIDB_URL: "tidb_url", MOUNT_RS_BACKING_EXPECT_R2_ENDPOINT: "rustfs_endpoint", MOUNT_RS_BACKING_EXPECT_R2_BUCKET: "rustfs_bucket" }
    for (const key of Object.keys(inherited)) {
      need(Object.hasOwn(mapping, key) && typeof inherited[key] === "string" && !inherited[key].includes("\0") && Buffer.byteLength(inherited[key]) <= 4096, "INPUT_INVALID")
      need(inherited[key] === "" || inherited[key] === created[mapping[key]], "ENDPOINT_MISMATCH")
    }
    const nonce = buffer(source.scope_nonce, 16); need(nonce.length === 16, "SCOPE_INVALID")
    const id = `layout-${nonce.toString("hex")}`
    return result("controller", { schema: "mount-rs.owned-layout-controller.v1", generation: "1", tidb_owner: created.tidb_owner, rustfs_owner: created.rustfs_owner,
      fixture_sha256: sha(fixtureBytes), engine_capability_sha256: sha(engineBytes), tidb_url: created.tidb_url, rustfs_endpoint: created.rustfs_endpoint, rustfs_bucket: created.rustfs_bucket,
      scope: { id, owner: created.tidb_owner, metadataPrefix: `mount-rs-owned-layout/${created.tidb_owner}/${id}`, blockPrefix: `mount-rs-owned-layout/${created.rustfs_owner}/${id}` } }, { count: 8 })
  })
}
function sourceHashes(source, paths) {
  record(source, paths, "SOURCE_INVALID"); return Object.fromEntries(paths.map((path) => [path, sha(buffer(source[path], 2097152))]))
}
function buildReceipt(value, nativeHash = null) {
  record(value, ["schema", "checkout_sha", "source_clean", "locked", "release", "no_js", "build_exit_code", "native_sha256", "source_sha256"], "BUILD_INVALID")
  need(value.schema === "mount-rs.owned-layout-native-build.v1" && hex(value.checkout_sha, 40) && value.source_clean === true && value.locked === true && value.release === true && value.no_js === true && value.build_exit_code === 0 && hex(value.native_sha256) && (nativeHash === null || value.native_sha256 === nativeHash), "BUILD_INVALID")
  record(value.source_sha256, SOURCE_PATHS, "SOURCE_INVALID"); need(SOURCE_PATHS.every((path) => hex(value.source_sha256[path])), "SOURCE_INVALID"); return value
}
export function buildOwnedLayoutNativeSeal(input) {
  return boundary("native_build", input, (source) => {
    record(source, ["checkout", "build", "native", "sources"], "INPUT_INVALID")
    const checkout = record(source.checkout, ["revision", "clean_before", "clean_after"], "BUILD_INVALID"), build = record(source.build, ["locked", "release", "no_js", "exit_code"], "BUILD_INVALID")
    need(hex(checkout.revision, 40) && checkout.clean_before === true && checkout.clean_after === true && build.locked === true && build.release === true && build.no_js === true && build.exit_code === 0, "BUILD_INVALID")
    return result("native_build", { schema: "mount-rs.owned-layout-native-build.v1", checkout_sha: checkout.revision, source_clean: true, locked: true, release: true,
      no_js: true, build_exit_code: 0, native_sha256: sha(buffer(source.native, 134217728)), source_sha256: sourceHashes(source.sources, SOURCE_PATHS) }, { count: 28, maximum: 65536 })
  })
}

function processReceipt(bytes, expected = null, suffix = "TERMINAL_INVALID") {
  const value = record(json(bytes, 4096), PROCESS_KEYS, suffix)
  need(value.schema === "mount-rs.owned-layout-process.v1" && ACTION_IDS.has(value.action_id) && (expected === null || value.action_id === expected) && integer(value.pid, 2147483647) && value.pid > 0 && value.pgid === value.pid, suffix)
  need((value.raw_returncode === null || Number.isInteger(value.raw_returncode) && value.raw_returncode >= -64 && value.raw_returncode <= 255) && integer(value.normalized_exit, 255) && [null, 2, 15].includes(value.supervisor_signal), suffix)
  need(value.child_return_signal === (value.raw_returncode !== null && value.raw_returncode < 0 ? -value.raw_returncode : null) && FLAGS.every((field) => bool(value[field])), suffix)
  need(Array.isArray(value.retirement_signals) && ["[]", "[15]", "[15,9]"].includes(JSON.stringify(value.retirement_signals)) && ["absent", "present", "unavailable"].includes(value.group_state), suffix)
  need(value.forced_process_retirement === (value.retirement_signals.length > 0), suffix)
  need(!value.retirement_signal_unavailable_observed || value.retirement_signals.length > 0, suffix)
  let failure = null, normalized
  if (!value.child_reaped) failure = "UNREAPED"
  else if (value.wait_unavailable_observed || value.raw_returncode === null) failure = "WAIT_UNAVAILABLE"
  else if (value.group_probe_unavailable_observed || value.group_state === "unavailable") failure = "GROUP_UNAVAILABLE"
  else if (value.retirement_signal_unavailable_observed) failure = "SIGNAL_UNAVAILABLE"
  else if (value.group_leak_observed || value.group_state !== "absent") failure = "GROUP_LEAK"
  else if (value.deadline_exceeded) failure = "TIMEOUT"
  else if (value.supervisor_signal !== null) failure = "SUPERVISOR_SIGNAL"
  else if (value.raw_returncode !== 0) failure = "CHILD_FAILED"
  else if (value.forced_process_retirement) failure = "GROUP_LEAK"
  if (["UNREAPED", "WAIT_UNAVAILABLE", "GROUP_UNAVAILABLE", "SIGNAL_UNAVAILABLE", "GROUP_LEAK"].includes(failure)) normalized = 125
  else if (failure === "TIMEOUT") normalized = 124
  else if (failure === "SUPERVISOR_SIGNAL") normalized = 128 + value.supervisor_signal
  else normalized = value.raw_returncode < 0 ? 128 - value.raw_returncode : value.raw_returncode
  need(value.normalized_exit === normalized && value.failure_code === (failure === null ? null : `OWNED_LAYOUT_PROCESS_${failure}`) && value.sticky_failure === (failure !== null), suffix)
  return value
}
const quiescent = (value) => value.child_reaped && value.group_state === "absent" && !value.wait_unavailable_observed && !value.deadline_exceeded && !value.group_leak_observed && !value.group_probe_unavailable_observed && !value.retirement_signal_unavailable_observed && !value.forced_process_retirement && value.supervisor_signal === null && value.child_return_signal === null
const clean = (value) => quiescent(value) && value.raw_returncode === 0 && value.normalized_exit === 0 && !value.sticky_failure && value.failure_code === null
function capacityReceipt(engineBytes, observation, actionBytes) {
  engine(parse(engineBytes)); const action = processReceipt(actionBytes, "engine.capacity", "CAPACITY_INVALID")
  let row
  try { row = new TextDecoder("utf-8", { fatal: true }).decode(observation) } catch { fail("CAPACITY_INVALID") }
  const match = /^(0|[1-9][0-9]{0,19}) (0|[1-9][0-9]{0,9})\n?$/u.exec(row)
  need(match?.[0] === row && BigInt(match[1]) <= 18446744073709551615n && Number(match[2]) <= 2147483647, "CAPACITY_INVALID")
  const enough = BigInt(match[1]) >= 10737418240n && Number(match[2]) >= 4, settled = clean(action)
  return { receipt: { schema: "mount-rs.owned-layout-engine-capacity.v1", engine_capability_sha256: sha(engineBytes), memory_bytes: match[1], cpu_count: Number(match[2]),
    min_memory_bytes: "10737418240", min_cpu_count: 4, qualified: settled && enough, observation_sha256: sha(observation), observation_base64: observation.toString("base64"),
    action, action_sha256: sha(actionBytes), action_base64: actionBytes.toString("base64") }, enough, settled }
}
export function assessOwnedLayoutEngineCapacity(input) {
  return boundary("engine_capacity", input, (source) => {
    record(source, ["engine", "observation", "action"], "INPUT_INVALID")
    const observed = capacityReceipt(buffer(source.engine, 16384), buffer(source.observation, 16384), buffer(source.action, 4096))
    return result("engine_capacity", observed.receipt, { count: 1, status: observed.receipt.qualified ? "ready" : "incomplete", uncertainty: !observed.settled,
      suffix: observed.receipt.qualified ? null : observed.settled ? "CAPACITY_FLOOR_NOT_MET" : "CAPACITY_INVALID" })
  })
}

function nullableOwner(value) { need(value === null || owner(value), "TERMINAL_INVALID") }
function resource(value, kind, role = null, owners = null) {
  const helper = kind === "container" && role === "helper"
  record(value, kind === "container" ? (helper ? ["creation_state", "cid", "name", "labels"] : ["role", "cid", "labels", "name", "creation_state"]) :
    kind === "volume" ? ["role", "creation_state", "name", "created_at", "driver", "scope", "labels"] : ["id", "creation_state", "name", "labels"], "TERMINAL_INVALID")
  if (kind !== "network" && !helper) need(ROLES.includes(value.role) && (role === null || value.role === role), "TERMINAL_INVALID")
  need(["observed", "not_created", "unknown"].includes(value.creation_state), "TERMINAL_INVALID")
  const observed = value.creation_state === "observed", id = kind === "container" ? value.cid : kind === "network" ? value.id : value.name
  if (kind !== "volume") need(observed ? hex(id) : id === null, "TERMINAL_INVALID")
  need(value.name === null ? !observed : text(value.name) && /^[A-Za-z0-9_][A-Za-z0-9_.-]{0,255}$/u.exec(value.name)?.[0] === value.name, "TERMINAL_INVALID")
  need(value.labels === null ? !observed : typeof value.labels === "object" && !Array.isArray(value.labels), "TERMINAL_INVALID")
  if (kind === "volume") {
    for (const key of ["created_at", "driver", "scope"]) need(value[key] === null ? !observed : text(value[key]), "TERMINAL_INVALID")
    if (observed) need(Number.isFinite(Date.parse(value.created_at)), "TERMINAL_INVALID")
  }
  if (owners !== null) {
    const isRustfs = helper || value.role === "rustfs-service", expectedOwner = isRustfs ? owners.rustfs_owner : owners.tidb_owner
    need(!observed || expectedOwner !== null, "TERMINAL_INVALID")
    if (expectedOwner !== null) {
      const expectedName = helper ? `${expectedOwner}-cleanup` : kind === "network" ? `mount-rs-tidb-net-${expectedOwner}` : isRustfs ? expectedOwner :
        `mount-rs-tidb-${expectedOwner}-${value.role.replace("-", "")}${kind === "volume" ? "-data" : ""}`
      need(value.name === null || value.name === expectedName, "TERMINAL_INVALID")
      const labels = labelsFor(isRustfs ? "rustfs" : "tidb", expectedOwner)
      if (helper) labels["com.mount-rs.rustfs-test-purpose"] = "cleanup"
      if (value.labels !== null) same(value.labels, labels, "TERMINAL_INVALID")
    } else need(value.name === null && value.labels === null, "TERMINAL_INVALID")
  } else if (observed) {
    // Standalone classification still requires the same fixed owner label set.
    const labels = value.labels, rustfs = helper || value.role === "rustfs-service"
    const observedOwner = labels[rustfs ? "com.mount-rs.rustfs-test-run" : "mount-rs.tidb.run"]
    need(owner(observedOwner), "TERMINAL_INVALID")
    const expected = labelsFor(rustfs ? "rustfs" : "tidb", observedOwner)
    if (helper) expected["com.mount-rs.rustfs-test-purpose"] = "cleanup"
    same(labels, expected, "TERMINAL_INVALID")
  }
  return value
}
function targetId(kind, target) { return kind === "container" ? target.cid : kind === "network" ? target.id : target.name }
function selectedIdentity(kind, target) {
  return kind === "container" ? { id: target.cid, name: `/${target.name}`, labels: target.labels } : kind === "volume" ?
    { name: target.name, labels: target.labels, created_at: target.created_at, driver: target.driver, scope: target.scope } : { id: target.id, name: target.name, labels: target.labels }
}
function classify(kind, target, actionKind, output, processBytes) {
  need(["container", "volume", "network"].includes(kind) && ["inspect_owner", "remove", "inspect_absence"].includes(actionKind), "TERMINAL_INVALID")
  resource(target, kind, kind === "container" && !Object.hasOwn(target, "role") ? "helper" : null)
  need(target.creation_state === "observed", "TERMINAL_INVALID")
  const phase = actionKind === "inspect_owner" ? "inspect" : actionKind === "inspect_absence" ? "absence" : "remove"
  const process = processReceipt(processBytes, `${kind}.${phase}`), id = targetId(kind, target)
  let typed = "unknown", basis = "none", settled = false
  const expectedNotFound = kind === "container" ? `Error response from daemon: No such container: ${id}\n` : kind === "volume" ?
    `Error response from daemon: get ${id}: no such volume\n` : `Error response from daemon: network ${id} not found\n`
  // Expected absence never rewrites the failed generic process receipt. Only
  // this identity-scoped action may exempt a settled status1 CHILD_FAILED.
  if (actionKind === "inspect_absence" && quiescent(process) && process.raw_returncode === 1 && process.normalized_exit === 1 &&
      process.failure_code === "OWNED_LAYOUT_PROCESS_CHILD_FAILED" && process.sticky_failure &&
      (output.equals(Buffer.from(expectedNotFound)) || output.equals(Buffer.from(`\n${expectedNotFound}`)))) {
    typed = "absent"; basis = "exact_target_not_found"; settled = true
  } else if (clean(process)) {
    if (actionKind === "remove") {
      if (output.equals(Buffer.from(`${id}\n`))) { typed = "present"; settled = true }
    } else {
      let observed
      try { observed = parse(output) } catch (error) { if (!failures.has(error)) throw error }
      if (observed !== undefined && isDeepStrictEqual(observed, snapshot(selectedIdentity(kind, target)))) {
        typed = actionKind === "inspect_owner" ? "owned" : "present"; settled = actionKind === "inspect_owner"
      }
    }
  }
  return { settled, process, receipt: { schema: "mount-rs.owned-layout-resource-observation.v1", kind, target, action_kind: actionKind,
    raw_command_returncode: process.raw_returncode, output_sha256: sha(output), output_base64: output.toString("base64"), process_sha256: sha(processBytes),
    process_base64: processBytes.toString("base64"), process, typed_observation: typed, absence_basis: basis } }
}
export function classifyOwnedLayoutResourceObservation(input) {
  return boundary("resource_observation", input, (source) => {
    record(source, ["kind", "target", "action_kind", "output", "process"], "INPUT_INVALID")
    const observation = classify(source.kind, source.target, source.action_kind, buffer(source.output, 16384, true), buffer(source.process, 4096))
    return result("resource_observation", observation.receipt, { count: 1, status: observation.settled ? "ready" : "incomplete", uncertainty: !quiescent(observation.process),
      suffix: observation.settled ? null : "TERMINAL_INCOMPLETE" })
  })
}

function base64(value, maximum, suffix) {
  need(typeof value === "string" && value.length <= Math.ceil(maximum / 3) * 4, suffix)
  const bytes = Buffer.from(value, "base64")
  need(bytes.length <= maximum && bytes.toString("base64") === value, suffix); return bytes
}
function retainedCapacity(value, engineBytes) {
  record(value, ["schema", "engine_capability_sha256", "memory_bytes", "cpu_count", "min_memory_bytes", "min_cpu_count", "qualified", "observation_sha256", "observation_base64", "action", "action_sha256", "action_base64"], "CAPACITY_INVALID")
  need(value.schema === "mount-rs.owned-layout-engine-capacity.v1" && engineBytes !== null, "CAPACITY_INVALID")
  const observation = base64(value.observation_base64, 16384, "CAPACITY_INVALID"), actionBytes = base64(value.action_base64, 4096, "CAPACITY_INVALID")
  const reconstructed = capacityReceipt(engineBytes, observation, actionBytes)
  same(value, reconstructed.receipt, "CAPACITY_INVALID"); return reconstructed
}
function processEvidence(bytes, expected = null) {
  const value = processReceipt(bytes, expected)
  return { sha256: sha(bytes), base64: bytes.toString("base64"), value }
}
function originalBoundary(value, schema) {
  need(value !== null && typeof value === "object" && !Array.isArray(value) && value.schema === schema && ["modeled_controls", "production_entry_attempt"].includes(value.runtime_scope), "TERMINAL_INVALID")
  return value
}
function resources(value, fixture) {
  record(value, ["schema", "generation", "tidb_owner", "rustfs_owner", "containers", "volumes", "network", "rustfs_bind", "helper"], "TERMINAL_INVALID")
  need(value.schema === "mount-rs.owned-layout-resources.v1" && value.generation === "1", "TERMINAL_INVALID")
  nullableOwner(value.tidb_owner); nullableOwner(value.rustfs_owner)
  if (fixture !== null) need(value.tidb_owner === fixture.tidb_owner && value.rustfs_owner === fixture.rustfs_owner, "TERMINAL_INVALID")
  array(value.containers, 8, "TERMINAL_INVALID"); array(value.volumes, 6, "TERMINAL_INVALID")
  value.containers.forEach((entry, index) => {
    resource(entry, "container", ROLES[index], value)
    if (fixture !== null && entry.creation_state === "observed") same({ role: entry.role, cid: entry.cid, labels: entry.labels }, fixture.entries[index], "TERMINAL_INVALID")
  })
  value.volumes.forEach((entry, index) => resource(entry, "volume", ROLES[index], value)); resource(value.network, "network", null, value)
  const ids = [...value.containers.filter((entry) => entry.creation_state === "observed").map((entry) => entry.cid), ...(value.network.creation_state === "observed" ? [value.network.id] : [])]
  need(new Set(ids).size === ids.length, "TERMINAL_INVALID")
  if (value.helper !== null) {
    resource(value.helper, "container", "helper", value)
    need(value.helper.cid === null || !ids.includes(value.helper.cid), "TERMINAL_INVALID")
  }
  if (value.rustfs_bind !== null) {
    const bind = record(value.rustfs_bind, ["owner", "marker_verified", "path_validated", "data_path", "data_empty", "run_dir_removed"], "TERMINAL_INVALID")
    need(owner(bind.owner) && bind.owner === value.rustfs_owner && ["marker_verified", "path_validated", "data_empty", "run_dir_removed"].every((key) => bool(bind[key])) && text(bind.data_path) && bind.data_path.startsWith("/") && !bind.data_path.split("/").some((part) => part === "." || part === "..") && bind.data_path.endsWith("/data"), "TERMINAL_INVALID")
  }
  return value
}
function availableArtifacts(value) {
  record(value, Object.keys(INPUT_CAPS), "TERMINAL_INVALID")
  const raw = {}, parsed = {}, joins = {}
  for (const [key, cap] of Object.entries(INPUT_CAPS)) {
    raw[key] = value[key] === null ? null : buffer(value[key], cap)
    parsed[key] = raw[key] === null || key === "native" ? null : parse(raw[key])
    joins[`${key}_sha256`] = raw[key] === null ? null : sha(raw[key])
  }
  if (parsed.engine !== null) engine(parsed.engine)
  if (parsed.fixtures !== null) fixtures(parsed.fixtures)
  if (parsed.controller !== null) controller(parsed.controller, parsed.fixtures, joins.fixtures_sha256, joins.engine_sha256)
  if (parsed.build !== null) buildReceipt(parsed.build, joins.native_sha256)
  if (parsed.projection !== null) originalBoundary(parsed.projection, "mount-rs.owned-layout-entry.v1")
  if (parsed.originals !== null) originalBoundary(parsed.originals, "mount-rs.owned-layout-originals.v1")
  if (parsed.projection !== null && parsed.originals !== null) {
    need(parsed.projection.runtime_scope === parsed.originals.runtime_scope, "TERMINAL_INVALID")
    if (parsed.projection.originals !== undefined && parsed.projection.originals.status === "retained") {
      need(parsed.projection.originals.sha256 === joins.originals_sha256 && parsed.projection.originals.bytes === raw.originals.length, "TERMINAL_INVALID")
    }
  }
  // The full original/summary/counter replay remains the offline verifier's
  // responsibility. Validate byte/value handoff joins when the sidecar carries
  // them; never fabricate missing ones from a successful cleanup boundary.
  if (parsed.originals?.joins !== undefined) {
    const joined = parsed.originals.joins
    for (const key of ["fixtures", "controller", "engine", "build"]) {
      const row = joined[key]
      need(row !== null && typeof row === "object" && row.sha256 === joins[`${key}_sha256`] && isDeepStrictEqual(row.value, parsed[key]), "TERMINAL_INVALID")
    }
    need(joined.native?.sha256 === joins.native_sha256, "TERMINAL_INVALID")
  }
  const observedCapacity = parsed.capacity === null ? null : retainedCapacity(parsed.capacity, raw.engine)
  return { raw, parsed, joins, observedCapacity }
}
export function buildOwnedLayoutOwnerTerminal(input) {
  return boundary("owner_terminal", input, (source) => {
    record(source, ["artifacts", "resources", "observations", "groups", "producer_sources", "producer_receipt"], "INPUT_INVALID")
    const artifacts = availableArtifacts(source.artifacts), owned = resources(source.resources, artifacts.parsed.fixtures)
    if (artifacts.parsed.controller !== null) need(owned.tidb_owner === artifacts.parsed.controller.tidb_owner && owned.rustfs_owner === artifacts.parsed.controller.rustfs_owner, "TERMINAL_INVALID")
    const producerBytes = buffer(source.producer_receipt, 65536), producer = record(parse(producerBytes), ["schema", "checkout_sha", "source_sha256"], "SOURCE_INVALID")
    need(producer.schema === "mount-rs.owned-layout-producer-sources.v1" && hex(producer.checkout_sha, 40), "SOURCE_INVALID")
    same(producer.source_sha256, sourceHashes(source.producer_sources, PRODUCER_PATHS), "SOURCE_INVALID")
    need(artifacts.parsed.build === null || producer.checkout_sha === artifacts.parsed.build.checkout_sha, "SOURCE_INVALID")
    record(source.groups, ["outer", "actions"], "TERMINAL_INVALID")
    need(Array.isArray(source.groups.actions) && source.groups.actions.length <= 1024 && Array.isArray(source.observations) && source.observations.length <= 16, "TERMINAL_INVALID")
    const groups = { outer: source.groups.outer === null ? null : processEvidence(buffer(source.groups.outer, 4096), "outer.comparison"),
      actions: source.groups.actions.map((bytes) => processEvidence(buffer(bytes, 4096))) }
    const unused = new Map()
    groups.actions.forEach((row, index) => { const entries = unused.get(row.sha256) ?? []; entries.push(index); unused.set(row.sha256, entries) })
    let errors = 0, uncertainty = false, complete = Object.values(artifacts.raw).every((bytes) => bytes !== null), count = 0
    const processUncertain = (value) => !quiescent(value)
    if (groups.outer === null) complete = false
    else { if (!clean(groups.outer.value)) { errors++; complete = false }; uncertainty ||= processUncertain(groups.outer.value) }
    const comparison = artifacts.parsed.projection?.comparison
    if (comparison === undefined || comparison === null || comparison.complete !== true || comparison.safe_to_continue !== true || comparison.native_uncertainty !== false) complete = false
    if (comparison?.native_uncertainty === true || integer(comparison?.pending_owned_call_count) && comparison.pending_owned_call_count > 0) { uncertainty = true; complete = false }
    if (artifacts.parsed.projection?.publication !== undefined && artifacts.parsed.projection.publication.status !== "complete") complete = false
    if (artifacts.parsed.originals?.evidence?.complete !== true) complete = false
    if (artifacts.observedCapacity === null || !artifacts.observedCapacity.receipt.qualified) complete = false
    if (artifacts.observedCapacity !== null && !artifacts.observedCapacity.settled) { errors++; uncertainty = true }
    const targets = [...owned.containers.map((target) => ({ kind: "container", target, owner: target.role === "rustfs-service" ? owned.rustfs_owner : owned.tidb_owner })),
      ...owned.volumes.map((target) => ({ kind: "volume", target, owner: owned.tidb_owner })), { kind: "network", target: owned.network, owner: owned.tidb_owner },
      ...(owned.helper?.creation_state === "observed" ? [{ kind: "container", target: owned.helper, owner: owned.rustfs_owner }] : [])]
    const observedTargets = new Map()
    for (const row of targets) {
      if (row.target.creation_state !== "observed") { complete = false; uncertainty ||= row.target.creation_state === "unknown"; continue }
      observedTargets.set(`${row.kind}:${targetId(row.kind, row.target)}`, row)
    }
    const seen = new Set(); let totalActions = 0
    const observations = source.observations.map((row) => {
      record(row, ["kind", "id", "owner", "before_owner_verified", "after_absent", "actions"], "TERMINAL_INVALID")
      const key = `${row.kind}:${row.id}`, expected = observedTargets.get(key)
      need(expected !== undefined && !seen.has(key) && row.owner === expected.owner && bool(row.before_owner_verified) && bool(row.after_absent) && Array.isArray(row.actions) && row.actions.length <= 64, "TERMINAL_INVALID")
      seen.add(key); totalActions += row.actions.length; need(totalActions <= 1024, "TERMINAL_INVALID")
      let ownerVerified = false, absent = false, removed = false
      const actions = row.actions.map((action) => {
        record(action, ["phase", "action_kind", "output", "process", "container_delete_mode"], "TERMINAL_INVALID")
        need(["inspect", "remove", "absence"].includes(action.phase) && action.action_kind === (action.phase === "inspect" ? "inspect_owner" : action.phase === "absence" ? "inspect_absence" : "remove"), "TERMINAL_INVALID")
        need(action.container_delete_mode === (row.kind === "container" && action.phase === "remove" ? "force_owned_resource" : null), "TERMINAL_INVALID")
        const processBytes = buffer(action.process, 4096), classification = classify(row.kind, expected.target, action.action_kind, buffer(action.output, 16384, true), processBytes)
        const available = unused.get(sha(processBytes)); need(available?.length > 0, "TERMINAL_INVALID"); available.shift()
        uncertainty ||= processUncertain(classification.process)
        if (!classification.settled) { errors++; complete = false }
        if (action.phase === "inspect" && classification.receipt.typed_observation === "owned") ownerVerified = true
        if (action.phase === "remove") {
          if (!ownerVerified || absent) { errors++; complete = false }
          if (classification.settled && ownerVerified) removed = true
        }
        if (action.phase === "absence") {
          absent = classification.receipt.typed_observation === "absent"
          if (!ownerVerified || !removed) { errors++; complete = false }
        }
        return { phase: action.phase, action_kind: action.action_kind, container_delete_mode: action.container_delete_mode, observation: classification.receipt }
      })
      if (row.before_owner_verified !== ownerVerified || row.after_absent !== absent) { errors++; complete = false }
      ownerVerified &&= row.before_owner_verified; absent &&= row.after_absent
      if (!ownerVerified || !absent || !removed) complete = false
      else count++
      return { kind: row.kind, id: row.id, owner: row.owner, before_owner_verified: ownerVerified, after_absent: absent, actions }
    })
    for (const indices of unused.values()) for (const index of indices) {
      const value = groups.actions[index].value
      if (!clean(value)) { errors++; complete = false }; uncertainty ||= processUncertain(value)
    }
    if (seen.size !== observedTargets.size) complete = false
    const helperState = owned.helper === null ? "not_needed" : owned.helper.creation_state === "not_created" ? "not_created" : owned.helper.creation_state === "unknown" ? "creation_unknown" :
      seen.has(`container:${owned.helper.cid}`) && observations.find((row) => row.id === owned.helper.cid)?.after_absent ? "absent" : "unconfirmed"
    if (["creation_unknown", "unconfirmed"].includes(helperState)) { complete = false; uncertainty = true }
    const bind = owned.rustfs_bind
    if (bind === null || !bind.marker_verified || !bind.path_validated || !bind.data_empty || !bind.run_dir_removed) complete = false
    complete &&= !uncertainty && errors === 0
    const cleanCount = (kind) => observations.filter((row) => row.kind === kind && row.before_owner_verified && row.after_absent).length
    const containerCount = cleanCount("container") - (helperState === "absent" ? 1 : 0), volumeCount = cleanCount("volume"), networkCount = cleanCount("network")
    complete &&= containerCount === 8 && volumeCount === 6 && networkCount === 1
    const scope = artifacts.parsed.controller?.scope
    const receipt = { schema: "mount-rs.owned-layout-owner-terminal.v1", generation: "1", tidb_owner: owned.tidb_owner, rustfs_owner: owned.rustfs_owner,
      scope_sha256: scope === undefined ? null : sha(Buffer.from(JSON.stringify({ id: scope.id, owner: scope.owner, metadataPrefix: scope.metadataPrefix, blockPrefix: scope.blockPrefix }))),
      joins: { ...artifacts.joins, producer_source_receipt_sha256: sha(producerBytes) }, producer_checkout_sha: producer.checkout_sha, producer_source_sha256: producer.source_sha256,
      resources: owned, observations, groups, cleanup: { kind: "owned_backing_resources_destroyed", namespace_purge: "not_performed", complete,
        container_count: containerCount, tidb_volume_count: volumeCount, network_count: networkCount, helper_state: helperState }, operation_error_count: errors, native_uncertainty: uncertainty }
    return result("owner_terminal", receipt, { status: complete ? "ready" : "incomplete", count, uncertainty, suffix: complete ? null : "TERMINAL_INCOMPLETE", maximum: 33554432 })
  })
}
