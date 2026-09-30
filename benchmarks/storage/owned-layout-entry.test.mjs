import assert from "node:assert/strict"
import test from "node:test"
import { createHash } from "node:crypto"
import { chmod, link, lstat, mkdir, mkdtemp, readFile, readdir, realpath, rm, symlink, truncate, writeFile } from "node:fs/promises"
import fsPromises from "node:fs/promises"
import { tmpdir } from "node:os"
import { basename, dirname, join, resolve } from "node:path"
import { fileURLToPath } from "node:url"
import Module, { createRequire, syncBuiltinESMExports } from "node:module"
import childProcess from "node:child_process"
import http from "node:http"
import https from "node:https"
import net from "node:net"
import tls from "node:tls"
import dgram from "node:dgram"

const directory = dirname(fileURLToPath(import.meta.url)), repo = resolve(directory, "../..")
const entryURL = new URL("./owned-layout-entry.mjs", import.meta.url), entryPath = fileURLToPath(entryURL)
const executeChild = childProcess.execFile.bind(childProcess)
const require = createRequire(import.meta.url), dispatches = { native: 0, network: 0, process: 0 }
const originals = [], originalNative = Module._extensions[".node"]
const replace = (object, key, value) => { originals.push([object, key, object[key]]); object[key] = value }
Module._extensions[".node"] = () => { dispatches.native++; throw new Error("entry test forbids actual native loading") }
const denyNetwork = () => { dispatches.network++; throw new Error("entry test forbids actual network dispatch") }
for (const [object, keys] of [[http, ["request", "get"]], [https, ["request", "get"]],
  [net, ["connect", "createConnection"]], [net.Socket.prototype, ["connect"]], [net.Server.prototype, ["listen"]],
  [tls, ["connect"]], [dgram, ["createSocket"]]]) for (const key of keys) replace(object, key, denyNetwork)
replace(globalThis, "fetch", denyNetwork)
for (const key of ["exec", "execSync", "execFile", "execFileSync", "spawn", "spawnSync", "fork"]) replace(childProcess, key, () => {
  dispatches.process++; throw new Error("entry test forbids actual subprocess dispatch")
})
syncBuiltinESMExports()

const { summarizeInterval } = await import("./backing-observer.mjs")
const { assessOwnedLayoutRunnerOutcome } = await import("./owned-layout-outcome.mjs")
const { projectOwnedLayoutPhaseMetrics } = await import("./owned-layout-metrics.mjs")
const {
  FOUNDATIONDB_DIAGNOSTIC_UNAVAILABLE, NATIVE_DIAGNOSTICS_SCHEMA, OBJECT_STORE_LOCAL_NAMES, RUSTFS_API_MEASUREMENT, RUSTFS_LOCAL_MEASUREMENT, STORAGE_BYTE_SEMANTICS,
  STORAGE_CALL_SEMANTICS, STORAGE_INSTRUMENTED_OPERATION_NAMES, STORAGE_OPERATION_FAMILIES,
  STORAGE_OPERATION_NAMES, STORAGE_ROW_SEMANTICS, TIDB_DIAGNOSTIC_COVERAGE, validateRawPhaseDiagnostics,
} = await import("./diagnostics.mjs")

// This fallback performs no work. Missing implementation fails the behavior
// assertions below, while actual CLI controls alone explicitly wait for a file.
let implemented = true, api
try { api = await import(entryURL.href) } catch (error) {
  if (error.code !== "ERR_MODULE_NOT_FOUND" || !error.message.includes("owned-layout-entry.mjs")) throw error
  implemented = false
  api = { SOURCE_PATHS: Object.freeze([]), validateOwnedLayoutHandoff: () => Object.freeze({}),
    preflightOwnedLayoutIdentity: async () => Object.freeze({}), runOwnedLayoutEntry: async () => Object.freeze({}), main: async () => 0 }
}

const SOURCE_PATHS = Object.freeze([
  "Cargo.toml", "Cargo.lock", "bindings/mount-rs-napi/Cargo.toml", "bindings/mount-rs-napi/index.js",
  "bindings/mount-rs-napi/src/lib.rs", "bindings/mount-rs-napi/src/namespace_presence.rs", "src/diagnostics/storage.rs", "src/diagnostics/profile.rs",
  "scripts/test-tidb.sh", "scripts/test-rustfs.sh", "scripts/rustfs-combo-runner.py", "scripts/rustfs-bounded-docker.py",
  ".github/workflows/remote-drives.yml", "benchmarks/storage/errors.mjs", "benchmarks/storage/stats.mjs", "benchmarks/storage/runner.mjs",
  "benchmarks/storage/providers.mjs", "benchmarks/storage/diagnostics.mjs", "benchmarks/storage/backing-engine-transport.mjs",
  "benchmarks/storage/backing-observer.mjs", "benchmarks/storage/compact-layout.mjs", "benchmarks/storage/namespace-presence.mjs",
  "benchmarks/storage/owned-backing-pilot.mjs", "benchmarks/storage/owned-layout-arm.mjs", "benchmarks/storage/owned-layout-outcome.mjs",
  "benchmarks/storage/owned-layout-metrics.mjs", "benchmarks/storage/owned-layout-comparison.mjs", "benchmarks/storage/owned-layout-entry.mjs",
])
const suffixes = ["CONFIG_INVALID", "HANDOFF_REJECTED", "ENDPOINT_BINDING_REJECTED", "ENGINE_BINDING_REJECTED", "SCOPE_REJECTED",
  "PRIVATE_FILE_REJECTED", "JSON_REJECTED", "BUILD_IDENTITY_REJECTED", "SOURCE_IDENTITY_REJECTED", "NATIVE_SELECTION_REJECTED",
  "NATIVE_LOAD_FAILED", "NATIVE_JOIN_REJECTED", "CAPTURE_JOIN_REJECTED", "OUTPUT_CAP", "PUBLICATION_FAILED", "COMPARISON_FAILED", "TESTING_REJECTED"]
const codes = new Set(suffixes.map((suffix) => `OWNED_LAYOUT_ENTRY_${suffix}`))
const secret = "PRIVATE_ENTRY_SECRET", checkout = "d".repeat(40), providerId = "mount-rs-split-tidb-rustfs"
const roles = ["pd-1", "pd-2", "pd-3", "tikv-1", "tikv-2", "tikv-3", "tidb", "rustfs-service"]
const observerCaps = Object.freeze({ maxContainers: 16, maxRequests: 257, maxBoundaries: 64, maxResponseBytes: 1_048_576,
  maxDeviceEntries: 128, maxNetworkInterfaces: 32, maxJournalBytes: 8_388_608, requestTimeoutMs: 2000, ownerTimeoutMs: 60_000 })
const observerRetention = "selected_projections_and_raw_version_inspect_or_first_stats_frame_sha256; consumed_trailing_bytes_counted_discarded; raw_bodies_discarded"
const sha = (bytes) => createHash("sha256").update(bytes).digest("hex")
function deeplyFrozen(value) {
  assert.equal(Object.isFrozen(value), true)
  for (const child of Object.values(value)) if (child && typeof child === "object") deeplyFrozen(child)
}
function fixedError(error) {
  assert.equal(codes.has(error?.code), true, `expected a fixed entry code; received ${error?.code}`)
  assert.doesNotMatch(error.message + JSON.stringify(error), new RegExp(`${secret}|mysql://|socket_path|node_modules|at .*\\.mjs`, "u"))
  return true
}
function fixtureReceipt() {
  return { schema: "mount-rs.owned-backing-cids.v1", tidb_owner: "model-tidb", rustfs_owner: "model-rustfs", generation: "1",
    entries: roles.map((role, index) => ({ role, cid: String(index + 1).repeat(64), labels: role === "rustfs-service"
      ? { "com.mount-rs.rustfs-test": "mount-rs-rustfs-test", "com.mount-rs.rustfs-test-run": "model-rustfs" }
      : { "mount-rs.tidb.run": "model-tidb" } })) }
}
async function sourceHashes() {
  return Object.fromEntries(await Promise.all(SOURCE_PATHS.map(async (path) => {
    let bytes
    try { bytes = await readFile(join(repo, path)) } catch (error) {
      if (implemented || path !== "benchmarks/storage/owned-layout-entry.mjs" || error.code !== "ENOENT") throw error
      bytes = Buffer.alloc(0)
    }
    return [path, sha(bytes)]
  })))
}
async function context(t) {
  const root = await realpath(await mkdtemp(join(tmpdir(), "mount-rs-owned-entry-")))
  await chmod(root, 0o700)
  t.after(() => rm(root, { recursive: true, force: true }))
  const paths = Object.fromEntries(["fixtures", "engine", "controller", "build"].map((key) => [key, join(root, `${key}.json`)]))
  paths.native = join(root, `${secret}.node`); paths.output = join(root, "artifact.json"); paths.originals = join(root, "originals.json")
  await writeFile(paths.native, "inert selected file; never a real native addon\n", { mode: 0o600 })
  const id = `layout-${"a".repeat(32)}`, fixtures = fixtureReceipt(), engine = { socket_path: "/var/run/docker.sock" }
  const fixtureBytes = Buffer.from(JSON.stringify(fixtures, null, 2) + "\n"), engineBytes = Buffer.from(JSON.stringify(engine, null, 2) + "\n")
  const controller = { schema: "mount-rs.owned-layout-controller.v1", generation: "1", tidb_owner: fixtures.tidb_owner, rustfs_owner: fixtures.rustfs_owner,
    fixture_sha256: sha(fixtureBytes), engine_capability_sha256: sha(engineBytes), tidb_url: `mysql://user:${secret}@127.0.0.1:4000/storage`,
    rustfs_endpoint: "http://127.0.0.1:19000/", rustfs_bucket: "model-bucket",
    scope: { id, owner: fixtures.tidb_owner, metadataPrefix: `mount-rs-owned-layout/${fixtures.tidb_owner}/${id}`,
      blockPrefix: `mount-rs-owned-layout/${fixtures.rustfs_owner}/${id}` } }
  const build = { schema: "mount-rs.owned-layout-native-build.v1", checkout_sha: checkout, source_clean: true, locked: true, release: true,
    no_js: true, build_exit_code: 0, native_sha256: sha(await readFile(paths.native)), source_sha256: await sourceHashes() }
  const environment = { NAPI_RS_NATIVE_LIBRARY_PATH: paths.native, MOUNT_RS_PROFILE_IO: "1", MOUNT_RS_TRACE_STORAGE: "0",
    DOCKER_HOST: "unix:///var/run/docker.sock", MOUNT_RS_BACKING_CID_RECEIPT: paths.fixtures, MOUNT_RS_BACKING_ENGINE_CAPABILITY: paths.engine,
    MOUNT_RS_OWNED_LAYOUT_CONTROLLER_RECEIPT: paths.controller, MOUNT_RS_OWNED_LAYOUT_BUILD_RECEIPT: paths.build, MOUNT_RS_OWNED_LAYOUT_OUTPUT: paths.output,
    MOUNT_RS_OWNED_LAYOUT_ORIGINALS_OUTPUT: paths.originals,
    MOUNT_RS_BACKING_TIDB_OWNER: fixtures.tidb_owner, MOUNT_RS_BACKING_RUSTFS_OWNER: fixtures.rustfs_owner, MOUNT_RS_BACKING_GENERATION: "1",
    MOUNT_RS_TIDB_URL: controller.tidb_url, MOUNT_RS_TIDB_DURABLE: "1", MOUNT_RS_BACKING_EXPECT_TIDB_URL: controller.tidb_url,
    MOUNT_RS_RUSTFS_ENDPOINT: controller.rustfs_endpoint, MOUNT_RS_RUSTFS_BUCKET: controller.rustfs_bucket, MOUNT_RS_RUSTFS_REGION: "us-east-1",
    MOUNT_RS_RUSTFS_ACCESS_KEY_ID: secret, MOUNT_RS_RUSTFS_SECRET_ACCESS_KEY: secret, MOUNT_RS_RUSTFS_DURABLE: "0",
    MOUNT_RS_BACKING_EXPECT_R2_ENDPOINT: controller.rustfs_endpoint, MOUNT_RS_BACKING_EXPECT_R2_BUCKET: controller.rustfs_bucket }
  const writeInputs = async () => {
    await writeFile(paths.fixtures, fixtureBytes, { mode: 0o600 }); await writeFile(paths.engine, engineBytes, { mode: 0o600 })
    await writeFile(paths.controller, JSON.stringify(controller, null, 2) + "\n", { mode: 0o600 })
    await writeFile(paths.build, JSON.stringify(build, null, 2) + "\n", { mode: 0o600 })
  }
  await writeInputs()
  return { root, paths, fixtures, engine, controller, build, environment, fixtureBytes, engineBytes, writeInputs,
    handoff: () => ({ fixtures, controller, engine, fixture_sha256: sha(fixtureBytes), engine_sha256: sha(engineBytes) }),
    metadata: async () => ({ checkout_sha: checkout, source_clean: true }) }
}

test.after(() => {
  try {
    assert.deepEqual(dispatches, { native: 0, network: 0, process: 0 }, "parent controls cannot dispatch actual native/network/process work")
    assert.equal(Object.keys(require.cache).some((path) => path.endsWith(".node")), false)
  } finally {
    Module._extensions[".node"] = originalNative
    for (const [object, key, original] of originals.reverse()) object[key] = original
    syncBuiltinESMExports()
  }
})

