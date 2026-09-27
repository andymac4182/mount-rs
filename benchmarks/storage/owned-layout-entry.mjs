import { Buffer } from "node:buffer"
import { execFile } from "node:child_process"
import { createHash, randomBytes } from "node:crypto"
import { constants } from "node:fs"
import { open, lstat, realpath, rename, unlink } from "node:fs/promises"
import { createRequire } from "node:module"
import { basename, dirname, isAbsolute, join, resolve } from "node:path"
import { fileURLToPath } from "node:url"
import { isDeepStrictEqual } from "node:util"
import { createOwnedLayoutComparison, OUTPUT_CAP } from "./owned-layout-comparison.mjs"
import { assessOwnedLayoutRunnerOutcome } from "./owned-layout-outcome.mjs"
import { projectOwnedLayoutPhaseMetrics } from "./owned-layout-metrics.mjs"
import { observePilotBinding } from "./owned-backing-pilot.mjs"

// Runtime/controller seam hashes; the remaining native source joins the clean
// exact Git revision in the build seal. This is not a full dependency manifest.
export const SOURCE_PATHS = Object.freeze([
  "Cargo.toml", "Cargo.lock", "bindings/mount-rs-napi/Cargo.toml", "bindings/mount-rs-napi/index.js",
  "bindings/mount-rs-napi/src/lib.rs", "bindings/mount-rs-napi/src/namespace_presence.rs", "src/diagnostics/storage.rs", "src/diagnostics/profile.rs",
  "scripts/test-tidb.sh", "scripts/test-rustfs.sh", "scripts/rustfs-combo-runner.py", "scripts/rustfs-bounded-docker.py",
  ".github/workflows/remote-drives.yml", "benchmarks/storage/errors.mjs", "benchmarks/storage/stats.mjs", "benchmarks/storage/runner.mjs",
  "benchmarks/storage/providers.mjs", "benchmarks/storage/diagnostics.mjs", "benchmarks/storage/backing-engine-transport.mjs",
  "benchmarks/storage/backing-observer.mjs", "benchmarks/storage/compact-layout.mjs", "benchmarks/storage/namespace-presence.mjs",
  "benchmarks/storage/owned-backing-pilot.mjs", "benchmarks/storage/owned-layout-arm.mjs", "benchmarks/storage/owned-layout-outcome.mjs",
  "benchmarks/storage/owned-layout-metrics.mjs", "benchmarks/storage/owned-layout-comparison.mjs", "benchmarks/storage/owned-layout-entry.mjs",
])
const directory = dirname(fileURLToPath(import.meta.url)), repo = resolve(directory, "../..")
const bindingPath = join(repo, "bindings/mount-rs-napi/index.js"), require = createRequire(import.meta.url)
const INPUT_CAP = 16_384, BUILD_CAP = 65_536, SOURCE_CAP = 2_097_152, ADDON_CAP = 134_217_728, OUTCOME_RESERVE = 65_536
const INPUT_PATH_KEYS = Object.freeze(["MOUNT_RS_BACKING_CID_RECEIPT", "MOUNT_RS_BACKING_ENGINE_CAPABILITY", "MOUNT_RS_OWNED_LAYOUT_CONTROLLER_RECEIPT", "MOUNT_RS_OWNED_LAYOUT_BUILD_RECEIPT"])
const OUTPUT_PATH_KEYS = Object.freeze(["MOUNT_RS_OWNED_LAYOUT_OUTPUT", "MOUNT_RS_OWNED_LAYOUT_ORIGINALS_OUTPUT"])
const ORIGINALS_SCHEMA = "mount-rs.owned-layout-originals.v1"
const ROLES = Object.freeze(["pd-1", "pd-2", "pd-3", "tikv-1", "tikv-2", "tikv-3", "tidb", "rustfs-service"])
const ORDER = Object.freeze(["A1", "B1", "B2", "A2"]), LAYOUTS = Object.freeze(["legacy", "compact", "compact", "legacy"])
const ENV_KEYS = Object.freeze([
  "NAPI_RS_NATIVE_LIBRARY_PATH", "NAPI_RS_FORCE_WASI", "NAPI_RS_WASI_FLAVOR", "NODE_PATH", "NODE_OPTIONS", "MOUNT_RS_PROFILE_IO", "MOUNT_RS_TRACE_STORAGE",
  "MOUNT_RS_TIDB_URL", "TIDB_URL", "MOUNT_RS_TIDB_DURABLE", "MOUNT_RS_RUSTFS_ENDPOINT", "MOUNT_RS_RUSTFS_BUCKET", "MOUNT_RS_RUSTFS_REGION",
  "MOUNT_RS_RUSTFS_ACCESS_KEY_ID", "MOUNT_RS_RUSTFS_SECRET_ACCESS_KEY", "MOUNT_RS_RUSTFS_DURABLE", "MOUNT_RS_BACKING_GENERATION",
  "MOUNT_RS_BACKING_TIDB_OWNER", "MOUNT_RS_BACKING_RUSTFS_OWNER", "MOUNT_RS_BACKING_EXPECT_TIDB_URL", "MOUNT_RS_BACKING_EXPECT_R2_ENDPOINT",
  "MOUNT_RS_BACKING_EXPECT_R2_BUCKET", "MOUNT_RS_BACKING_CID_RECEIPT", "MOUNT_RS_BACKING_ENGINE_CAPABILITY",
  "MOUNT_RS_OWNED_LAYOUT_CONTROLLER_RECEIPT", "MOUNT_RS_OWNED_LAYOUT_BUILD_RECEIPT", "MOUNT_RS_OWNED_LAYOUT_OUTPUT", "MOUNT_RS_OWNED_LAYOUT_ORIGINALS_OUTPUT", "DOCKER_HOST", "DOCKER_CONTEXT",
  "DOCKER_TLS", "DOCKER_TLS_VERIFY", "DOCKER_CERT_PATH", "MOUNTX_SOURCE",
])
const SUFFIXES = Object.freeze(["CONFIG_INVALID", "HANDOFF_REJECTED", "ENDPOINT_BINDING_REJECTED", "ENGINE_BINDING_REJECTED", "SCOPE_REJECTED",
  "PRIVATE_FILE_REJECTED", "JSON_REJECTED", "BUILD_IDENTITY_REJECTED", "SOURCE_IDENTITY_REJECTED", "NATIVE_SELECTION_REJECTED",
  "NATIVE_LOAD_FAILED", "NATIVE_JOIN_REJECTED", "CAPTURE_JOIN_REJECTED", "OUTPUT_CAP", "PUBLICATION_FAILED", "COMPARISON_FAILED", "TESTING_REJECTED"])