// Local copies of reviewed pure fixtures; no test module is imported.
const presence = () => ({
  schema: "mount-rs.split-namespace-presence.v1", namespace_absent: true,
  metadata: { provider: "tidb", key_scope: "exact_input_utf8_bytes", schema_setup: "shared_ddl_and_session_configuration",
    row_presence: { metadata: false, inodes: false, compact_guards: false, compact_members: false, compact_dentries: false, block_authority: false, blocks: false }, observed_at_ns: "1" },
  blobs: { provider: "rustfs", scope: "canonical_ascii_prefix_descendants", observation: "signed_list_page_api", requested_max_keys: 1, prefix_absent: true, observed_at_ns: "2" },
  pool_shutdown: { confirmed: true, elapsed_ns: "1" }, clock: "elapsed_monotonic_since_native_preflight_start", consistency: "separate_observations_no_reservation",
  limits: { probe_deadline_ms: 30000, pool_shutdown_deadline_ms: 15000, list_request_keys: 1,
    response_byte_cap: "unavailable", server_truncation_flag: "unavailable", deadline_semantics: "cooperative_await_with_elapsed_recheck" },
})
function fakeBinding(events, behavior = {}) {
  const stores = new Map(), creates = []
  const binding = {
    async inspectSplitNamespacePresence(metadata, blocks) {
      assert.equal(this, binding)
      events.push({ event: "preflight", key: metadata.key })
      deeplyFrozen(metadata); deeplyFrozen(blocks)
      if (behavior.preflightError) throw Object.assign(new Error(secret), { code: secret, path: secret })
      return JSON.stringify(presence())
    },
    async createChunkedDriver(options) {
      assert.equal(this, binding)
      deeplyFrozen(options)
      const index = creates.length
      creates.push(options)
      events.push({ event: "native-create", key: options.metadata.key, index })
      if (!stores.has(options.metadata.key)) stores.set(options.metadata.key, { files: new Map(), generation: 1n, backingId: (index + 1).toString(16).padStart(32, "0") })
      const store = stores.get(options.metadata.key)
      const filesystem = {
        closed: false,
        async shutdown() { assert.equal(this, filesystem); events.push({ event: "shutdown", key: options.metadata.key, index }); filesystem.closed = true },
        async inspectCompactLayout() {
          assert.equal(this, filesystem); assert.equal(filesystem.closed, false)
          return options.compactInodeUpdates ? { schema: "mount-rs.compact-layout-receipt.v1", marker: "MRC5", backingId: store.backingId,
            structuralGeneration: String(store.generation), blockAuthorityVerified: true } : null
        },
        async readdirBounded(path, limit) {
          assert.equal(this, filesystem); assert.equal(filesystem.closed, false); assert.equal(path, "/"); assert.equal(limit, 1)
          return [...store.files.keys()].map((name) => ({ name })).slice(0, 1)
        },
        async open(path, flags) {
          assert.equal(this, filesystem); assert.equal(filesystem.closed, false)
          if (flags === "wx") { assert.equal(store.files.has(path), false); store.files.set(path, new Uint8Array()); store.generation++ }
          else { assert.equal(flags, "r"); assert.equal(store.files.has(path), true) }
          const handle = {
            async write(bytes, offset, length, position) {
              assert.equal(this, handle); assert.equal(offset, 0); assert.equal(length, 4096); assert.equal(position, 0)
              store.files.set(path, Uint8Array.from(bytes)); return { bytesWritten: 4096, buffer: bytes }
            },
            async sync() { assert.equal(this, handle) },
            async read(bytes, offset, length, position) { assert.equal(this, handle); assert.equal(position, 4096); return { bytesRead: 0, buffer: bytes } },
            async close() { assert.equal(this, handle) },
          }
          return handle
        },
        async readFile(path) {
          assert.equal(this, filesystem); assert.equal(filesystem.closed, false)
          const bytes = Uint8Array.from(store.files.get(path) || [])
          if (behavior.corruptCanary) bytes[0] ^= 1
          return bytes
        },
        async unlink(path) { assert.equal(this, filesystem); assert.equal(filesystem.closed, false); assert.equal(store.files.delete(path), true); store.generation++ },
      }
      return filesystem
    },
  }
  return { binding, creates, stores }
}

const zeros = (names) => Object.fromEntries(names.map((name) => [name, "0"]))
const rawNames = ["put_opts.block_create", "get.block_read", "body_read.block_read", "get.conflict_verify", "body_read.conflict_verify", "get.migration", "body_read.migration", "head.direct_delete", "delete.direct", "delete.reconcile"]
const claims = ["leader_claims", "leader_success", "leader_error", "leader_cancelled", "follower_claims", "follower_success", "follower_error", "follower_cancelled"]

function nativePhase(elapsed) {
  const storage = STORAGE_OPERATION_NAMES.map((name, index) => ({
    name, ...zeros(["calls", "success", "error", "cancelled", "bytes", "returned_rows", "returned_row_observations", "elapsed_ns", "in_flight_start", "in_flight_end"]),
    ...(index === 0 ? { calls: "400", success: "400", elapsed_ns: "400000" } : {}),
    latency_log2_us: [index === 0 ? "400" : "0", ...Array(31).fill("0")],
  }))
  const instance = {
    id: "1", opened_during_phase: false,
    ...zeros(["puts", "gets", "deletes", "reconciles", "successes", "errors", "duration_ms_total", "bytes_read", "bytes_written", "conditional_conflicts", "id_collision_exhausted", "retry_exhausted", "cache_hits"]),
    raw_api: {
      schema: "mount-rs.object-store-api.v1", scope: "one_object_store_block_store_instance",
      saturated_start: false, saturated_end: false, in_flight_start: "0", in_flight_end: "0",
      pending_claims_start: "0", pending_claims_end: "0", claims: zeros(claims),
      entries: rawNames.map((name) => ({ name,
        ...zeros(["calls", "success", "error", "cancelled", "elapsed_ns", "attempted_bytes", "confirmed_bytes", "returned_bytes", "latency_max_ns_start", "latency_max_ns_end"]),
        exact_phase_max_ns: "unavailable", latency_log2_us: Array(32).fill("0"),
      })),
    },
  }
  return {
    name: "workload-4096bytes", quiescent: true, elapsed_ms: elapsed + 1,
    benchmark_measured_elapsed_ms: elapsed, observer_snapshot_ms: 1,
    native: {
      schema_version: NATIVE_DIAGNOSTICS_SCHEMA, complete: true, issues: [],
      storage: { in_flight_start: "0", in_flight_end: "0", entries: storage },
      measurement: {
        rustfs: "live_store_logical_calls_and_cache_hits; not_http_attempts",
        rustfs_api: structuredClone(RUSTFS_API_MEASUREMENT),
        storage_calls: STORAGE_CALL_SEMANTICS, storage_bytes: STORAGE_BYTE_SEMANTICS,
        storage_rows: STORAGE_ROW_SEMANTICS, storage_operations: [...STORAGE_OPERATION_NAMES],
        storage_families: structuredClone(STORAGE_OPERATION_FAMILIES),
        storage_instrumented_operations: [...STORAGE_INSTRUMENTED_OPERATION_NAMES],
        tidb_coverage: structuredClone(TIDB_DIAGNOSTIC_COVERAGE),
        foundationdb_coverage: structuredClone(FOUNDATIONDB_DIAGNOSTIC_UNAVAILABLE),
        latency_histogram: { unit: "microseconds", intervals: Array.from({ length: 32 }, (_, bucket) => bucket === 0
          ? { lower_inclusive_us: "0", upper_exclusive_us: "1" }
          : bucket === 31 ? { lower_inclusive_us: String(2 ** 30), upper_exclusive_us: null, terminal_overflow: true }
            : { lower_inclusive_us: String(2 ** (bucket - 1)), upper_exclusive_us: String(2 ** bucket) }) },
      },
      rustfs: { scope: "process_live_instances", complete: true, internal_successful_retries: "unavailable",
        missing_instance_ids: [], instance_ids_start: ["1"], instance_ids_end: ["1"], instances: [instance] },
    },
  }
}

function modeledBacking(elapsed, status) {
  const roles = ["pd-1", "pd-2", "pd-3", "tikv-1", "tikv-2", "tikv-3", "tidb", "rustfs-service"]
  const allowlist = roles.map((role, index) => ({ role, cid: String(index + 1).repeat(64),
    labels: role === "rustfs-service" ? { "com.mount-rs.rustfs-test": "mount-rs-rustfs-test", "com.mount-rs.rustfs-test-run": "model-rustfs" } : { "mount-rs.tidb.run": "model-tidb" } }))
  const boundary = (id, read) => ({ type: "boundary", id, kind: "phase", complete: true, issues: [],
    samples: allowlist.map((entry) => ({ cid: entry.cid,
      identity: { ...entry, image: `sha256:${"a".repeat(64)}`, running: true, started_at: "2026-09-27T00:00:00Z", restart_count: "0", limits: { Memory: "0", MemorySwap: "0", NanoCpus: "0", CpuQuota: "0", CpuPeriod: "100000", CpuShares: "0", PidsLimit: "0", CpusetCpus: "" }, configured_limit_semantics: "field_specific_zero_unset_and_minus_one_unlimited; not_observed_capacity" },
      inspect_body_sha256: "b".repeat(64), stats_body_sha256: "c".repeat(64), stats_frame_bytes: 512, stats_received_bytes: 512,
      inspect_window: { dispatch_ms: 0, headers_ms: 0.25, first_frame_ms: null, retired_ms: null, response_ms: 1,
        dispatch_utc: "2026-09-27T00:00:00Z", response_utc: "2026-09-27T00:00:01Z" },
      stats_window: { dispatch_ms: 1, headers_ms: 1.25, first_frame_ms: 1.5, retired_ms: 2, response_ms: 2,
        dispatch_utc: "2026-09-27T00:00:01Z", response_utc: "2026-09-27T00:00:02Z" },
      stats: { cid: entry.cid, read: new Date(Number(read) / 1_000_000).toISOString(), read_ns: read,
        cpu_usage_ns: read, cpu_user_ns: "0", cpu_kernel_ns: "0", system_cpu_usage_ns: read, online_cpus: "1",
        throttling: { periods: "0", throttled_periods: "0", throttled_ns: "0" },
        memory: { usage_bytes: "0", limit_bytes: "0", semantics: "api_gauges; not_cli_cache_adjusted" }, block_bytes: { "1:1:Read": read },
        block_operations: { "1:1:Read": read }, network_rx_bytes: { eth0: read }, network_tx_bytes: { eth0: read } },
    })),
  })
  const begin = boundary("workload-4096bytes:begin", "1000000000")
  const end = boundary("workload-4096bytes:end", "3000000000")
  const interval = { type: "interval", ...summarizeInterval(allowlist, begin, end, { workload_elapsed_ms: elapsed }),
    native_quiescent: true, owned_operations_settled: true, native_evidence_state: "complete" }
  const terminal = () => ({ status, native_quiescent: true, owned_operations_settled: true,
    native_profiling_enabled: true, workload_native_evidence_complete: true, operation_deadline_failed: false,
    cleanup_complete: true, prior_native_uncertainty: false, safe_to_continue_pair: true,
    quiescence_scope: "runner_workload_native_diagnostics_and_owned_operation_settlement" })
  const journal = [{ type: "version", api_version: "1.51", mode: "stream=true;first-frame", platform: "linux", body_sha256: "b".repeat(64) }, begin, end, interval]
  return { schema: "mount-rs.runner-backing-observer.v1", complete: true, issues: [], terminal: terminal(),
    events: ["beginPhase", "endPhase", "finalize"].map((hook) => ({ hook, status: "ok",
      ...(hook === "finalize" ? {} : { phase: "workload-4096bytes" }),
      ...(hook === "endPhase" ? { owned_operations_settled: true, native_quiescent: true, native_evidence_state: "complete", pending_count: 0 } : {}) })),
    backing_evidence: { schema: "mount-rs.backing-observer.v1", complete: true, issues: [], dropped_entries: 0, api_version: "1.51",
      caps: { ...observerCaps }, journal_bytes: journal.reduce((sum, entry) => sum + Buffer.byteLength(JSON.stringify(entry)) + 1, 0),
      terminal: terminal(), allowlist,
      cost: { requests: 33, response_bytes: 24_704, wall_ms: 33, cpu_user_us: 0, cpu_system_us: 0, peak_in_flight: 8,
        headers_received: 33, first_frames_received: 16, streamed_iterators_retired: 16, consumed_trailing_bytes: 0,
        in_flight: 0, owner_lifetime_ms: elapsed + 4,
        response_bytes_scope: "adapter_yielded_bytes_including_consumed_same_chunk_tail; rejected_overflow_chunks_unavailable; not_wire_bytes",
        wall_scope: "inclusive_request_windows; concurrent_windows_overlap; not_exclusive_hook_wall",
        cpu_scope: "inclusive_process_cpu_during_observer_calls; overlapping_work_not_isolated",
        pending_stages: Object.fromEntries(["headers", "body", "retirement"].map((stage) => [stage, { count: 0, age_ms: 0 }])) },
      journal, daemon_overhead: "unisolated", retention: observerRetention,
    },
  }
}

function modeledRunner({ floor = true, layout = "legacy" } = {}) {
  const iops = floor ? 1001 : 394, elapsed = 1200000 / iops, status = floor ? "ok" : "failed"
  const rawSamples = Array.from({ length: 400 }, (_, index) => ({ provider: providerId,
    fileSizeBytes: 4096, iteration: index + 1, concurrencySlot: index % 64, path: `/model/${index}`,
    status: "ok", success: true, writeSucceeded: true, readSucceeded: true, deleteSucceeded: true,
    payloadVerified: true, bytesExpected: 4096, bytesReturned: 4096, cleanupSucceeded: true,
    cleanupFailure: false, timedOut: false, cleanupDeferred: false,
    timeoutCount: 0, timeoutOperations: [], lateOperations: [], errors: [],
  }))
  const result = { provider: providerId, status, layout, workload: "lifecycle", sizeMiB: 1,
    fileSizeBytes: 4096, writePayloadBytes: 4096, iterationsRequested: 400, concurrency: 64, chunkSizeBytes: 65536,
    summary: { elapsedMs: elapsed, iops, iopsTarget: 1000, iopsTargetMet: floor,
      successfulIterations: 400, failedIterations: 0, successfulOperations: 1200, attemptedOperations: 1200,
      operationsPerLifecycle: 3, timeoutCount: 0, cleanupFailureCount: 0,
      operationSuccess: { write: 400, read: 400, delete: 400, verifiedReads: 400 } },
    rawSamples,
    ...(floor ? {} : { failures: [{ operation: "iops-target", error: { name: "IopsTargetError", code: "IOPS_TARGET_NOT_MET", actual: iops, target: 1000, message: "PRIVATE_FLOOR_MESSAGE" } }] }),
  }
  return {
    schemaVersion: "mount-rs.storage-benchmark.v1", status,
    config: { layout, workload: "lifecycle", sizesMiB: [1], payloadSizesBytes: [4096], payloadBytes: 4096,
      iterations: 400, concurrency: 64, chunkSizeBytes: 65536, minIops: 1000,
      requireConfigured: true, storageDiagnosticsEnabled: true },
    configurationFailures: [], counts: { providersRequested: 1, providersFailed: floor ? 0 : 1,
      providersSkipped: 0, configurationFailures: 0, sizeResults: 1, sizeResultsFailed: floor ? 0 : 1, sizeResultsSkipped: 0 },
    providers: [{ provider: providerId, status, sizes: [result], missingConfiguration: [],
      ...(layout === "compact" ? { layoutSelection: { requested: "compact", selected: "compact",
        selectionEvidence: "createChunkedDriver-constructor-accepted",
        persistedMarkerEvidence: "metadata-MRC5-and-matching-block-authority-observed",
        persistedReceipt: { schema: "mount-rs.compact-layout-receipt.v1", marker: "MRC5",
          backingId: "a".repeat(32), structuralGeneration: "9", blockAuthorityVerified: true } } } : {}),
      cleanup: { pathsAttempted: 0, remainingPaths: 0, failures: [], pendingOperations: [], resource: { status: "ok" } },
      backingResourceCoverage: { create: "not_selected", "workload-4096bytes": "captured", cleanup: "not_selected", shutdown: "not_selected" },
      storageDiagnostics: { enabled: true, phases: [nativePhase(elapsed)] }, backingObserver: modeledBacking(elapsed, status) }],
    results: [result],
  }
}

function addMeasuredNativeCounters(result) {
  const phase = result.providers[0].storageDiagnostics.phases[0], native = phase.native, instance = native.rustfs.instances[0]
  Object.assign(native.storage.entries.find(({ name }) => name === "tidb.sql.inode_read"), {
    calls: "600", success: "600", returned_rows: "400", returned_row_observations: "600", elapsed_ns: "9007199254740993",
    latency_log2_us: ["600", ...Array(31).fill("0")],
  })
  Object.assign(instance.raw_api.entries[0], { calls: "4", success: "4", elapsed_ns: "4000", attempted_bytes: "16384", confirmed_bytes: "16384",
    latency_max_ns_end: "1000", latency_log2_us: ["4", ...Array(31).fill("0")] })
  native.measurement.rustfs_local = structuredClone(RUSTFS_LOCAL_MEASUREMENT)
  const entries = OBJECT_STORE_LOCAL_NAMES.map((name) => ({ name,
    ...zeros(["calls", "success", "error", "cancelled", "elapsed_ns", "input_bytes", "output_bytes", "latency_max_ns_start", "latency_max_ns_end"]),
    exact_phase_max_ns: "unavailable", latency_log2_us: Array(32).fill("0"),
  }))
  Object.assign(entries[0], { calls: "4", success: "4", elapsed_ns: "4000", input_bytes: "16384", output_bytes: "128",
    latency_max_ns_end: "1000", latency_log2_us: ["4", ...Array(31).fill("0")] })
  instance.local_work = { status: "observed", complete: true, schema: "mount-rs.object-store-local.v1", scope: "one_object_store_block_store_instance",
    saturated_start: false, saturated_end: false, in_flight_start: "0", in_flight_end: "0", entries }
}

function comparisonModel(fixture, behavior = {}) {
  const events = [], native = fakeBinding(events, behavior), runners = [], observers = []
  const proof = () => ({ native_used_identity: "verified", kind: "selected_native_file", native_sha256: fixture.build.native_sha256 })
  const testing = { mode: "modeled", checkoutMetadata: fixture.metadata,
    loadBinding: async () => { events.push({ event: "load" }); return native.binding },
    comparisonDependencies: {
      verifyBinding: async (identity, binding) => {
        assert.equal(binding, native.binding); assert.equal(identity.sha256, fixture.build.native_sha256)
        events.push({ event: "verify" }); return proof()
      },
      providerById: async () => new Map([[providerId, { id: providerId, create() { throw new Error("original constructor cannot run") } }]]),
      createTransport: (input) => { assert.equal(input.socketPath, "/var/run/docker.sock"); assert.equal(input.allowlistedCids.length, 8); return {} },
      createObserver: (input) => { assert.equal(input.allowlist.length, 8); const observer = {}; observers.push(observer); return observer },
      runBenchmark: async (options, environment, providers, runtime) => {
        const index = runners.length
        assert.equal(environment.MOUNT_RS_RUSTFS_SECRET_ACCESS_KEY, secret)
        assert.equal(runtime.backingPilotIdentity.sha256, fixture.build.native_sha256)
        assert.equal(runtime.backingObserver, observers[index])
        const opened = await providers.get(providerId).create({ layout: options.layout, chunkSizeBytes: options.chunkSizeBytes })
        const originalLayout = await opened.filesystem.inspectCompactLayout()
        await opened.cleanup()
        const result = modeledRunner({ layout: options.layout, floor: behavior.floor ? behavior.floor(index) : true })
        if (originalLayout) result.providers[0].layoutSelection.persistedReceipt = originalLayout
        result.providers[0].backingPilotIdentity = proof()
        result.environment = { arbitrary_private_config: secret, endpoint: "https://foreign.invalid/" }
        if (behavior.mutate) behavior.mutate(result, index)
        runners.push(result)
        return result
      },
    } }
  return { testing, events, native, runners, observers }
}
async function readArtifact(fixture) { return JSON.parse(await readFile(fixture.paths.output, "utf8")) }
async function readOriginals(fixture, record) {
  const bytes = await readFile(fixture.paths.originals), value = JSON.parse(bytes)
  assert.deepEqual(record.originals, { schema: "mount-rs.owned-layout-originals.v1", status: "retained", sha256: sha(bytes), bytes: bytes.length, count: value.evidence.arms.length })
  assert.equal(value.schema, "mount-rs.owned-layout-originals.v1")
  assert.equal(value.runtime_scope, record.runtime_scope)
  assert.equal((await lstat(fixture.paths.originals)).mode & 0o777, 0o600)
  assert.equal((await lstat(fixture.root)).mode & 0o777, 0o700)
  return { bytes, value }
}
function unavailableOriginals(record) {
  assert.deepEqual(record.originals, { schema: "mount-rs.owned-layout-originals.v1", status: "unavailable", sha256: null, bytes: null, count: null })
}
function afterLastRunner(model, action) {
  const run = model.testing.comparisonDependencies.runBenchmark
  model.testing.comparisonDependencies.runBenchmark = async (...args) => {
    const value = await run(...args)
    if (model.runners.length === 4) await action()
    return value
  }
}
function assertIncomplete(record) {
  assert.equal(record.schema, "mount-rs.owned-layout-entry.v1")
  assert.equal(record.publication.status, "incomplete")
  assert.equal(record.qualification.hosted, false)
  assert.equal(codes.has(record.failure_code), true, "a rejected entry must retain a fixed failure code")
}
function assertModeled(record) {
  assert.equal(record.schema, "mount-rs.owned-layout-entry.v1")
  assert.equal(record.runtime_scope, "modeled_controls")
  assert.equal(record.qualification.hosted, false)
  assert.equal(record.qualification.reason, "owner_teardown_and_independent_verification_required")
  assert.equal(record.ownership.final_teardown.status, "unverified")
  assert.equal(record.ownership.namespace_purge, "unverified")
  assert.equal(record.identity.kind, "selected_native_file")
  assert.equal(record.identity.native_used_identity, "unverified", "injected proof cannot qualify a modeled runtime")
  assert.equal(record.identity.profiling_enabled_before_load, true)
}
function partialDaemonCounters(result) {
  const evidence = result.providers[0].backingObserver.backing_evidence
  const boundaries = evidence.journal.filter(({ type }) => type === "boundary")
  for (const boundary of boundaries) {
    for (const sample of boundary.samples) sample.stats.block_operations = null
    boundary.samples[0].stats.block_bytes = null
    boundary.samples[0].stats.network_rx_bytes = { a: null, b: boundary.samples[0].stats.read_ns }
  }
  const intervalIndex = evidence.journal.findIndex(({ type }) => type === "interval")
  evidence.journal[intervalIndex] = { type: "interval", ...summarizeInterval(evidence.allowlist, boundaries[0], boundaries[1],
    { workload_elapsed_ms: result.results[0].summary.elapsedMs }), native_quiescent: true, owned_operations_settled: true, native_evidence_state: "complete" }
  evidence.journal_bytes = evidence.journal.reduce((sum, entry) => sum + Buffer.byteLength(JSON.stringify(entry)) + 1, 0)
}

test("SOURCE_PATHS is the exact frozen ordered 28-path runtime and controller seam", () => {
  assert.deepEqual(api.SOURCE_PATHS, SOURCE_PATHS)
  assert.equal(Object.isFrozen(api.SOURCE_PATHS), true)
  assert.equal(new Set(api.SOURCE_PATHS).size, 28)
  assert.equal(api.SOURCE_PATHS.includes("benchmarks/storage/owned-layout-entry.test.mjs"), false)
})

test("private originals retain exact captured samples, independent projections and actual immutable joins", async (t) => {
  const fixture = await context(t), model = comparisonModel(fixture, { mutate: addMeasuredNativeCounters })
  const record = await api.runOwnedLayoutEntry(fixture.environment, model.testing)
  assert.equal(record.publication.status, "complete")
  const { bytes, value } = await readOriginals(fixture, record)
  assert.deepEqual(Object.keys(value).sort(), ["evidence", "joins", "runtime_scope", "schema"])
  assert.equal(value.evidence.schema, "mount-rs.owned-layout-private-evidence.v1")
  assert.equal(value.evidence.complete, true); assert.equal(value.evidence.arms.length, 4)
  for (const [index, role] of ["A1", "B1", "B2", "A2"].entries()) {
    const retained = value.evidence.arms[index], arm = record.comparison.arms[index]
    assert.equal(retained.role, role)
    assert.deepEqual(retained.benchmark, model.runners[index])
    assert.equal(retained.benchmark.results[0].rawSamples.length, 400)
    assert.deepEqual(retained.cohort, { id: `${fixture.controller.scope.id}/${role}`, layout: ["legacy", "compact", "compact", "legacy"][index],
      metadataKey: `${fixture.controller.scope.metadataPrefix}/${role}`, blockPrefix: `${fixture.controller.scope.blockPrefix}/${role}`, owner: fixture.controller.scope.owner })
    assert.deepEqual(assessOwnedLayoutRunnerOutcome(retained.benchmark), arm.outcome)
    assert.deepEqual(projectOwnedLayoutPhaseMetrics(retained.benchmark.providers[0].storageDiagnostics.phases[0]), arm.native_metrics)
  }
  for (const [key, path] of [["fixtures", fixture.paths.fixtures], ["controller", fixture.paths.controller], ["engine", fixture.paths.engine], ["build", fixture.paths.build]]) {
    const input = await readFile(path)
    assert.deepEqual(value.joins[key], { sha256: sha(input), value: JSON.parse(input) })
  }
  assert.deepEqual(value.joins.scope, { sha256: record.ownership.scope_sha256, value: fixture.controller.scope })
  assert.deepEqual(value.joins.native, { sha256: fixture.build.native_sha256, selection: fixture.paths.native, kind: "selected_native_file",
    native_used_identity: "unverified", profiling_enabled_before_load: true })
  assert.match(bytes.toString("utf8"), new RegExp(`${secret}|arbitrary_private_config|mysql://|metadataKey`, "u"))
  const publicBytes = await readFile(fixture.paths.output)
  assert.deepEqual(JSON.parse(publicBytes), record)
  assert.doesNotMatch(publicBytes.toString("utf8"), new RegExp(`${secret}|arbitrary_private_config|mysql://|metadataKey|blockPrefix|originals.json|foreign.invalid`, "u"))
  model.runners[0].results[0].rawSamples[0].path = secret + "changed"
  assert.equal((await readFile(fixture.paths.originals)).equals(bytes), true)
  deeplyFrozen(record)
})