const CODES = new Set(SUFFIXES.map((suffix) => `OWNED_LAYOUT_ENTRY_${suffix}`)), issued = new WeakSet()
const fail = (suffix) => { const code = `OWNED_LAYOUT_ENTRY_${suffix}`, error = Object.assign(new Error(code), { code }); issued.add(error); throw error }
const ownCode = (error, fallback) => issued.has(error) && CODES.has(error.code) ? error.code : `OWNED_LAYOUT_ENTRY_${fallback}`
const object = (value) => value !== null && typeof value === "object" && !Array.isArray(value)
const hash = (value) => typeof value === "string" && /^[a-f0-9]{64}$/u.exec(value)?.[0] === value
const text = (value) => typeof value === "string" && value.length > 0 && !value.includes("\0") && Buffer.byteLength(value) <= 4096
const owner = (value) => typeof value === "string" && /^[A-Za-z0-9_][A-Za-z0-9_.-]{0,119}$/u.exec(value)?.[0] === value && ![".", ".."].includes(value)
const pathText = (value) => text(value) && isAbsolute(value) && resolve(value) === value
const sha = (bytes) => createHash("sha256").update(bytes).digest("hex")
const frozen = (value) => { if (value && typeof value === "object" && !Object.isFrozen(value)) { for (const child of Object.values(value)) frozen(child); Object.freeze(value) }; return value }
function record(source, fields, code = "CONFIG_INVALID") {
  if (!object(source)) fail(code)
  const descriptors = Object.getOwnPropertyDescriptors(source)
  if (Reflect.ownKeys(descriptors).length !== fields.length || !fields.every((field) => Object.hasOwn(descriptors, field) && Object.hasOwn(descriptors[field], "value"))) fail(code)
  return Object.fromEntries(fields.map((field) => [field, descriptors[field].value]))
}
function data(source, maximum = BUILD_CAP, code = "CONFIG_INVALID") {
  let nodes = 0, bytes = 0
  const active = new Set(), seen = new Map()
  function copy(value, depth) {
    if (++nodes > 200_000 || depth > 40) fail(code)
    if (value === null || value === undefined || typeof value === "boolean") return value
    if (typeof value === "number") { if (!Number.isFinite(value)) fail(code); return value }
    if (typeof value === "string") { bytes += Buffer.byteLength(value); if (bytes > maximum) fail(code); return value }
    if (typeof value !== "object" || active.has(value)) fail(code)
    if (seen.has(value)) return seen.get(value)
    const array = Array.isArray(value), prototype = Object.getPrototypeOf(value)
    if (prototype !== (array ? Array.prototype : Object.prototype) && prototype !== null) fail(code)
    const descriptors = Object.getOwnPropertyDescriptors(value), keys = Reflect.ownKeys(descriptors), length = array ? descriptors.length?.value : null
    if (array && (!Number.isSafeInteger(length) || length < 0 || length > 100_000 || keys.length !== length + 1 || !Array.from({ length }, (_, index) => index).every((index) => Object.hasOwn(descriptors, String(index))))) fail(code)
    const result = array ? [] : {}
    seen.set(value, result); active.add(value)
    for (const key of keys) {
      if (array && key === "length") continue
      if (typeof key !== "string" || !Object.hasOwn(descriptors[key], "value")) fail(code)
      bytes += Buffer.byteLength(key) + 8
      if (bytes > maximum) fail(code)
      Object.defineProperty(result, key, { value: copy(descriptors[key].value, depth + 1), enumerable: true, writable: true, configurable: true })
    }
    active.delete(value)
    return result
  }
  return copy(source, 0)
}
function pinEnvironment(source) {
  if (!object(source)) fail("CONFIG_INVALID")
  const result = {}
  for (const key of ENV_KEYS) {
    const descriptor = Object.getOwnPropertyDescriptor(source, key)
    if (!descriptor) continue
    if (!Object.hasOwn(descriptor, "value") || descriptor.value !== undefined && typeof descriptor.value !== "string" || typeof descriptor.value === "string" && Buffer.byteLength(descriptor.value) > 4096) fail("CONFIG_INVALID")
    result[key] = descriptor.value
  }
  return frozen(result)
}
function testingOptions(source, preflight = false) {
  if (!object(source) || ![Object.prototype, null].includes(Object.getPrototypeOf(source))) fail("TESTING_REJECTED")
  const descriptors = Object.getOwnPropertyDescriptors(source), keys = Reflect.ownKeys(descriptors)
  if (keys.length === 0) return { modeled: false, maximum: OUTPUT_CAP }
  const allowed = preflight ? ["mode", "checkoutMetadata"] : ["mode", "checkoutMetadata", "loadBinding", "comparisonDependencies", "publicationMaximum"]
  if (keys.some((key) => !allowed.includes(key))) fail("TESTING_REJECTED")
  const value = record(source, keys, "TESTING_REJECTED")
  if (value.mode !== "modeled" || typeof value.checkoutMetadata !== "function" || !preflight && typeof value.loadBinding !== "function") fail("TESTING_REJECTED")
  const maximum = value.publicationMaximum ?? OUTPUT_CAP
  if (!Number.isSafeInteger(maximum) || maximum < OUTCOME_RESERVE || maximum > OUTPUT_CAP) fail("TESTING_REJECTED")
  if (!preflight && value.comparisonDependencies !== undefined) {
    if (!object(value.comparisonDependencies)) fail("TESTING_REJECTED")
    const dependencies = record(value.comparisonDependencies, Reflect.ownKeys(Object.getOwnPropertyDescriptors(value.comparisonDependencies)), "TESTING_REJECTED")
    if (Object.keys(dependencies).some((key) => !["createArm", "runBenchmark", "providerById", "createTransport", "createObserver", "verifyBinding", "clock"].includes(key))) fail("TESTING_REJECTED")
    value.comparisonDependencies = dependencies
  }
  return { ...value, modeled: true, maximum }
}
async function privateParent(path) {
  if (!pathText(path) || typeof process.getuid !== "function") fail("PRIVATE_FILE_REJECTED")
  const parent = await lstat(dirname(path))
  if (!parent.isDirectory() || parent.uid !== process.getuid() || (parent.mode & 0o777) !== 0o700) fail("PRIVATE_FILE_REJECTED")
}
async function boundedRead(path, maximum, privateFile = false, expectedIdentity = null) {
  let file
  try {
    if (!pathText(path)) fail("PRIVATE_FILE_REJECTED")
    if (privateFile) await privateParent(path)
    file = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW)
    const first = await file.stat()
    if (!first.isFile() || first.size < 1 || first.size > maximum || privateFile && (first.uid !== process.getuid() || (first.mode & 0o777) !== 0o600 || first.nlink !== 1)) fail("PRIVATE_FILE_REJECTED")
    if (expectedIdentity) {
      const exact = await file.stat({ bigint: true })
      if (exact.dev !== expectedIdentity.dev || exact.ino !== expectedIdentity.ino) fail("PRIVATE_FILE_REJECTED")
    }
    const buffer = Buffer.alloc(first.size + 1)
    let used = 0
    while (used < buffer.length) { const { bytesRead } = await file.read(buffer, used, buffer.length - used, null); if (!bytesRead) break; used += bytesRead }
    const last = await file.stat()
    if (used !== first.size || last.size !== first.size || last.mtimeMs !== first.mtimeMs || last.ctimeMs !== first.ctimeMs || last.ino !== first.ino || last.dev !== first.dev) fail("PRIVATE_FILE_REJECTED")
    if (expectedIdentity) {
      const current = await lstat(path, { bigint: true })
      if (!current.isFile() || current.dev !== expectedIdentity.dev || current.ino !== expectedIdentity.ino || current.uid !== BigInt(process.getuid()) ||
          (current.mode & 0o777n) !== 0o600n || current.nlink !== 1n) fail("PRIVATE_FILE_REJECTED")
    }
    return buffer.subarray(0, used)
  } catch { fail("PRIVATE_FILE_REJECTED") }
  finally { await file?.close() }
}
// JSON.parse alone silently accepts duplicate keys. Scan decoded key tokens
// before parsing, retaining existing whitespace/ordering in private producers.
function parsePrivateJSON(bytes) {
  try {
    const source = new TextDecoder("utf-8", { fatal: true }).decode(bytes), stack = []
    for (let index = 0; index < source.length; index++) {
      const char = source[index]
      if (char === "{" || char === "[") { stack.push(char === "{" ? new Set() : null); if (stack.length > 40) fail("JSON_REJECTED") }
      else if (char === "}" || char === "]") stack.pop()
      else if (char === '"') {
        const start = index++
        for (; index < source.length && source[index] !== '"'; index++) if (source[index] === "\\") index++
        if (index >= source.length) fail("JSON_REJECTED")
        let next = index + 1
        while (/\s/u.test(source[next] || "") && next < source.length) next++
        if (source[next] === ":") {
          const keys = stack.at(-1), key = JSON.parse(source.slice(start, index + 1))
          if (!keys || keys.has(key)) fail("JSON_REJECTED")
          keys.add(key)
        }
      }
    }
    return data(JSON.parse(source), BUILD_CAP, "JSON_REJECTED")
  } catch { fail("JSON_REJECTED") }
}
async function privateJSON(path, maximum = INPUT_CAP) {
  const bytes = await boundedRead(path, maximum, true)
  return { value: parsePrivateJSON(bytes), sha256: sha(bytes) }
}
export function validateOwnedLayoutHandoff(input, environment) {
  const source = record(data(input), ["fixtures", "controller", "engine", "fixture_sha256", "engine_sha256"], "HANDOFF_REJECTED"), env = pinEnvironment(environment)
  const fixtures = record(source.fixtures, ["schema", "tidb_owner", "rustfs_owner", "generation", "entries"], "HANDOFF_REJECTED")
  const controller = record(source.controller, ["schema", "generation", "tidb_owner", "rustfs_owner", "fixture_sha256", "engine_capability_sha256", "tidb_url", "rustfs_endpoint", "rustfs_bucket", "scope"], "HANDOFF_REJECTED")
  if (fixtures.schema !== "mount-rs.owned-backing-cids.v1" || controller.schema !== "mount-rs.owned-layout-controller.v1" || fixtures.generation !== "1" || controller.generation !== "1" || env.MOUNT_RS_BACKING_GENERATION !== "1" ||
      !owner(fixtures.tidb_owner) || !owner(fixtures.rustfs_owner) || fixtures.tidb_owner !== controller.tidb_owner || fixtures.rustfs_owner !== controller.rustfs_owner ||
      fixtures.tidb_owner !== env.MOUNT_RS_BACKING_TIDB_OWNER || fixtures.rustfs_owner !== env.MOUNT_RS_BACKING_RUSTFS_OWNER ||
      !hash(source.fixture_sha256) || source.fixture_sha256 !== controller.fixture_sha256 || !hash(source.engine_sha256) || source.engine_sha256 !== controller.engine_capability_sha256 || !Array.isArray(fixtures.entries) || fixtures.entries.length !== 8) fail("HANDOFF_REJECTED")
  const cids = new Set(), entries = ROLES.map((role) => {
    const matches = fixtures.entries.filter((entry) => entry?.role === role)
    if (matches.length !== 1) fail("HANDOFF_REJECTED")
    const entry = record(matches[0], ["cid", "role", "labels"], "HANDOFF_REJECTED")
    const labels = role === "rustfs-service" ? { "com.mount-rs.rustfs-test": "mount-rs-rustfs-test", "com.mount-rs.rustfs-test-run": fixtures.rustfs_owner } : { "mount-rs.tidb.run": fixtures.tidb_owner }
    if (!hash(entry.cid) || cids.has(entry.cid) || !isDeepStrictEqual(record(entry.labels, Object.keys(labels), "HANDOFF_REJECTED"), labels)) fail("HANDOFF_REJECTED")
    cids.add(entry.cid); return { role, cid: entry.cid, labels }
  })
  const engine = record(source.engine, ["socket_path"], "ENGINE_BINDING_REJECTED")
  if (engine.socket_path !== "/var/run/docker.sock" || env.DOCKER_HOST !== "unix:///var/run/docker.sock" || ["DOCKER_CONTEXT", "DOCKER_TLS", "DOCKER_TLS_VERIFY", "DOCKER_CERT_PATH"].some((key) => env[key] !== undefined)) fail("ENGINE_BINDING_REJECTED")
  if (!text(controller.tidb_url) || !text(controller.rustfs_endpoint) || !text(controller.rustfs_bucket) ||
      (env.MOUNT_RS_TIDB_URL || env.TIDB_URL) !== controller.tidb_url || env.MOUNT_RS_BACKING_EXPECT_TIDB_URL !== controller.tidb_url ||
      env.MOUNT_RS_RUSTFS_ENDPOINT !== controller.rustfs_endpoint || env.MOUNT_RS_BACKING_EXPECT_R2_ENDPOINT !== controller.rustfs_endpoint ||
      env.MOUNT_RS_RUSTFS_BUCKET !== controller.rustfs_bucket || env.MOUNT_RS_BACKING_EXPECT_R2_BUCKET !== controller.rustfs_bucket ||
      !["MOUNT_RS_RUSTFS_REGION", "MOUNT_RS_RUSTFS_ACCESS_KEY_ID", "MOUNT_RS_RUSTFS_SECRET_ACCESS_KEY"].every((key) => text(env[key]))) fail("ENDPOINT_BINDING_REJECTED")
  try {
    const tidb = new URL(controller.tidb_url), blob = new URL(controller.rustfs_endpoint)
    const port = (url) => /^[1-9][0-9]{0,4}$/u.test(url.port) && Number(url.port) <= 65535
    if (tidb.protocol !== "mysql:" || tidb.hostname !== "127.0.0.1" || !port(tidb) || tidb.search || tidb.hash ||
        blob.protocol !== "http:" || blob.hostname !== "127.0.0.1" || !port(blob) || blob.username || blob.password || blob.search || blob.hash || blob.pathname !== "/") fail("ENDPOINT_BINDING_REJECTED")
  } catch { fail("ENDPOINT_BINDING_REJECTED") }
  const scope = record(controller.scope, ["id", "owner", "metadataPrefix", "blockPrefix"], "SCOPE_REJECTED")
  if (typeof scope.id !== "string" || /^layout-[a-f0-9]{32}$/u.exec(scope.id)?.[0] !== scope.id || scope.owner !== fixtures.tidb_owner ||
      scope.metadataPrefix !== `mount-rs-owned-layout/${fixtures.tidb_owner}/${scope.id}` || scope.blockPrefix !== `mount-rs-owned-layout/${fixtures.rustfs_owner}/${scope.id}`) fail("SCOPE_REJECTED")
  return frozen({ fixtures: { ...fixtures, entries }, engine: { socketPath: engine.socket_path }, scope, public: {
    status: "verified", generation: "1", fixture_count: 8, fixture_sha256: source.fixture_sha256, engine_capability_sha256: source.engine_sha256,
    scope_sha256: sha(JSON.stringify(scope)), endpoint_binding: "controller_manifest_join_verified", scope_binding: "owner_scoped_prefixes_verified",
    evidence_scope: "private_controller_created_endpoint_and_manifest_join; not_active_endpoint_probe",
  } })
}
async function checkoutMetadata() {
  const read = (args) => new Promise((resolveResult, rejectResult) => execFile("git", args, { cwd: repo, encoding: "utf8", maxBuffer: 1_048_576, timeout: 5000,
    env: { PATH: process.env.PATH, LANG: "C", GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null", GIT_OPTIONAL_LOCKS: "0" } },
  (error, stdout) => error ? rejectResult(error) : resolveResult(stdout)))
  try {
    const head = await read(["rev-parse", "HEAD"]), status = await read(["status", "--porcelain=v1", "--untracked-files=all"])
    if (!/^[a-f0-9]{40}\n$/u.test(head)) fail("BUILD_IDENTITY_REJECTED")
    return { checkout_sha: head.trim(), source_clean: status === "" }
  } catch { fail("BUILD_IDENTITY_REJECTED") }
}
export async function preflightOwnedLayoutIdentity(environment, seal, testing = {}) {
  const env = pinEnvironment(environment), options = testingOptions(testing, true), build = record(data(seal), ["schema", "checkout_sha", "source_clean", "locked", "release", "no_js", "build_exit_code", "native_sha256", "source_sha256"], "BUILD_IDENTITY_REJECTED")
  const selection = env.NAPI_RS_NATIVE_LIBRARY_PATH
  if (!/^v24\./u.test(process.version) || !pathText(selection) || !selection.endsWith(".node") || selection.startsWith(`${repo}/`) || env.MOUNT_RS_PROFILE_IO !== "1" || env.MOUNT_RS_TRACE_STORAGE === "1" ||
      ["NAPI_RS_FORCE_WASI", "NAPI_RS_WASI_FLAVOR", "NODE_PATH", "NODE_OPTIONS"].some((key) => env[key] !== undefined) || require.cache[bindingPath] || require.cache[selection]) fail("NATIVE_SELECTION_REJECTED")
  if (!options.modeled && (process.env.NAPI_RS_NATIVE_LIBRARY_PATH !== selection || process.env.MOUNT_RS_PROFILE_IO !== "1" || process.env.MOUNT_RS_TRACE_STORAGE === "1" ||
      ["NAPI_RS_FORCE_WASI", "NAPI_RS_WASI_FLAVOR", "NODE_PATH", "NODE_OPTIONS"].some((key) => process.env[key] !== undefined))) fail("NATIVE_SELECTION_REJECTED")
  try { if (await realpath(selection) !== selection) fail("NATIVE_SELECTION_REJECTED") }
  catch { fail("NATIVE_SELECTION_REJECTED") }
  if (build.schema !== "mount-rs.owned-layout-native-build.v1" || typeof build.checkout_sha !== "string" || /^[a-f0-9]{40}$/u.exec(build.checkout_sha)?.[0] !== build.checkout_sha ||
      build.source_clean !== true || build.locked !== true || build.release !== true || build.no_js !== true || build.build_exit_code !== 0 || !hash(build.native_sha256)) fail("BUILD_IDENTITY_REJECTED")
  const sources = record(build.source_sha256, SOURCE_PATHS, "SOURCE_IDENTITY_REJECTED")
  if (!SOURCE_PATHS.every((path) => hash(sources[path]))) fail("SOURCE_IDENTITY_REJECTED")
  let metadata
  try { metadata = record(data(await (options.modeled ? options.checkoutMetadata() : checkoutMetadata())), ["checkout_sha", "source_clean"], "BUILD_IDENTITY_REJECTED") }
  catch { fail("BUILD_IDENTITY_REJECTED") }
  if (metadata.checkout_sha !== build.checkout_sha || metadata.source_clean !== true) fail("BUILD_IDENTITY_REJECTED")
  let native
  try { native = sha(await boundedRead(selection, ADDON_CAP)) } catch { fail("NATIVE_SELECTION_REJECTED") }
  if (native !== build.native_sha256) fail("BUILD_IDENTITY_REJECTED")
  for (const path of SOURCE_PATHS) {
    let digest
    try { digest = sha(await boundedRead(join(repo, path), SOURCE_CAP)) } catch { fail("SOURCE_IDENTITY_REJECTED") }
    if (digest !== sources[path]) fail("SOURCE_IDENTITY_REJECTED")
  }
  return frozen({ selection, sha256: native, mock: false, public: { kind: "selected_native_file", native_sha256: native, real_native_proof: "unverified",
    source_sha256: sources, source_is_binary_build_revision: "clean_exact_git_revision_and_build_seal_boundary_join; not_immutable_interval_proof" } })
}
function captureJoin(comparison, privateEvidence) {
  if (privateEvidence.schema !== "mount-rs.owned-layout-private-evidence.v1" || privateEvidence.complete !== comparison.complete || !Array.isArray(privateEvidence.arms)) fail("CAPTURE_JOIN_REJECTED")
  const seen = new Set()
  for (const source of privateEvidence.arms) {
    if (!ORDER.includes(source.role) || seen.has(source.role)) fail("CAPTURE_JOIN_REJECTED")
    seen.add(source.role)
    const targets = comparison.arms.filter((arm) => arm.role === source.role)
    const phase = source.benchmark.providers?.[0]?.storageDiagnostics?.phases?.filter((item) => typeof item?.name === "string" && item.name.startsWith("workload-"))
    if (targets.length !== 1 || !isDeepStrictEqual(targets[0].outcome, assessOwnedLayoutRunnerOutcome(source.benchmark)) ||
        !isDeepStrictEqual(targets[0].native_metrics, projectOwnedLayoutPhaseMetrics(phase?.length === 1 ? phase[0] : null))) fail("CAPTURE_JOIN_REJECTED")
  }
  if (comparison.arms.some((arm) => arm.outcome !== null && !seen.has(arm.role))) fail("CAPTURE_JOIN_REJECTED")
}
function encode(source, maximum) {
  let size = 0, nodes = 0
  const chunks = [], active = new Set()
  const append = (part) => { size += Buffer.byteLength(part); if (size > maximum) fail("OUTPUT_CAP"); chunks.push(part) }
  function visit(value, depth) {
    if (++nodes > 500_000 || depth > 40) fail("OUTPUT_CAP")
    if (value === null || typeof value === "boolean" || typeof value === "number" && Number.isFinite(value)) { append(JSON.stringify(value)); return }
    if (typeof value === "string") { if (Buffer.byteLength(value) > maximum - size) fail("OUTPUT_CAP"); append(JSON.stringify(value)); return }
    if (!value || typeof value !== "object" || active.has(value)) fail("PUBLICATION_FAILED")
    const descriptors = Object.getOwnPropertyDescriptors(value), keys = Reflect.ownKeys(descriptors), array = Array.isArray(value)
    if (keys.some((key) => typeof key !== "string" || !Object.hasOwn(descriptors[key], "value"))) fail("PUBLICATION_FAILED")
    const length = array ? descriptors.length.value : null
    if (array && (keys.length !== length + 1 || !Array.from({ length }, (_, index) => index).every((index) => Object.hasOwn(descriptors, String(index))))) fail("PUBLICATION_FAILED")
    active.add(value); append(array ? "[" : "{")
    const fields = array ? Array.from({ length }, (_, index) => String(index)) : keys.sort()
    fields.forEach((key, index) => { if (index) append(","); if (!array) { append(JSON.stringify(key)); append(":") }; visit(descriptors[key].value, depth + 1) })
    append(array ? "]" : "}"); active.delete(value)
  }
  visit(source, 0); append("\n")
  return Buffer.from(chunks.join(""))
}
function incompleteComparison(source) {
  if (!source) return null
  return { schema: "mount-rs.owned-layout-comparison-incomplete.v1", status: source.status, complete: false, comparable: false, safe_to_continue: false,
    floor_qualified: source.floor_qualified, native_uncertainty: source.native_uncertainty, pending_owned_call_count: source.pending_owned_call_count,
    completed_arms: source.completed_arms, stopped_after: source.stopped_after, stop_code: source.stop_code,
    required_evidence: "omitted_output_cap_or_publication_failure", arms: source.arms.map((arm, index) => ({ role: ORDER[index], layout: LAYOUTS[index], status: arm.status, outcome: arm.outcome })) }
}
function publicationPaths(environment) {
  if (!object(environment)) fail("PRIVATE_FILE_REJECTED")
  const values = [...OUTPUT_PATH_KEYS, ...INPUT_PATH_KEYS, "NAPI_RS_NATIVE_LIBRARY_PATH"].map((key) => {
    const descriptor = Object.getOwnPropertyDescriptor(environment, key)
    if (!descriptor || !Object.hasOwn(descriptor, "value") || !pathText(descriptor.value)) fail("PRIVATE_FILE_REJECTED")
    return descriptor.value
  })
  return { outputs: values.slice(0, 2), inputs: values.slice(2) }
}
// Keep receipt protection independent of setup success. Canonical paths cover
// ancestor symlinks; exact device/inode pairs cover receipt/native hardlinks.
// Retain initial identities as well as checking both current output identities.
async function separatePublicationTargets(paths, observed = { inputs: [], outputs: [] }) {
  try {
    const inputs = []
    for (const path of paths.inputs) {
      const canonical = await realpath(path), identity = await lstat(canonical, { bigint: true })
      inputs.push({ canonical, dev: identity.dev, ino: identity.ino })
    }
    const outputs = []
    for (const path of paths.outputs) {
      let canonical, identity
      try { canonical = await realpath(path); identity = await lstat(canonical, { bigint: true }) }
      catch (error) {
        if (error.code !== "ENOENT") throw error
        canonical = join(await realpath(dirname(path)), basename(path))
      }
      outputs.push({ canonical, ...(identity ? { dev: identity.dev, ino: identity.ino } : {}) })
    }
    for (const [index, output] of outputs.entries()) {
      const protectedIdentities = [...inputs, ...observed.inputs, outputs[1 - index], ...observed.outputs.filter((_, peer) => peer !== index), ...(paths.published || []).filter((receipt) => receipt.index !== index)]
      if (paths.inputs.includes(paths.outputs[index]) || paths.outputs[index] === paths.outputs[1 - index] || protectedIdentities.some((input) => input.canonical === output.canonical ||
          output.dev !== undefined && input.dev === output.dev && input.ino === output.ino)) fail("PRIVATE_FILE_REJECTED")
    }
    return { inputs, outputs }
  } catch { fail("PRIVATE_FILE_REJECTED") }
}
async function recheckPublication(target) {
  if (target.revoked) fail("PUBLICATION_FAILED")
  try {
    for (const receipt of target.published) {
      try {
        const bytes = await boundedRead(target.outputs[receipt.index], OUTPUT_CAP, true, receipt)
        if (bytes.length !== receipt.bytes || sha(bytes) !== receipt.sha256) fail("PRIVATE_FILE_REJECTED")
      } catch (error) { if (receipt.index === 1) target.originalsInvalid = true; throw error }
    }
    await separatePublicationTargets(target, target.observed)
  }
  catch { target.revoked = true; fail("PUBLICATION_FAILED") }
}
async function privateWrite(target, index, bytes) {
  const path = target.outputs[index]
  let file, temporary
  try {
    await recheckPublication(target)
    await privateParent(path)
    try {
      const target = await lstat(path)
      if (!target.isFile() || target.uid !== process.getuid() || (target.mode & 0o777) !== 0o600 || target.nlink !== 1) fail("PRIVATE_FILE_REJECTED")
    } catch (error) { if (error.code !== "ENOENT") throw error }
    temporary = join(dirname(path), `.mount-rs-owned-layout-${randomBytes(12).toString("hex")}.tmp`)
    file = await open(temporary, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW, 0o600)
    await file.writeFile(bytes)
    const owned = await file.stat({ bigint: true })
    if (!owned.isFile() || owned.size !== BigInt(bytes.length)) fail("PUBLICATION_FAILED")
    const receipt = frozen({ index, canonical: target.observed.outputs[index].canonical, dev: owned.dev, ino: owned.ino, bytes: bytes.length, sha256: sha(bytes) })
    await file.close(); file = null
    await recheckPublication(target)
    await rename(temporary, path)
    // Bind the actual owned temporary inode and verify stable current bytes.
    // These are publication boundary observations, not interval immutability.
    target.published.push(receipt)
    await recheckPublication(target)
  } catch { fail("PUBLICATION_FAILED") }
  finally { await file?.close(); if (temporary) await unlink(temporary).catch(() => {}) }
}
export async function runOwnedLayoutEntry(environment = process.env, testing = {}) {
  let publicationTarget, maximum = OUTPUT_CAP, comparisonCapability
  const result = { schema: "mount-rs.owned-layout-entry.v1", runtime_scope: "production_entry_attempt", failure_code: null,
    publication: { status: "incomplete", reason: null }, comparison: null, handoff: null,
    originals: { schema: ORIGINALS_SCHEMA, status: "unavailable", sha256: null, bytes: null, count: null },
    identity: { kind: "selected_native_file", native_sha256: null, native_used_identity: "unverified", profiling_enabled_before_load: false },
    build: { status: "unavailable" }, ownership: { tidb_owner: null, rustfs_owner: null, generation: null, scope_sha256: null,
      final_teardown: { status: "unverified" }, namespace_purge: "unverified" },
    qualification: { hosted: false, floor_qualified: false, comparable: false, safe_to_continue: false, reason: "owner_teardown_and_independent_verification_required" },
    evidence_scope: { controller: "new_private_handoff_contract; producer_integration_not_established_here", source_and_build: "boundary_hash_and_clean_git_build_seal_observations; not_immutable_interval_proof",
      resource_floor: "unchanged_owned_controller_prerequisite; unobserved_here", execution: "production_entry_attempt; qualification_requires_separate_verifier" } }
  try {
    const paths = publicationPaths(environment), observed = await separatePublicationTargets(paths)
    publicationTarget = { ...paths, observed, published: [] }
    const options = testingOptions(testing), env = pinEnvironment(environment)
    if (OUTPUT_PATH_KEYS.some((key, index) => env[key] !== paths.outputs[index]) || INPUT_PATH_KEYS.some((key, index) => env[key] !== paths.inputs[index]) || env.NAPI_RS_NATIVE_LIBRARY_PATH !== paths.inputs[4]) fail("CONFIG_INVALID")
    if (options.modeled) {
      result.runtime_scope = "modeled_controls"
      result.evidence_scope.execution = "pure_injected_model; not_live_or_performance_evidence"
    }
    maximum = options.maximum
    const fixture = await privateJSON(env.MOUNT_RS_BACKING_CID_RECEIPT), engine = await privateJSON(env.MOUNT_RS_BACKING_ENGINE_CAPABILITY)
    const controller = await privateJSON(env.MOUNT_RS_OWNED_LAYOUT_CONTROLLER_RECEIPT), seal = await privateJSON(env.MOUNT_RS_OWNED_LAYOUT_BUILD_RECEIPT, BUILD_CAP)
    const handoff = validateOwnedLayoutHandoff({ fixtures: fixture.value, controller: controller.value, engine: engine.value, fixture_sha256: fixture.sha256, engine_sha256: engine.sha256 }, env)
    result.handoff = { ...handoff.public, controller_receipt_sha256: controller.sha256 }
    result.ownership = { ...result.ownership, tidb_owner: handoff.fixtures.tidb_owner, rustfs_owner: handoff.fixtures.rustfs_owner, generation: "1", scope_sha256: handoff.public.scope_sha256 }
    const identity = await preflightOwnedLayoutIdentity(env, seal.value, options.modeled ? { mode: "modeled", checkoutMetadata: options.checkoutMetadata } : {})
    result.identity.native_sha256 = identity.sha256; result.identity.profiling_enabled_before_load = true
    result.build = { status: "verified", ...seal.value, receipt_sha256: seal.sha256 }
    let binding
    try {
      if (options.modeled) binding = await options.loadBinding(identity)
      else {
        // A failed selected native load must stop before the public NAPI loader
        // can try its automatic platform/WASI alternatives.
        const selected = require(identity.selection)
        binding = require(bindingPath)
        if (selected !== binding || require.cache[identity.selection]?.exports !== selected || require.cache[bindingPath]?.exports !== selected) fail("NATIVE_JOIN_REJECTED")
      }
    }
    catch (error) { if (issued.has(error)) throw error; fail("NATIVE_LOAD_FAILED") }
    if (!object(binding)) fail("NATIVE_LOAD_FAILED")
    if (!options.modeled) {
      const proof = await observePilotBinding(identity)
      if (proof.native_used_identity !== "verified" || proof.native_sha256 !== identity.sha256 || require.cache[identity.selection]?.exports !== binding || require.cache[bindingPath]?.exports !== binding) fail("NATIVE_JOIN_REJECTED")
      result.identity.native_used_identity = "verified"
    }
    comparisonCapability = createOwnedLayoutComparison({ binding, environment: env, identity, fixtures: handoff.fixtures, engine: handoff.engine, scope: handoff.scope }, options.comparisonDependencies || {})
    result.comparison = await comparisonCapability.run()
    const evidence = comparisonCapability.takePrivateEvidence()
    captureJoin(result.comparison, evidence)
    // The private sidecar retains the exact one-use source snapshot. Public
    // output contains only its current successful byte receipt, never a path.
    const originals = { schema: ORIGINALS_SCHEMA, runtime_scope: result.runtime_scope,
      joins: { fixtures: fixture, controller, engine, build: seal,
        native: { sha256: identity.sha256, selection: identity.selection, kind: result.identity.kind,
          native_used_identity: result.identity.native_used_identity, profiling_enabled_before_load: result.identity.profiling_enabled_before_load },
        scope: { sha256: handoff.public.scope_sha256, value: handoff.scope } }, evidence }
    const originalsBytes = encode(originals, OUTPUT_CAP)
    await privateWrite(publicationTarget, 1, originalsBytes)
    result.originals = { schema: ORIGINALS_SCHEMA, status: "retained", sha256: sha(originalsBytes), bytes: originalsBytes.length, count: evidence.arms.length }
    result.publication.status = "complete"
  } catch (error) {
    if (!result.comparison && comparisonCapability) result.comparison = comparisonCapability.snapshot()
    result.failure_code = ownCode(error, "COMPARISON_FAILED"); result.publication.reason = result.failure_code
  }
  result.qualification.floor_qualified = result.comparison?.floor_qualified === true
  result.qualification.comparable = result.publication.status === "complete" && result.comparison?.comparable === true
  result.qualification.safe_to_continue = result.publication.status === "complete" && result.comparison?.safe_to_continue === true
  let published = result, bytes
  try { bytes = encode(result, maximum) }
  catch (error) {
    const reason = ownCode(error, "PUBLICATION_FAILED")
    published = { ...result, failure_code: result.failure_code || reason, publication: { status: "incomplete", reason }, comparison: incompleteComparison(result.comparison),
      qualification: { ...result.qualification, comparable: false, safe_to_continue: false } }
    bytes = encode(published, OUTCOME_RESERVE)
  }
  try { if (publicationTarget) await privateWrite(publicationTarget, 0, bytes) }
  catch (error) { const reason = ownCode(error, "PUBLICATION_FAILED"); published = { ...published, failure_code: published.failure_code || reason, publication: { status: "incomplete", reason },
    ...(publicationTarget.originalsInvalid ? { originals: { schema: ORIGINALS_SCHEMA, status: "unavailable", sha256: null, bytes: null, count: null } } : {}),
    qualification: { ...published.qualification, comparable: false, safe_to_continue: false } } }
  return frozen(published)
}
export async function main(argv = process.argv.slice(2), environment = process.env) {
  try {
    if (!Array.isArray(argv) || argv.length !== 1 || argv[0] !== "run") fail("CONFIG_INVALID")
    const record = await runOwnedLayoutEntry(environment)
    process.stdout.write(`OWNED_LAYOUT_ENTRY ${JSON.stringify({ publication_status: record.publication.status, comparison_status: record.comparison?.status || "not_started", floor_qualified: record.qualification.floor_qualified,
      hosted_qualification: false, failure_code: record.failure_code, native_uncertainty: record.comparison?.native_uncertainty ?? false })}\n`)
    if (record.failure_code) process.stderr.write(`OWNED_LAYOUT_ENTRY_FAILURE ${record.failure_code}\n`)
    return record.publication.status === "complete" && record.comparison?.complete === true && record.qualification.floor_qualified && record.qualification.comparable && record.qualification.safe_to_continue && record.identity.native_used_identity === "verified" ? 0 : 1
  } catch (error) { process.stderr.write(`OWNED_LAYOUT_ENTRY_FAILURE ${ownCode(error, "COMPARISON_FAILED")}\n`); return 1 }
}
// Finish module evaluation before asynchronous entry work/imports begin.
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main().then((code) => { process.exitCode = code })