test("below-floor originals preserve all failed statuses and private full original failures", async (t) => {
  const fixture = await context(t), model = comparisonModel(fixture, { floor: () => false })
  const record = await api.runOwnedLayoutEntry(fixture.environment, model.testing), retained = await readOriginals(fixture, record)
  assert.equal(record.publication.status, "complete"); assert.equal(record.qualification.floor_qualified, false)
  assert.equal(retained.value.evidence.arms.length, 4)
  for (const [index, arm] of retained.value.evidence.arms.entries()) {
    assert.equal(arm.benchmark.status, "failed")
    assert.equal(arm.benchmark.results[0].failures[0].error.message, "PRIVATE_FLOOR_MESSAGE")
    assert.equal(record.comparison.arms[index].outcome.status, "failed")
    assert.equal(record.comparison.arms[index].outcome.runner_safe_to_continue, true)
  }
  assert.doesNotMatch(await readFile(fixture.paths.output, "utf8"), /PRIVATE_FLOOR_MESSAGE/u)
})

test("stopped uncertain original is retained without changing its floor or uncertainty", async (t) => {
  const fixture = await context(t), model = comparisonModel(fixture, { mutate: (result) => {
    for (const terminal of [result.providers[0].backingObserver.terminal, result.providers[0].backingObserver.backing_evidence.terminal]) {
      terminal.prior_native_uncertainty = true; terminal.safe_to_continue_pair = false
    }
  } })
  const record = await api.runOwnedLayoutEntry(fixture.environment, model.testing), retained = await readOriginals(fixture, record)
  assert.equal(record.publication.status, "complete"); assert.equal(record.comparison.status, "stopped")
  assert.equal(retained.value.evidence.complete, false); assert.equal(retained.value.evidence.arms.length, 1)
  assert.equal(record.comparison.native_uncertainty, true); assert.equal(record.comparison.arms[0].outcome.floor_qualified, true)
  assert.equal(retained.value.evidence.arms[0].benchmark.providers[0].backingObserver.terminal.prior_native_uncertainty, true)
})

test("a prepare failure retains an actual empty capture without inventing runner originals", async (t) => {
  const fixture = await context(t), model = comparisonModel(fixture, { preflightError: true })
  const record = await api.runOwnedLayoutEntry(fixture.environment, model.testing), retained = await readOriginals(fixture, record)
  assert.equal(record.publication.status, "complete"); assert.equal(record.comparison.status, "stopped")
  assert.equal(retained.value.evidence.complete, false); assert.deepEqual(retained.value.evidence.arms, [])
  assert.equal(model.runners.length, 0)
})

test("honest partial daemon metrics remain null in exact originals and retain original interval completeness", async (t) => {
  const fixture = await context(t), model = comparisonModel(fixture, { mutate: partialDaemonCounters })
  const record = await api.runOwnedLayoutEntry(fixture.environment, model.testing), retained = await readOriginals(fixture, record)
  assert.equal(record.publication.status, "complete"); assert.equal(record.comparison.comparable, true)
  for (const arm of retained.value.evidence.arms) {
    const journal = arm.benchmark.providers[0].backingObserver.backing_evidence.journal
    assert.equal(journal.find(({ type }) => type === "interval").complete, false)
    for (const boundary of journal.filter(({ type }) => type === "boundary")) assert.equal(boundary.samples[0].stats.block_operations, null)
  }
})

test("private originals use their own full32MiB cap and never link a stale file after cap failure", async (t) => {
  const fixture = await context(t), stale = Buffer.from("prior private originals\n")
  await writeFile(fixture.paths.originals, stale, { mode: 0o600 })
  const model = comparisonModel(fixture, { floor: () => false, mutate: (result) => { result.private_note = "p".repeat(9 * 1024 * 1024) } })
  const record = await api.runOwnedLayoutEntry(fixture.environment, model.testing)
  assertIncomplete(record); assert.equal(record.failure_code, "OWNED_LAYOUT_ENTRY_OUTPUT_CAP")
  unavailableOriginals(record); assert.equal((await readFile(fixture.paths.originals)).equals(stale), true)
  assert.equal(record.comparison.status, "complete"); assert.equal(record.comparison.floor_qualified, false)
  for (const arm of record.comparison.arms) assert.equal(arm.outcome.status, "failed")
  assert.equal(model.runners.length, 4); assert.deepEqual(await readArtifact(fixture), record)
})

test("sidecar publication failure preserves below-floor originals in memory without linking stale bytes", async (t) => {
  const fixture = await context(t), stale = Buffer.from("prior private originals\n")
  await writeFile(fixture.paths.originals, stale, { mode: 0o600 })
  const model = comparisonModel(fixture, { floor: () => false })
  afterLastRunner(model, () => chmod(fixture.paths.originals, 0o644))
  const record = await api.runOwnedLayoutEntry(fixture.environment, model.testing)
  assertIncomplete(record); assert.equal(record.failure_code, "OWNED_LAYOUT_ENTRY_PUBLICATION_FAILED")
  unavailableOriginals(record); assert.equal((await readFile(fixture.paths.originals)).equals(stale), true)
  assert.equal(record.comparison.status, "complete"); assert.equal(record.qualification.floor_qualified, false)
  for (const arm of record.comparison.arms) assert.equal(arm.outcome.status, "failed")
  assert.deepEqual(await readArtifact(fixture), record)
})

test("failed private publication retains pending native uncertainty and supported original floor", async (t) => {
  const fixture = await context(t), model = comparisonModel(fixture, { mutate: (result) => {
    for (const terminal of [result.providers[0].backingObserver.terminal, result.providers[0].backingObserver.backing_evidence.terminal]) {
      terminal.prior_native_uncertainty = true; terminal.safe_to_continue_pair = false
    }
  } })
  const run = model.testing.comparisonDependencies.runBenchmark
  model.testing.comparisonDependencies.runBenchmark = async (...args) => { const value = await run(...args); await mkdir(fixture.paths.originals); return value }
  const record = await api.runOwnedLayoutEntry(fixture.environment, model.testing)
  assertIncomplete(record); unavailableOriginals(record)
  assert.equal(record.comparison.status, "stopped"); assert.equal(record.comparison.native_uncertainty, true)
  assert.equal(record.comparison.arms[0].outcome.floor_qualified, true); assert.equal(model.runners.length, 1)
})

test("projection failure after sidecar retention keeps current private receipt and refuses complete publication", async (t) => {
  const fixture = await context(t), model = comparisonModel(fixture, { floor: () => false })
  afterLastRunner(model, () => mkdir(fixture.paths.output))
  const record = await api.runOwnedLayoutEntry(fixture.environment, model.testing)
  assertIncomplete(record); assert.equal(record.failure_code, "OWNED_LAYOUT_ENTRY_PUBLICATION_FAILED")
  await readOriginals(fixture, record)
  assert.equal(record.comparison.floor_qualified, false); assert.equal(record.qualification.safe_to_continue, false)
})

test("new mutual output inode alias after capture disables both writers and preserves prior bytes", async (t) => {
  const fixture = await context(t), model = comparisonModel(fixture, { floor: () => false }), stale = Buffer.from("mutual prior bytes\n")
  afterLastRunner(model, async () => { await writeFile(fixture.paths.originals, stale, { mode: 0o600 }); await link(fixture.paths.originals, fixture.paths.output) })
  const record = await api.runOwnedLayoutEntry(fixture.environment, model.testing)
  assertIncomplete(record); unavailableOriginals(record)
  assert.equal((await readFile(fixture.paths.originals)).equals(stale), true); assert.equal((await readFile(fixture.paths.output)).equals(stale), true)
  assert.equal(record.comparison.floor_qualified, false); assert.equal(model.runners.length, 4)
})

test("input alias created during awaited sidecar temporary write is rejected before rename", async (t) => {
  const fixture = await context(t), model = comparisonModel(fixture), before = await readFile(fixture.paths.controller)
  const originalOpen = fsPromises.open; let changed = false
  fsPromises.open = async (...args) => {
    const file = await originalOpen(...args)
    if (typeof args[0] === "string" && basename(args[0]).startsWith(".mount-rs-owned-layout-")) {
      const originalWrite = file.writeFile.bind(file)
      file.writeFile = async (...writeArgs) => {
        await originalWrite(...writeArgs)
        if (!changed) { changed = true; await link(fixture.paths.controller, fixture.paths.originals) }
      }
    }
    return file
  }
  syncBuiltinESMExports()
  let record
  try { record = await api.runOwnedLayoutEntry(fixture.environment, model.testing) }
  finally { fsPromises.open = originalOpen; syncBuiltinESMExports() }
  assert.equal(changed, true, "the control must mutate the target during an actual awaited temporary write")
  assertIncomplete(record); unavailableOriginals(record)
  assert.equal((await readFile(fixture.paths.controller)).equals(before), true)
  assert.equal((await readFile(fixture.paths.originals)).equals(before), true)
  assert.equal((await readdir(fixture.root)).includes("artifact.json"), false)
  assert.equal((await readdir(fixture.root)).some((name) => name.startsWith(".mount-rs-owned-layout-")), false, "the owned temporary must be removed after refusal")
})

test("actual successful sidecar rename inode cannot be moved to projection and overwritten", async (t) => {
  const fixture = await context(t), model = comparisonModel(fixture, { floor: () => false }), replacement = Buffer.from("distinct replacement sidecar\n")
  const originalRename = fsPromises.rename; let moved = false, written
  fsPromises.rename = async (...args) => {
    await originalRename(...args)
    if (!moved && args[1] === fixture.paths.originals) {
      moved = true; written = await readFile(fixture.paths.originals)
      await originalRename(fixture.paths.originals, fixture.paths.output)
      await writeFile(fixture.paths.originals, replacement, { mode: 0o600 })
    }
  }
  syncBuiltinESMExports()
  let record
  try { record = await api.runOwnedLayoutEntry(fixture.environment, model.testing) }
  finally { fsPromises.rename = originalRename; syncBuiltinESMExports() }
  assert.equal(moved, true, "the control must move the successfully published sidecar inode")
  assertIncomplete(record); unavailableOriginals(record)
  assert.equal(record.failure_code, "OWNED_LAYOUT_ENTRY_PUBLICATION_FAILED")
  assert.equal((await readFile(fixture.paths.output)).equals(written), true, "projection publication cannot overwrite retained originals moved to its path")
  assert.equal((await readFile(fixture.paths.originals)).equals(replacement), true)
  assert.equal(record.comparison.floor_qualified, false)
  for (const arm of record.comparison.arms) assert.equal(arm.outcome.status, "failed")
  assert.equal((await readdir(fixture.root)).some((name) => name.startsWith(".mount-rs-owned-layout-")), false)
})

for (const change of ["replacement", "in-place content mutation"]) test(`published sidecar ${change} before projection loses current-success linkage`, async (t) => {
  const fixture = await context(t), model = comparisonModel(fixture), replacement = Buffer.from("changed private originals\n")
  const tombstone = join(fixture.root, "retained-originals-inode.json")
  const originalRename = fsPromises.rename, originalUnlink = fsPromises.unlink
  let renamed = false, mutated = false, beforeIdentity, afterIdentity, beforeBytes
  fsPromises.rename = async (...args) => { await originalRename(...args); if (args[1] === fixture.paths.originals) renamed = true }
  fsPromises.unlink = async (...args) => {
    if (renamed && !mutated && typeof args[0] === "string" && basename(args[0]).startsWith(".mount-rs-owned-layout-")) {
      mutated = true; beforeIdentity = await lstat(fixture.paths.originals, { bigint: true }); beforeBytes = await readFile(fixture.paths.originals)
      // Keep the prior inode allocated: unlink/recreate can reuse it on Linux.
      if (change === "replacement") await originalRename(fixture.paths.originals, tombstone)
      await writeFile(fixture.paths.originals, replacement, { mode: 0o600 })
      afterIdentity = await lstat(fixture.paths.originals, { bigint: true })
    }
    return originalUnlink(...args)
  }
  syncBuiltinESMExports()
  let record
  try { record = await api.runOwnedLayoutEntry(fixture.environment, model.testing) }
  finally { fsPromises.rename = originalRename; fsPromises.unlink = originalUnlink; syncBuiltinESMExports() }
  assert.equal(mutated, true, "the control must alter the sidecar after successful rename and before projection publication")
  assert.equal(beforeIdentity.ino === afterIdentity.ino, change === "in-place content mutation")
  if (change === "replacement") {
    assert.equal((await readdir(fixture.root)).includes(basename(tombstone)), true, "the replacement fixture must keep the old inode allocated")
    const retainedIdentity = await lstat(tombstone, { bigint: true }), replacementIdentity = await lstat(fixture.paths.originals, { bigint: true })
    assert.deepEqual([retainedIdentity.dev, retainedIdentity.ino], [beforeIdentity.dev, beforeIdentity.ino])
    assert.notDeepEqual([retainedIdentity.dev, retainedIdentity.ino], [replacementIdentity.dev, replacementIdentity.ino], "both simultaneously existing files must have distinct identities")
    assert.equal((await readFile(tombstone)).equals(beforeBytes), true)
  }
  assertIncomplete(record); unavailableOriginals(record)
  assert.equal(record.failure_code, "OWNED_LAYOUT_ENTRY_PUBLICATION_FAILED")
  assert.equal((await readFile(fixture.paths.originals)).equals(replacement), true)
  assert.equal((await readdir(fixture.root)).includes("artifact.json"), false)
  assert.equal((await readdir(fixture.root)).some((name) => name.startsWith(".mount-rs-owned-layout-")), false)
})

test("sidecar mutation during projection temporary write revokes current private metadata before rename", async (t) => {
  const fixture = await context(t), model = comparisonModel(fixture, { floor: () => false }), replacement = Buffer.from("sidecar changed during projection write\n")
  const originalOpen = fsPromises.open; let writes = 0, changed = false
  fsPromises.open = async (...args) => {
    const file = await originalOpen(...args)
    if (typeof args[0] === "string" && basename(args[0]).startsWith(".mount-rs-owned-layout-")) {
      const originalWrite = file.writeFile.bind(file)
      file.writeFile = async (...writeArgs) => {
        await originalWrite(...writeArgs)
        if (++writes === 2) { changed = true; await writeFile(fixture.paths.originals, replacement) }
      }
    }
    return file
  }
  syncBuiltinESMExports()
  let record
  try { record = await api.runOwnedLayoutEntry(fixture.environment, model.testing) }
  finally { fsPromises.open = originalOpen; syncBuiltinESMExports() }
  assert.equal(changed, true, "the sidecar must change during the actual projection temporary write")
  assertIncomplete(record); unavailableOriginals(record)
  assert.equal(record.failure_code, "OWNED_LAYOUT_ENTRY_PUBLICATION_FAILED")
  assert.equal((await readFile(fixture.paths.originals)).equals(replacement), true)
  assert.equal((await readdir(fixture.root)).includes("artifact.json"), false)
  assert.equal((await readdir(fixture.root)).some((name) => name.startsWith(".mount-rs-owned-layout-")), false)
  assert.equal(record.comparison.floor_qualified, false)
  for (const arm of record.comparison.arms) assert.equal(arm.outcome.status, "failed")
})

test("projection-only native alias rejection preserves a verified intact private sidecar receipt", async (t) => {
  const fixture = await context(t), model = comparisonModel(fixture), nativeBytes = await readFile(fixture.paths.native)
  const originalRename = fsPromises.rename, originalUnlink = fsPromises.unlink; let renamed = false, changed = false
  fsPromises.rename = async (...args) => { await originalRename(...args); if (args[1] === fixture.paths.originals) renamed = true }
  fsPromises.unlink = async (...args) => {
    if (renamed && !changed && typeof args[0] === "string" && basename(args[0]).startsWith(".mount-rs-owned-layout-")) {
      changed = true; await symlink(fixture.paths.native, fixture.paths.output)
    }
    return originalUnlink(...args)
  }
  syncBuiltinESMExports()
  let record
  try { record = await api.runOwnedLayoutEntry(fixture.environment, model.testing) }
  finally { fsPromises.rename = originalRename; fsPromises.unlink = originalUnlink; syncBuiltinESMExports() }
  assert.equal(changed, true)
  assertIncomplete(record); assert.equal(record.failure_code, "OWNED_LAYOUT_ENTRY_PUBLICATION_FAILED")
  await readOriginals(fixture, record)
  assert.equal((await readFile(fixture.paths.native)).equals(nativeBytes), true)
  assert.equal((await lstat(fixture.paths.output)).isSymbolicLink(), true)
  assert.equal(record.qualification.safe_to_continue, false)
})

for (const failure of ["none", "configuration", "testing", "preflight"]) test(`mutual output path alias preserves bytes on ${failure} path`, async (t) => {
  const fixture = await context(t), model = comparisonModel(fixture), before = Buffer.from("existing public bytes\n")
  await writeFile(fixture.paths.output, before, { mode: 0o600 })
  fixture.environment.MOUNT_RS_OWNED_LAYOUT_ORIGINALS_OUTPUT = fixture.paths.output
  if (failure === "configuration") Object.defineProperty(fixture.environment, "MOUNT_RS_RUSTFS_REGION", { get() { throw new Error(secret) } })
  if (failure === "testing") model.testing.unknown_dependency = secret
  if (failure === "preflight") fixture.environment.MOUNT_RS_PROFILE_IO = "0"
  const record = await api.runOwnedLayoutEntry(fixture.environment, model.testing)
  assert.equal((await readFile(fixture.paths.output)).equals(before), true)
  assertIncomplete(record); unavailableOriginals(record); assert.equal(model.events.length, 0)
})

for (const target of ["controller", "output", "native"]) test(`sidecar hardlink alias to ${target} is rejected before any publication`, async (t) => {
  const fixture = await context(t), model = comparisonModel(fixture)
  if (target === "output") await writeFile(fixture.paths.output, "prior public\n", { mode: 0o600 })
  const before = await readFile(fixture.paths[target])
  await link(fixture.paths[target], fixture.paths.originals)
  const record = await api.runOwnedLayoutEntry(fixture.environment, model.testing)
  assertIncomplete(record); unavailableOriginals(record); assert.equal(model.events.length, 0)
  assert.equal((await readFile(fixture.paths[target])).equals(before), true); assert.equal((await readFile(fixture.paths.originals)).equals(before), true)
  if (target !== "output") assert.equal((await readdir(fixture.root)).includes("artifact.json"), false)
})

for (const target of ["controller", "output", "native"]) test(`sidecar ancestor alias to ${target} protects both outputs`, async (t) => {
  const fixture = await context(t), model = comparisonModel(fixture), alias = join(fixture.root, "ancestor-alias")
  if (target === "output") await writeFile(fixture.paths.output, "prior public\n", { mode: 0o600 })
  const before = await readFile(fixture.paths[target])
  await symlink(dirname(fixture.root), alias)
  fixture.environment.MOUNT_RS_OWNED_LAYOUT_ORIGINALS_OUTPUT = join(alias, basename(fixture.root), basename(fixture.paths[target]))
  const record = await api.runOwnedLayoutEntry(fixture.environment, model.testing)
  assertIncomplete(record); unavailableOriginals(record); assert.equal(model.events.length, 0)
  assert.equal((await readFile(fixture.paths[target])).equals(before), true)
  if (target !== "output") assert.equal((await readdir(fixture.root)).includes("artifact.json"), false)
})

for (const output of ["MOUNT_RS_OWNED_LAYOUT_OUTPUT", "MOUNT_RS_OWNED_LAYOUT_ORIGINALS_OUTPUT"]) {
  for (const failure of ["none", "configuration", "testing", "preflight"]) test(`${output} selected-native alias preserves addon bytes on ${failure} path`, async (t) => {
    const fixture = await context(t), model = comparisonModel(fixture), before = await readFile(fixture.paths.native)
    fixture.environment[output] = fixture.paths.native
    if (failure === "configuration") Object.defineProperty(fixture.environment, "MOUNT_RS_RUSTFS_REGION", { get() { throw new Error(secret) } })
    if (failure === "testing") model.testing.unknown_dependency = secret
    if (failure === "preflight") fixture.environment.MOUNT_RS_PROFILE_IO = "0"
    const record = await api.runOwnedLayoutEntry(fixture.environment, model.testing)
    assert.equal((await readFile(fixture.paths.native)).equals(before), true)
    assertIncomplete(record); unavailableOriginals(record); assert.equal(model.events.length, 0)
    assert.equal((await readdir(fixture.root)).includes("artifact.json"), false)
    assert.equal((await readdir(fixture.root)).includes("originals.json"), false)
  })
}

for (const [name, mutate] of [
  ["missing", (env) => { delete env.MOUNT_RS_OWNED_LAYOUT_ORIGINALS_OUTPUT }],
  ["relative", (env) => { env.MOUNT_RS_OWNED_LAYOUT_ORIGINALS_OUTPUT = "originals.json" }],
  ["numeric", (env) => { env.MOUNT_RS_OWNED_LAYOUT_ORIGINALS_OUTPUT = 1 }],
  ["inaccessible parent", (env, f) => { env.MOUNT_RS_OWNED_LAYOUT_ORIGINALS_OUTPUT = join(f.root, "missing-parent", "originals.json") }],
]) test(`${name} mandatory sidecar target rejects publication before setup`, async (t) => {
  const fixture = await context(t), model = comparisonModel(fixture); mutate(fixture.environment, fixture)
  const record = await api.runOwnedLayoutEntry(fixture.environment, model.testing)
  assertIncomplete(record); unavailableOriginals(record); assert.equal(model.events.length, 0)
  assert.equal((await readdir(fixture.root)).includes("artifact.json"), false)
})

test("sidecar target getter is rejected without executing it or enabling either publication", async (t) => {
  const fixture = await context(t), model = comparisonModel(fixture); let reads = 0
  Object.defineProperty(fixture.environment, "MOUNT_RS_OWNED_LAYOUT_ORIGINALS_OUTPUT", { get() { reads++; throw new Error(secret) } })
  const record = await api.runOwnedLayoutEntry(fixture.environment, model.testing)
  assertIncomplete(record); unavailableOriginals(record); assert.equal(reads, 0); assert.equal(model.events.length, 0)
  assert.equal((await readdir(fixture.root)).includes("artifact.json"), false)
})

test("valid handoff joins exact private file hashes, eight fixtures, fixed Engine and owner-scoped prefixes", async (t) => {
  const fixture = await context(t), input = fixture.handoff(), handoff = api.validateOwnedLayoutHandoff(input, fixture.environment)
  assert.deepEqual(handoff.fixtures, fixture.fixtures)
  assert.deepEqual(handoff.engine, { socketPath: "/var/run/docker.sock" })
  assert.deepEqual(handoff.scope, fixture.controller.scope)
  assert.deepEqual(handoff.public, { status: "verified", generation: "1", fixture_count: 8,
    fixture_sha256: sha(fixture.fixtureBytes), engine_capability_sha256: sha(fixture.engineBytes),
    scope_sha256: sha(JSON.stringify(fixture.controller.scope)), endpoint_binding: "controller_manifest_join_verified",
    scope_binding: "owner_scoped_prefixes_verified", evidence_scope: "private_controller_created_endpoint_and_manifest_join; not_active_endpoint_probe" })
  deeplyFrozen(handoff)
  fixture.fixtures.entries[0].labels["mount-rs.tidb.run"] = secret
  fixture.controller.scope.owner = secret
  assert.equal(handoff.fixtures.entries[0].labels["mount-rs.tidb.run"], "model-tidb")
  assert.equal(handoff.scope.owner, "model-tidb")
  assert.doesNotMatch(JSON.stringify(handoff.public), new RegExp(`${secret}|mysql://|socket_path|metadataPrefix|blockPrefix`, "u"))
})

const handoffFaults = [
  ["controller extra fields", (f) => { f.controller.private_config = secret }],
  ["controller schema", (f) => { f.controller.schema = "mount-rs.owned-layout-controller.v2" }],
  ["controller generation", (f) => { f.controller.generation = "2" }],
  ["fixture generation", (f) => { f.fixtures.generation = "2" }],
  ["controller TiDB owner", (f) => { f.controller.tidb_owner = "foreign-owner" }],
  ["controller RustFS owner", (f) => { f.controller.rustfs_owner = "foreign-owner" }],
  ["fixture digest join", (f) => { f.controller.fixture_sha256 = "b".repeat(64) }],
  ["Engine digest join", (f) => { f.controller.engine_capability_sha256 = "b".repeat(64) }],
  ["duplicate CID", (f) => { f.fixtures.entries[1].cid = f.fixtures.entries[0].cid }],
  ["foreign owner label", (f) => { f.fixtures.entries[0].labels["mount-rs.tidb.run"] = secret }],
  ["missing fixture member", (f) => { f.fixtures.entries.pop() }],
  ["extra fixture member", (f) => { f.fixtures.entries.push(structuredClone(f.fixtures.entries[0])) }],
  ["nonliteral Engine socket", (f) => { f.engine.socket_path = "/private/foreign.sock" }],
  ["extra Engine capability", (f) => { f.engine.endpoint = secret }],
  ["Docker TCP selection", (f) => { f.environment.DOCKER_HOST = "tcp://127.0.0.1:2375" }],
  ["Docker context alias", (f) => { f.environment.DOCKER_CONTEXT = "default" }],
  ["Docker TLS alias", (f) => { f.environment.DOCKER_TLS_VERIFY = "1" }],
  ["Docker cert alias", (f) => { f.environment.DOCKER_CERT_PATH = secret }],
  ["scope ID format", (f) => { f.controller.scope.id = "layout-not-random" }],
  ["scope owner join", (f) => { f.controller.scope.owner = "model-rustfs" }],
  ["scope metadata prefix join", (f) => { f.controller.scope.metadataPrefix += "/foreign" }],
  ["scope block prefix join", (f) => { f.controller.scope.blockPrefix = f.controller.scope.metadataPrefix }],
  ["TiDB primary mismatch hidden by secondary", (f) => { f.environment.TIDB_URL = f.controller.tidb_url; f.environment.MOUNT_RS_TIDB_URL = "mysql://127.0.0.1:4001/foreign" }],
  ["TiDB controller expectation mismatch", (f) => { f.environment.MOUNT_RS_BACKING_EXPECT_TIDB_URL += "foreign" }],
  ["RustFS endpoint raw spelling mismatch", (f) => { f.environment.MOUNT_RS_RUSTFS_ENDPOINT = f.controller.rustfs_endpoint.slice(0, -1) }],
  ["RustFS endpoint controller expectation mismatch", (f) => { f.environment.MOUNT_RS_BACKING_EXPECT_R2_ENDPOINT = "http://127.0.0.1:19001/" }],
  ["RustFS bucket controller expectation mismatch", (f) => { f.environment.MOUNT_RS_BACKING_EXPECT_R2_BUCKET = "foreign-bucket" }],
  ["missing new RustFS region", (f) => { delete f.environment.MOUNT_RS_RUSTFS_REGION }],
  ["legacy R2 aliases cannot supply new RustFS config", (f) => {
    for (const key of ["ENDPOINT", "BUCKET", "REGION", "ACCESS_KEY_ID", "SECRET_ACCESS_KEY"]) {
      f.environment[`MOUNT_RS_R2_${key}`] = f.environment[`MOUNT_RS_RUSTFS_${key}`]; delete f.environment[`MOUNT_RS_RUSTFS_${key}`]
    }
  }],
  ["consistently external TiDB", (f) => { f.controller.tidb_url = f.environment.MOUNT_RS_TIDB_URL = f.environment.MOUNT_RS_BACKING_EXPECT_TIDB_URL = "mysql://foreign.invalid:4000/test" }],
  ["consistently external RustFS", (f) => { f.controller.rustfs_endpoint = f.environment.MOUNT_RS_RUSTFS_ENDPOINT = f.environment.MOUNT_RS_BACKING_EXPECT_R2_ENDPOINT = "http://foreign.invalid:9000/" }],
  ["TiDB query", (f) => { f.controller.tidb_url = f.environment.MOUNT_RS_TIDB_URL = f.environment.MOUNT_RS_BACKING_EXPECT_TIDB_URL = "mysql://127.0.0.1:4000/test?foreign=1" }],
  ["RustFS userinfo", (f) => { f.controller.rustfs_endpoint = f.environment.MOUNT_RS_RUSTFS_ENDPOINT = f.environment.MOUNT_RS_BACKING_EXPECT_R2_ENDPOINT = `http://user:${secret}@127.0.0.1:9000/` }],
]
for (const [name, mutate] of handoffFaults) test(`${name} rejects before any native/network/process dispatch`, async (t) => {
  const fixture = await context(t); mutate(fixture)
  assert.throws(() => api.validateOwnedLayoutHandoff(fixture.handoff(), fixture.environment), fixedError)
  assert.deepEqual(dispatches, { native: 0, network: 0, process: 0 })
})

test("effective secondary TiDB URL is accepted only when primary is absent", async (t) => {
  const fixture = await context(t); fixture.environment.TIDB_URL = fixture.environment.MOUNT_RS_TIDB_URL; delete fixture.environment.MOUNT_RS_TIDB_URL
  assert.equal(api.validateOwnedLayoutHandoff(fixture.handoff(), fixture.environment).public.status, "verified")
})
for (const [name, select, field] of [
  ["controller endpoint", (f) => f.controller, "rustfs_endpoint"], ["nested owner label", (f) => f.fixtures.entries[0].labels, "mount-rs.tidb.run"],
  ["known environment credential", (f) => f.environment, "MOUNT_RS_RUSTFS_SECRET_ACCESS_KEY"],
]) test(`${name} accessor is rejected without getter invocation`, async (t) => {
  const fixture = await context(t); let reads = 0
  Object.defineProperty(select(fixture), field, { enumerable: true, configurable: true, get() { reads++; throw new Error(secret) } })
  assert.throws(() => api.validateOwnedLayoutHandoff(fixture.handoff(), fixture.environment), fixedError)
  assert.equal(reads, 0)
})

test("modeled preflight recomputes actual selected-file and all fixed source hashes", async (t) => {
  const fixture = await context(t), identity = await api.preflightOwnedLayoutIdentity(fixture.environment, fixture.build,
    { mode: "modeled", checkoutMetadata: fixture.metadata })
  assert.equal(identity.selection, fixture.paths.native)
  assert.equal(identity.sha256, fixture.build.native_sha256)
  assert.equal(identity.mock, false)
  assert.equal(identity.public.kind, "selected_native_file")
  assert.equal(identity.public.real_native_proof, "unverified")
  assert.deepEqual(identity.public.source_sha256, fixture.build.source_sha256)
  deeplyFrozen(identity)
})
const buildFaults = [
  ["wrong build schema", (f) => { f.build.schema = "mount-rs.hosted-native-build.v1" }],
  ["extra build field", (f) => { f.build.private_note = secret }],
  ["dirty recorded source", (f) => { f.build.source_clean = false }],
  ["unlocked build", (f) => { f.build.locked = false }],
  ["debug build", (f) => { f.build.release = false }],
  ["generated JS build", (f) => { f.build.no_js = false }],
  ["failed build", (f) => { f.build.build_exit_code = 1 }],
  ["different checkout", (f) => { f.build.checkout_sha = "a".repeat(40) }],
  ["different native digest", (f) => { f.build.native_sha256 = "a".repeat(64) }],
  ["different entry source digest", (f) => { f.build.source_sha256["benchmarks/storage/owned-layout-entry.mjs"] = "b".repeat(64) }],
  ["missing runtime source digest", (f) => { delete f.build.source_sha256["benchmarks/storage/owned-layout-comparison.mjs"] }],
  ["extra unreviewed source digest", (f) => { f.build.source_sha256["private-config.json"] = "a".repeat(64) }],
  ["disabled profiling", (f) => { f.environment.MOUNT_RS_PROFILE_IO = "0" }],
  ["trace enabled", (f) => { f.environment.MOUNT_RS_TRACE_STORAGE = "1" }],
  ["WASI force alias", (f) => { f.environment.NAPI_RS_FORCE_WASI = "1" }],
  ["WASI flavor alias", (f) => { f.environment.NAPI_RS_WASI_FLAVOR = "wasm32" }],
  ["NODE_PATH alias", (f) => { f.environment.NODE_PATH = secret }],
  ["NODE_OPTIONS alias", (f) => { f.environment.NODE_OPTIONS = "--require=/private/foreign.cjs" }],
  ["capture mock selection", (f) => { f.environment.NAPI_RS_NATIVE_LIBRARY_PATH = join(directory, "capture-native.cjs") }],
  ["checkout-local addon selection", (f) => { f.environment.NAPI_RS_NATIVE_LIBRARY_PATH = join(repo, "unbuilt.node") }],
]
for (const [name, mutate] of buildFaults) test(`${name} fails selected source/build identity before loader`, async (t) => {
  const fixture = await context(t); mutate(fixture)
  await assert.rejects(api.preflightOwnedLayoutIdentity(fixture.environment, fixture.build, { mode: "modeled", checkoutMetadata: fixture.metadata }), fixedError)
})

test("current dirty checkout metadata cannot be replaced by a clean build assertion", async (t) => {
  const fixture = await context(t)
  await assert.rejects(api.preflightOwnedLayoutIdentity(fixture.environment, fixture.build, {
    mode: "modeled", checkoutMetadata: async () => ({ checkout_sha: checkout, source_clean: false }),
  }), fixedError)
})
test("production preflight rejects actual-process selection/profile mismatch without Git or native dispatch", async (t) => {
  const fixture = await context(t)
  assert.notEqual(process.env.NAPI_RS_NATIVE_LIBRARY_PATH, fixture.paths.native)
  await assert.rejects(api.preflightOwnedLayoutIdentity(fixture.environment, fixture.build), fixedError)
  assert.deepEqual(dispatches, { native: 0, network: 0, process: 0 })
})
test("selected addon larger than 128MiB is rejected using bounded reads", async (t) => {
  const fixture = await context(t); await truncate(fixture.paths.native, 128 * 1024 * 1024 + 1)
  await assert.rejects(api.preflightOwnedLayoutIdentity(fixture.environment, fixture.build, { mode: "modeled", checkoutMetadata: fixture.metadata }), fixedError)
})
test("cached selected addon is rejected before modeled preflight can bless it", async (t) => {
  const fixture = await context(t); require.cache[fixture.paths.native] = { loaded: true, exports: {} }
  try { await assert.rejects(api.preflightOwnedLayoutIdentity(fixture.environment, fixture.build, { mode: "modeled", checkoutMetadata: fixture.metadata }), fixedError) }
  finally { delete require.cache[fixture.paths.native] }
})
test("cached public loader is rejected before modeled preflight can bless it", async (t) => {
  const fixture = await context(t), publicLoader = join(repo, "bindings/mount-rs-napi/index.js")
  require.cache[publicLoader] = { loaded: true, exports: {} }
  try { await assert.rejects(api.preflightOwnedLayoutIdentity(fixture.environment, fixture.build, { mode: "modeled", checkoutMetadata: fixture.metadata }), fixedError) }
  finally { delete require.cache[publicLoader] }
})

test("ancestor-symlink selected addon cannot pass canonical native preflight", async (t) => {
  const fixture = await context(t), alias = join(fixture.root, "native-parent-alias")
  await symlink(fixture.root, alias)
  fixture.environment.NAPI_RS_NATIVE_LIBRARY_PATH = join(alias, `${secret}.node`)
  await assert.rejects(api.preflightOwnedLayoutIdentity(fixture.environment, fixture.build, { mode: "modeled", checkoutMetadata: fixture.metadata }), (error) => {
    fixedError(error); assert.equal(error.code, "OWNED_LAYOUT_ENTRY_NATIVE_SELECTION_REJECTED"); return true
  })
})

test("pure runner fixtures independently satisfy the real outcome and required native projection gates", () => {
  for (const layout of ["legacy", "compact"]) for (const floor of [true, false]) {
    const result = modeledRunner({ layout, floor }); addMeasuredNativeCounters(result); partialDaemonCounters(result)
    assert.deepEqual(validateRawPhaseDiagnostics(result.providers[0].storageDiagnostics.phases[0], "rustfs"), ["1"])
    assert.equal(assessOwnedLayoutRunnerOutcome(result).runner_safe_to_continue, true)
    assert.equal(projectOwnedLayoutPhaseMetrics(result.providers[0].storageDiagnostics.phases[0]).status, "observed")
  }
})

test("modeled actual coordinator publishes ABBA, exact SQL/native and nullable daemon counters without hosted claims", async (t) => {
  const fixture = await context(t), model = comparisonModel(fixture, { mutate: (result) => { addMeasuredNativeCounters(result); partialDaemonCounters(result) } })
  const record = await api.runOwnedLayoutEntry(fixture.environment, model.testing)
  assertModeled(record)
  assert.equal(record.failure_code, null); assert.equal(record.publication.status, "complete")
  assert.equal(record.comparison.status, "complete"); assert.equal(record.comparison.completed_arms, 4)
  assert.equal(record.comparison.floor_qualified, true); assert.equal(record.comparison.comparable, true); assert.equal(record.comparison.safe_to_continue, true)
  assert.deepEqual(record.comparison.arms.map(({ role }) => role), ["A1", "B1", "B2", "A2"])
  for (const arm of record.comparison.arms) {
    assert.equal(arm.outcome.status, "ok")
    const sql = arm.native_metrics.storage.entries.find(({ name }) => name === "tidb.sql.inode_read")
    assert.equal(sql.calls, "600"); assert.equal(sql.returned_rows, "400"); assert.equal(sql.elapsed_ns, "9007199254740993")
    assert.equal(arm.native_metrics.rustfs.instances[0].raw_api.entries[0].calls, "4")
    assert.equal(arm.native_metrics.rustfs.instances[0].local_work.entries[0].input_bytes, "16384")
    assert.equal(arm.native_metrics.process.status, "unavailable"); assert.equal(arm.native_metrics.core_profile.status, "unavailable")
    assert.equal(arm.backing.interval.complete, false)
    assert.equal(arm.backing.interval.metrics.block_operations.total, null)
    assert.equal(arm.backing.interval.metrics.block_operations.missing_members.length, 8)
    assert.equal(arm.backing.interval.metrics.block_bytes.total, null)
    assert.equal(arm.backing.boundaries[0].samples[0].stats.network_rx_bytes.a, null)
  }
  assert.equal(model.events.filter(({ event }) => event === "load").length, 1)
  assert.equal(model.native.creates.length, 12); assert.equal(model.observers.length, 4); assert.equal(model.runners.length, 4)
  assert.deepEqual(await readArtifact(fixture), record)
  assert.equal((await lstat(fixture.paths.output)).mode & 0o777, 0o600)
  assert.equal((await lstat(fixture.root)).mode & 0o777, 0o700)
  deeplyFrozen(record)
})

test("sole original floor failures are published as failed outcomes after all four safe arms", async (t) => {
  const fixture = await context(t), model = comparisonModel(fixture, { floor: () => false })
  const record = await api.runOwnedLayoutEntry(fixture.environment, model.testing)
  assertModeled(record)
  assert.equal(record.publication.status, "complete"); assert.equal(record.failure_code, null)
  assert.equal(record.comparison.status, "complete"); assert.equal(record.comparison.floor_qualified, false)
  assert.equal(record.comparison.comparable, true); assert.equal(record.comparison.safe_to_continue, true)
  for (const arm of record.comparison.arms) {
    assert.equal(arm.outcome.status, "failed"); assert.equal(arm.outcome.floor_qualified, false)
    assert.equal(arm.outcome.runner_safe_to_continue, true)
  }
  assert.equal(record.qualification.floor_qualified, false); assert.equal(record.qualification.comparable, true)
  assert.equal(model.runners.length, 4); assert.deepEqual(await readArtifact(fixture), record)
})

test("closed publication excludes credentials, endpoint URLs, paths, namespaces and arbitrary original data", async (t) => {
  const fixture = await context(t), model = comparisonModel(fixture, { mutate: (result) => {
    result.providers[0].storageDiagnostics.phases[0].private_note = secret
    result.results[0].rawSamples[0].path = `/private/${secret}`
  } })
  fixture.environment.UNUSED_PRIVATE_TOKEN = secret
  const record = await api.runOwnedLayoutEntry(fixture.environment, model.testing), serialized = JSON.stringify(record)
  assert.equal(record.publication.status, "complete")
  assert.doesNotMatch(serialized, new RegExp(`${secret}|mysql://|http://|foreign.invalid|socket_path|metadataPrefix|blockPrefix|"filesystem"|"benchmark"`, "u"))
  assert.ok(serialized.includes(fixture.build.native_sha256)); assert.ok(serialized.includes(checkout))
  assert.deepEqual(record.build.source_sha256, fixture.build.source_sha256)
  assert.equal(record.handoff.fixture_count, 8); assert.equal(record.handoff.fixture_sha256, sha(fixture.fixtureBytes))
  assert.equal(record.handoff.controller_receipt_sha256, sha(await readFile(fixture.paths.controller)))
  assert.equal(record.build.receipt_sha256, sha(await readFile(fixture.paths.build)))
  assert.equal(record.build.status, "verified")
  deeplyFrozen(record)
  model.runners[0].providers[0].storageDiagnostics.phases.length = 0
  assert.equal(JSON.stringify(record), serialized)
})

const privateFaults = [
  ["readable receipt mode", async (f) => chmod(f.paths.fixtures, 0o644)],
  ["readable parent mode", async (f) => chmod(f.root, 0o755)],
  ["symlink CID receipt", async (f) => { const original = f.paths.fixtures; const alias = join(f.root, "fixture-alias.json"); await symlink(original, alias); f.environment.MOUNT_RS_BACKING_CID_RECEIPT = alias }],
  ["hardlinked CID receipt", async (f) => link(f.paths.fixtures, join(f.root, "fixture-hardlink.json"))],
  ["oversized controller JSON", async (f) => writeFile(f.paths.controller, " ".repeat(16_385), { mode: 0o600 })],
  ["malformed build JSON", async (f) => writeFile(f.paths.build, `{ "secret": "${secret}"`, { mode: 0o600 })],
  ["duplicate decoded Engine key", async (f) => writeFile(f.paths.engine, '{"socket_path":"/var/run/docker.sock","\\u0073ocket_path":"/var/run/docker.sock"}', { mode: 0o600 })],
  ["duplicate nested owner label", async (f) => {
    const original = await readFile(f.paths.fixtures, "utf8")
    await writeFile(f.paths.fixtures, original.replace('"mount-rs.tidb.run": "model-tidb"', '"mount-rs.tidb.run": "model-tidb", "mount-rs.tidb.run": "model-tidb"'), { mode: 0o600 })
  }],
  ["controller source hash mismatch", async (f) => { f.build.source_sha256["scripts/test-rustfs.sh"] = "e".repeat(64); await f.writeInputs() }],
  ["endpoint setup mismatch", async (f) => { f.environment.MOUNT_RS_RUSTFS_ENDPOINT = "http://127.0.0.1:19001/" }],
]

for (const [input, variable] of [["fixtures", "MOUNT_RS_BACKING_CID_RECEIPT"], ["engine", "MOUNT_RS_BACKING_ENGINE_CAPABILITY"],
  ["controller", "MOUNT_RS_OWNED_LAYOUT_CONTROLLER_RECEIPT"], ["build", "MOUNT_RS_OWNED_LAYOUT_BUILD_RECEIPT"]]) {
  for (const output of ["MOUNT_RS_OWNED_LAYOUT_OUTPUT", "MOUNT_RS_OWNED_LAYOUT_ORIGINALS_OUTPUT"]) {
  for (const failure of ["none", "configuration", "testing", "preflight"]) test(`${output} alias to ${input} preserves receipt bytes after ${failure} setup`, async (t) => {
    const fixture = await context(t), model = comparisonModel(fixture)
    fixture.environment[output] = fixture.environment[variable]
    if (failure === "configuration") Object.defineProperty(fixture.environment, "MOUNT_RS_RUSTFS_REGION", { get() { throw new Error(secret) } })
    if (failure === "testing") model.testing.unknown_dependency = secret
    if (failure === "preflight") fixture.environment.MOUNT_RS_PROFILE_IO = "0"
    const before = await readFile(fixture.paths[input])
    const record = await api.runOwnedLayoutEntry(fixture.environment, model.testing)
    assert.equal((await readFile(fixture.paths[input])).equals(before), true, "publication must never overwrite a private input, even after earlier rejection")
    assertIncomplete(record); assert.equal(record.comparison, null)
    assert.equal(model.events.length, 0); assert.equal(model.runners.length, 0)
    assert.equal((await readdir(fixture.root)).includes(output === "MOUNT_RS_OWNED_LAYOUT_OUTPUT" ? "originals.json" : "artifact.json"), false, "a rejected output disables both publications")
  })
  }
}

test("canonical output alias through an ancestor symlink preserves a private receipt", async (t) => {
  const fixture = await context(t), model = comparisonModel(fixture), alias = join(fixture.root, "ancestor-alias")
  await symlink(dirname(fixture.root), alias)
  fixture.environment.MOUNT_RS_OWNED_LAYOUT_OUTPUT = join(alias, basename(fixture.root), "controller.json")
  const before = await readFile(fixture.paths.controller)
  const record = await api.runOwnedLayoutEntry(fixture.environment, model.testing)
  assert.equal((await readFile(fixture.paths.controller)).equals(before), true, "an ancestor alias cannot bypass input protection")
  assertIncomplete(record); assert.equal(record.comparison, null)
  assert.equal(model.events.length, 0); assert.equal(model.runners.length, 0)
})

for (const [name, mutate] of privateFaults) test(`${name} publishes fixed incomplete evidence without loading or starting an arm`, async (t) => {
  const fixture = await context(t), model = comparisonModel(fixture); await mutate(fixture)
  const record = await api.runOwnedLayoutEntry(fixture.environment, model.testing)
  assertIncomplete(record)
  assert.equal(record.comparison, null)
  assert.equal(model.events.length, 0); assert.equal(model.native.creates.length, 0); assert.equal(model.runners.length, 0)
  assert.doesNotMatch(JSON.stringify(record), new RegExp(`${secret}|mysql://|foreign.sock`, "u"))
})

for (const [name, mutate] of [
  ["missing explicit modeled mode", (testing) => { delete testing.mode }],
  ["unknown testing dependency", (testing) => { testing.comparisonFactory = () => { throw new Error(secret) } }],
  ["production mode as testing override", (testing) => { testing.mode = "production" }],
]) test(`${name} cannot bypass production setup or substitute a comparison`, async (t) => {
  const fixture = await context(t), model = comparisonModel(fixture); mutate(model.testing)
  const record = await api.runOwnedLayoutEntry(fixture.environment, model.testing)
  assertIncomplete(record); assert.equal(record.failure_code, "OWNED_LAYOUT_ENTRY_TESTING_REJECTED")
  assert.equal(model.events.length, 0)
})

test("known-field runtime environment getter is rejected without executing it or loading", async (t) => {
  const fixture = await context(t), model = comparisonModel(fixture); let reads = 0
  Object.defineProperty(fixture.environment, "MOUNT_RS_RUSTFS_ENDPOINT", { enumerable: true, get() { reads++; throw new Error(secret) } })
  const record = await api.runOwnedLayoutEntry(fixture.environment, model.testing)
  assertIncomplete(record); assert.equal(reads, 0); assert.equal(model.events.length, 0)
})

test("explicit modeled loader failure is fixed, private and incomplete", async (t) => {
  const fixture = await context(t), model = comparisonModel(fixture)
  model.testing.loadBinding = async () => { model.events.push({ event: "load" }); throw Object.assign(new Error(secret), { code: secret, path: secret }) }
  const record = await api.runOwnedLayoutEntry(fixture.environment, model.testing)
  assertIncomplete(record); assert.equal(record.failure_code, "OWNED_LAYOUT_ENTRY_NATIVE_LOAD_FAILED")
  assert.equal(model.events.length, 1); assert.equal(model.runners.length, 0)
  assert.doesNotMatch(JSON.stringify(record), new RegExp(secret, "u"))
})

test("actual coordinator failure preserves supported original floor and uncertainty instead of claiming qualification", async (t) => {
  const fixture = await context(t), model = comparisonModel(fixture, { mutate: (result) => {
    result.providers[0].backingObserver.terminal.prior_native_uncertainty = true
    result.providers[0].backingObserver.terminal.safe_to_continue_pair = false
    result.providers[0].backingObserver.backing_evidence.terminal.prior_native_uncertainty = true
    result.providers[0].backingObserver.backing_evidence.terminal.safe_to_continue_pair = false
  } })
  const record = await api.runOwnedLayoutEntry(fixture.environment, model.testing)
  assert.equal(record.publication.status, "complete", "complete publication is distinct from a stopped comparison")
  assert.equal(record.comparison.status, "stopped"); assert.equal(record.comparison.native_uncertainty, true)
  assert.equal(record.comparison.arms[0].outcome.floor_qualified, true)
  assert.equal(record.comparison.complete, false); assert.equal(record.qualification.hosted, false)
  assert.equal(record.qualification.safe_to_continue, false); assert.equal(model.runners.length, 1)
})

for (const argv of [[], ["run", "extra"], ["fixture-tidb"], ["run", "--modeled"], ["--help"]]) test(`main rejects exact argument shape ${JSON.stringify(argv)} without setup dispatch`, async () => {
  const before = { ...dispatches }, writes = [], original = process.stderr.write
  process.stderr.write = function (value) { writes.push(String(value)); return true }
  try {
    assert.equal(await api.main(argv, { MOUNT_RS_RUSTFS_SECRET_ACCESS_KEY: secret }), 1)
    assert.match(writes.join(""), /^OWNED_LAYOUT_ENTRY_FAILURE OWNED_LAYOUT_ENTRY_[A-Z_]+\n$/u)
    assert.doesNotMatch(writes.join(""), new RegExp(secret, "u"))
  } finally { process.stderr.write = original }
  assert.deepEqual(dispatches, before)
})


test("modeled lowered publication cap retains original floor statuses in a fixed incomplete envelope", async (t) => {
  const fixture = await context(t), model = comparisonModel(fixture, { floor: () => false })
  model.testing.publicationMaximum = 65_536
  const record = await api.runOwnedLayoutEntry(fixture.environment, model.testing)
  assertModeled(record); assertIncomplete(record)
  assert.equal(record.failure_code, "OWNED_LAYOUT_ENTRY_OUTPUT_CAP")
  assert.equal(record.publication.reason, "OWNED_LAYOUT_ENTRY_OUTPUT_CAP")
  assert.equal(record.comparison.status, "complete", "supported original protocol status remains distinguishable from publication")
  assert.equal(record.comparison.floor_qualified, false); assert.equal(record.comparison.native_uncertainty, false)
  assert.equal(record.comparison.complete, false); assert.equal(record.comparison.comparable, false); assert.equal(record.comparison.safe_to_continue, false)
  for (const arm of record.comparison.arms) assert.equal(arm.outcome.status, "failed")
  assert.equal(record.qualification.hosted, false); assert.equal(record.qualification.comparable, false)
  assert.equal(model.runners.length, 4)
  const bytes = await readFile(fixture.paths.output)
  assert.ok(bytes.length <= 65_536); assert.deepEqual(JSON.parse(bytes), record)
  const retained = await readOriginals(fixture, record)
  assert.ok(retained.bytes.length > 65_536, "the modeled projection ceiling must not lower the private originals ceiling")
  assert.equal(Object.hasOwn(record, "publicationMaximum"), false)
  assert.doesNotMatch(bytes.toString("utf8"), new RegExp(`${secret}|PRIVATE_FLOOR_MESSAGE`, "u"))
})
for (const maximum of [0, 65_535, 33_554_433, 1.5, "65536"]) test(`modeled publication cap rejects unsupported value ${maximum}`, async (t) => {
  const fixture = await context(t), model = comparisonModel(fixture); model.testing.publicationMaximum = maximum
  const record = await api.runOwnedLayoutEntry(fixture.environment, model.testing)
  assertIncomplete(record); assert.equal(record.failure_code, "OWNED_LAYOUT_ENTRY_TESTING_REJECTED"); assert.equal(model.events.length, 0)
})
test("lowered publication cap cannot be supplied without explicit modeled scope", async (t) => {
  const fixture = await context(t)
  const record = await api.runOwnedLayoutEntry(fixture.environment, { publicationMaximum: 65_536 })
  assertIncomplete(record); assert.equal(record.failure_code, "OWNED_LAYOUT_ENTRY_TESTING_REJECTED")
})

for (const testing of [null, 0, true, [], new (class Testing {})()]) test(`non-record testing value ${String(testing)} is fixed and incomplete before setup`, async (t) => {
  const fixture = await context(t)
  const record = await api.runOwnedLayoutEntry(fixture.environment, testing)
  assertIncomplete(record); assert.equal(record.failure_code, "OWNED_LAYOUT_ENTRY_TESTING_REJECTED")
  assert.equal(record.comparison, null); assert.equal(record.handoff, null)
  assert.deepEqual(JSON.parse(await readFile(fixture.paths.output, "utf8")), record)
  await assert.rejects(api.preflightOwnedLayoutIdentity(fixture.environment, fixture.build, testing), (error) => {
    fixedError(error); assert.equal(error.code, "OWNED_LAYOUT_ENTRY_TESTING_REJECTED"); return true
  })
})

function childGuards({ metadata = false } = {}) {
  return `
    import Module, { syncBuiltinESMExports } from 'node:module';
    import childProcess from 'node:child_process';
    import http from 'node:http'; import https from 'node:https'; import net from 'node:net'; import tls from 'node:tls'; import dgram from 'node:dgram';
    const counts = { native:0, public_loader_attempts:0, wasi_attempts:0, network:0, process:0, modeled_metadata_callbacks:0 }, attempts = [];
    const originalLoad=Module._load, originalResolve=Module._resolveFilename;
    const publicLoader=${JSON.stringify(join(repo, "bindings/mount-rs-napi/index.js"))};
    const isWasi=(name)=>typeof name==='string' && /wasi|wasm32|\\.wasm(?:$|[?#])/iu.test(name);
    Module._load=function(name,...args) {
      if(typeof name==='string' && (name===publicLoader || name.endsWith('/bindings/mount-rs-napi/index.js'))) {
        counts.public_loader_attempts++; throw new Error('OWNED_ENTRY_TEST_PUBLIC_LOADER_DENIED');
      }
      if(isWasi(name)) { counts.wasi_attempts++; throw new Error('OWNED_ENTRY_TEST_WASI_DENIED'); }
      return Reflect.apply(originalLoad,this,[name,...args]);
    };
    Module._resolveFilename=function(name,...args) {
      if(isWasi(name)) { counts.wasi_attempts++; throw new Error('OWNED_ENTRY_TEST_WASI_RESOLUTION_DENIED'); }
      return Reflect.apply(originalResolve,this,[name,...args]);
    };
    Module._extensions['.node'] = () => { counts.native++; throw new Error('OWNED_ENTRY_TEST_NATIVE_DENIED'); };
    const denyNetwork = () => { counts.network++; throw new Error('OWNED_ENTRY_TEST_NETWORK_DENIED'); };
    for (const [object, keys] of [[http,['request','get']],[https,['request','get']],[net,['connect','createConnection']],
      [net.Socket.prototype,['connect']],[net.Server.prototype,['listen']],[tls,['connect']],[dgram,['createSocket']]]) for(const key of keys) object[key]=denyNetwork;
    globalThis.fetch=denyNetwork;
    for(const key of ['exec','execSync','execFile','execFileSync','spawn','spawnSync','fork']) childProcess[key] = (file,args,options,callback) => {
      const approved = key==='execFile' && file==='git' && options?.cwd===${JSON.stringify(repo)} &&
        (JSON.stringify(args)===JSON.stringify(['rev-parse','HEAD']) || JSON.stringify(args)===JSON.stringify(['status','--porcelain=v1','--untracked-files=all']));
      attempts.push({api:key,file,args,cwd:options?.cwd,approved_metadata:approved});
      if (${metadata} && approved && typeof callback==='function') {
        counts.modeled_metadata_callbacks++;
        queueMicrotask(()=>callback(null,args[0]==='rev-parse'?${JSON.stringify(checkout + "\n")}:'', ''));
        return {on(){return this}, once(){return this}, kill(){throw new Error('OWNED_ENTRY_TEST_KILL_DENIED')}};
      }
      counts.process++; throw new Error('OWNED_ENTRY_TEST_PROCESS_DENIED');
    };
    syncBuiltinESMExports();
    process.on('exit',()=>process.stderr.write('OWNED_ENTRY_TEST_GUARDS '+JSON.stringify({...counts,attempts,
      actual_native_loads:0,actual_network_calls:0,actual_child_processes:0})+'\\n'));
  `
}
function parseChildGuards(result) {
  return JSON.parse(String(result.stderr).match(/OWNED_ENTRY_TEST_GUARDS (\{[^\n]+\})/u)?.[1] || "null")
}
async function launchChild(args, environment) {
  return new Promise((resolveChild) => {
    executeChild(process.execPath, args, { cwd: repo, env: { PATH: process.env.PATH, ...environment },
      encoding: "utf8", timeout: 5_000, maxBuffer: 1024 * 1024 }, (error, stdout, stderr) => {
      resolveChild({ code: error ? error.code : 0, stdout, stderr, signal: error?.signal, killed: error?.killed === true })
    })
  })
}

test("entry import with profiling disabled is inert under child native/network/process denial", async (t) => {
  // An absent entry fails this capability assertion rather than a module error.
  assert.equal(typeof api.main, "function")
  assert.equal(api.SOURCE_PATHS.length, 28, "missing entry cannot provide the runtime source closure")
  const fixture = await context(t), preload = join(fixture.root, "import-guards.mjs")
  await writeFile(preload, childGuards(), { mode: 0o600 })
  const script = `import assert from 'node:assert/strict'; const before=JSON.stringify(process.env);
    const api=await import(${JSON.stringify(entryURL.href)}); assert.equal(typeof api.main,'function');
    assert.equal(JSON.stringify(process.env),before); process.stdout.write('OWNED_ENTRY_IMPORT_INERT\\n');`
  const result = await launchChild(["--import", preload, "--input-type=module", "-e", script], { MOUNT_RS_PROFILE_IO: "0", MOUNT_RS_TRACE_STORAGE: "0" })
  assert.equal(result.code, 0, result.stderr)
  assert.equal(result.signal == null, true); assert.equal(result.killed, false)
  assert.match(result.stdout, /OWNED_ENTRY_IMPORT_INERT/u)
  const guards = parseChildGuards(result)
  assert.deepEqual({ native: guards?.native, network: guards?.network, process: guards?.process }, { native: 0, network: 0, process: 0 })
  assert.equal(guards.public_loader_attempts, 0); assert.equal(guards.wasi_attempts, 0)
  assert.equal(guards.actual_native_loads, 0); assert.equal(guards.actual_network_calls, 0); assert.equal(guards.actual_child_processes, 0)
  assert.equal(await readdir(fixture.root).then((names) => names.includes("artifact.json")), false)
})

test("actual CLI rejects an extra argument before reading setup or dispatching metadata", async (t) => {
  const fixture = await context(t), preload = join(fixture.root, "argument-guards.mjs")
  await writeFile(preload, childGuards(), { mode: 0o600 })
  const result = await launchChild(["--import", preload, entryPath, "run", "extra"], fixture.environment)
  assert.equal(result.code, 1); assert.equal(result.signal == null, true); assert.equal(result.killed, false)
  assert.match(result.stderr, /^OWNED_LAYOUT_ENTRY_FAILURE OWNED_LAYOUT_ENTRY_CONFIG_INVALID\n/mu)
  const guards = parseChildGuards(result)
  assert.deepEqual({ native: guards.native, network: guards.network, process: guards.process, public: guards.public_loader_attempts, wasi: guards.wasi_attempts },
    { native: 0, network: 0, process: 0, public: 0, wasi: 0 })
  assert.equal(guards.attempts.length, 0)
  assert.equal(await readdir(fixture.root).then((names) => names.includes("artifact.json")), false)
})

test("actual CLI rejects disabled profiling before Git metadata or native load", async (t) => {
  const fixture = await context(t), preload = join(fixture.root, "profile-guards.mjs")
  await writeFile(preload, childGuards(), { mode: 0o600 })
  const result = await launchChild(["--import", preload, entryPath, "run"], { ...fixture.environment, MOUNT_RS_PROFILE_IO: "0" })
  assert.equal(result.code, 1); assert.equal(result.signal == null, true); assert.equal(result.killed, false)
  const guards = parseChildGuards(result)
  assert.deepEqual({ native: guards.native, network: guards.network, process: guards.process, public: guards.public_loader_attempts, wasi: guards.wasi_attempts },
    { native: 0, network: 0, process: 0, public: 0, wasi: 0 })
  assert.equal(guards.attempts.length, 0)
  const record = await readArtifact(fixture)
  assertIncomplete(record); assert.equal(record.failure_code, "OWNED_LAYOUT_ENTRY_NATIVE_SELECTION_REJECTED")
  assert.equal(record.comparison, null); assert.equal(record.identity.profiling_enabled_before_load, false)
  unavailableOriginals(record); assert.equal((await readdir(fixture.root)).includes("originals.json"), false)
})

for (const metadata of [false, true]) test(`actual Node24 CLI settles under deny guards with modeled Git callbacks ${metadata}`, {
  skip: !implemented ? "CLI control awaits new owned-layout-entry.mjs; programmatic semantic REDs remain active" : false,
}, async (t) => {
  assert.match(process.version, /^v24\./u)
  const fixture = await context(t), preload = join(fixture.root, "cli-guards.mjs")
  await writeFile(preload, childGuards({ metadata }), { mode: 0o600 })
  const result = await launchChild(["--import", preload, entryPath, "run"], fixture.environment)
  assert.equal(result.code, 1, `CLI failure must settle rather than leaving its module promise unfinished: ${result.stderr}`)
  assert.equal(result.signal == null, true, "a signal or timeout is a real CLI failure")
  assert.equal(result.killed, false, "the child must settle and be reaped within five seconds")
  const guards = parseChildGuards(result)
  assert.ok(guards, "guard report must be emitted on normal child exit")
  assert.equal(guards.public_loader_attempts, 0, "selected-native failure must precede every public loader attempt")
  assert.equal(guards.wasi_attempts, 0, "selected-native failure cannot dispatch or resolve a WASI fallback candidate")
  assert.equal(guards.network, 0); assert.equal(guards.actual_native_loads, 0); assert.equal(guards.actual_network_calls, 0); assert.equal(guards.actual_child_processes, 0)
  assert.equal(guards.attempts.every(({ approved_metadata }) => approved_metadata), true, "only bounded read-only Git metadata calls may be attempted")
  if (metadata) {
    assert.equal(guards.process, 0); assert.equal(guards.modeled_metadata_callbacks, 2); assert.equal(guards.native, 1)
  } else {
    assert.ok(guards.process >= 1 && guards.process <= 2); assert.equal(guards.modeled_metadata_callbacks, 0); assert.equal(guards.native, 0)
  }
  const record = await readArtifact(fixture)
  assertIncomplete(record)
  assert.equal(record.runtime_scope, "production_entry_attempt")
  assert.equal(record.failure_code, metadata ? "OWNED_LAYOUT_ENTRY_NATIVE_LOAD_FAILED" : "OWNED_LAYOUT_ENTRY_BUILD_IDENTITY_REJECTED")
  assert.equal(record.comparison, null); assert.equal(record.identity.native_used_identity, "unverified")
  unavailableOriginals(record); assert.equal((await readdir(fixture.root)).includes("originals.json"), false)
  assert.match(result.stdout, /^OWNED_LAYOUT_ENTRY /mu)
  assert.doesNotMatch(result.stdout + result.stderr, new RegExp(`${secret}|OWNED_ENTRY_TEST_NATIVE_DENIED|OWNED_ENTRY_TEST_PROCESS_DENIED|at .*\\.mjs`, "u"))
  t.diagnostic(JSON.stringify({ scope: "actual_repository_CLI_with_denied_boundaries", actual_native_loads: guards.actual_native_loads,
    actual_network_calls: guards.actual_network_calls, actual_child_processes: guards.actual_child_processes,
    modeled_metadata_callbacks: guards.modeled_metadata_callbacks, denied_native_attempts: guards.native,
    public_loader_attempts: guards.public_loader_attempts, wasi_attempts: guards.wasi_attempts, child_exit_code: result.code }))
})
