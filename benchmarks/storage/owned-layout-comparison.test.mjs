import assert from "node:assert/strict"
import test from "node:test"
import { execFileSync } from "node:child_process"
import { createOwnedLayoutArm } from "./owned-layout-arm.mjs"
import { assessOwnedLayoutRunnerOutcome } from "./owned-layout-outcome.mjs"
import { projectOwnedLayoutPhaseMetrics } from "./owned-layout-metrics.mjs"
import { summarizeInterval } from "./backing-observer.mjs"
import { providerById as actualProviderById } from "./providers.mjs"
import {
  FOUNDATIONDB_DIAGNOSTIC_UNAVAILABLE, NATIVE_DIAGNOSTICS_SCHEMA, OBJECT_STORE_LOCAL_NAMES, RUSTFS_API_MEASUREMENT, RUSTFS_LOCAL_MEASUREMENT, STORAGE_BYTE_SEMANTICS,
  STORAGE_CALL_SEMANTICS, STORAGE_INSTRUMENTED_OPERATION_NAMES, STORAGE_OPERATION_FAMILIES,
  STORAGE_OPERATION_NAMES, STORAGE_ROW_SEMANTICS, TIDB_DIAGNOSTIC_COVERAGE,
  validateRawPhaseDiagnostics,
} from "./diagnostics.mjs"

// Missing implementation fails an intended capability assertion, not module loading.
// Every native, runner and engine input below is modeled; no backend is opened.
let api = {}
try { api = await import("./owned-layout-comparison.mjs") } catch (error) {
  if (error.code !== "ERR_MODULE_NOT_FOUND" || !error.message.includes("owned-layout-comparison.mjs")) throw error
}
function implementation() {
  assert.equal(typeof api.createOwnedLayoutComparison, "function", "owned layout comparison capability is missing")
  return api.createOwnedLayoutComparison
}

const providerId = "mount-rs-split-tidb-rustfs"
const roles = ["pd-1", "pd-2", "pd-3", "tikv-1", "tikv-2", "tikv-3", "tidb", "rustfs-service"]
const order = ["A1", "B1", "B2", "A2"]
const layouts = ["legacy", "compact", "compact", "legacy"]
const digest = "a".repeat(64)
const secret = "PRIVATE_COMPARISON_SECRET"
const observerCaps = Object.freeze({ maxContainers: 16, maxRequests: 257, maxBoundaries: 64, maxResponseBytes: 1_048_576,
  maxDeviceEntries: 128, maxNetworkInterfaces: 32, maxJournalBytes: 8_388_608, requestTimeoutMs: 2000, ownerTimeoutMs: 60_000 })
const observerRetention = "selected_projections_and_raw_version_inspect_or_first_stats_frame_sha256; consumed_trailing_bytes_counted_discarded; raw_bodies_discarded"
const identity = () => ({ selection: `/private/${secret}.node`, sha256: digest, mock: false,
  public: { kind: "selected_native_file", native_sha256: digest, mock_source_sha256: null, real_native_proof: "unverified",
    source_sha256: { "binding.cjs": "b".repeat(64) }, source_is_binary_build_revision: "unavailable" } })
const nativeIdentity = () => ({ native_used_identity: "verified", kind: "selected_native_file", native_sha256: digest })
function fixtureReceipt() {
  return { schema: "mount-rs.owned-backing-cids.v1", tidb_owner: "model-tidb", rustfs_owner: "model-rustfs", generation: "1",
    entries: roles.map((role, index) => ({ role, cid: String(index + 1).repeat(64), labels: role === "rustfs-service"
      ? { "com.mount-rs.rustfs-test": "mount-rs-rustfs-test", "com.mount-rs.rustfs-test-run": "model-rustfs" }
      : { "mount-rs.tidb.run": "model-tidb" } })) }
}
function input(binding = {}) {
  return { binding, environment: { MOUNT_RS_TIDB_URL: `mysql://user:${secret}@127.0.0.1:4000/storage`, MOUNT_RS_TIDB_DURABLE: "1",
    MOUNT_RS_RUSTFS_ENDPOINT: "http://127.0.0.1:19000", MOUNT_RS_RUSTFS_BUCKET: secret, MOUNT_RS_RUSTFS_REGION: "model-region",
    MOUNT_RS_RUSTFS_ACCESS_KEY_ID: secret, MOUNT_RS_RUSTFS_SECRET_ACCESS_KEY: secret, MOUNT_RS_RUSTFS_DURABLE: "0",
    MOUNT_RS_PROFILE_IO: "1", MOUNT_RS_TRACE_STORAGE: "0", MOUNT_RS_BACKING_TIDB_OWNER: "model-tidb",
    MOUNT_RS_BACKING_RUSTFS_OWNER: "model-rustfs", MOUNT_RS_BACKING_GENERATION: "1", NAPI_RS_NATIVE_LIBRARY_PATH: `/private/${secret}.node` },
    identity: identity(), fixtures: fixtureReceipt(), engine: { socketPath: `/private/${secret}.sock` },
    scope: { id: "comparison-one", owner: "comparison-owner", metadataPrefix: "comparison-one/metadata", blockPrefix: "comparison-one/blocks" } }
}
function deeplyFrozen(value) {
  assert.equal(Object.isFrozen(value), true)
  for (const child of Object.values(value)) if (child && typeof child === "object") deeplyFrozen(child)
}

test("import is inert with profiling disabled and no native, network or controller calls", () => {
  implementation()
  const script = `
    import assert from "node:assert/strict";
    import { createRequire, syncBuiltinESMExports } from "node:module";
    const require = createRequire(import.meta.url), Module = require("node:module"), originalLoad = Module._load;
    let dispatches = 0;
    const forbidden = () => { dispatches++; throw new Error("forbidden modeled dispatch"); };
    Module._load = function (name, ...args) {
      if (typeof name === "string" && (name.endsWith(".node") || name.includes("/napi/binding.cjs"))) return forbidden();
      return Reflect.apply(originalLoad, this, [name, ...args]);
    };
    require("node:http").request = forbidden;
    require("node:https").request = forbidden;
    require("node:net").connect = forbidden;
    require("node:net").createConnection = forbidden;
    for (const name of ["spawn", "exec", "execFile", "fork", "spawnSync", "execSync", "execFileSync"]) require("node:child_process")[name] = forbidden;
    globalThis.fetch = forbidden;
    syncBuiltinESMExports();
    const before = JSON.stringify(process.env);
    const api = await import(${JSON.stringify(new URL("./owned-layout-comparison.mjs", import.meta.url).href)});
    assert.equal(typeof api.createOwnedLayoutComparison, "function");
    assert.equal(JSON.stringify(process.env), before);
    assert.equal(dispatches, 0);
    assert.equal(Object.keys(require.cache).some(path => path.endsWith(".node") || path.includes("/napi/binding.cjs")), false);
  `
  execFileSync(process.execPath, ["--input-type=module", "-e", script], { env: { PATH: process.env.PATH,
    MOUNT_RS_PROFILE_IO: "0", MOUNT_RS_TRACE_STORAGE: "0" }, timeout: 3000, stdio: "pipe" })
})

function deferred() {
  let resolve, reject
  const promise = new Promise((yes, no) => { resolve = yes; reject = no })
  return { promise, resolve, reject }
}
async function flush() { for (let index = 0; index < 30; index++) await Promise.resolve() }
function fakeClock() {
  let current = 0, next = 0
  const timers = new Map()
  return { now: () => current, setTimeout: (callback, delay) => { const id = ++next; timers.set(id, { callback, at: current + delay }); return id },
    clearTimeout: (id) => timers.delete(id), utc: () => "2026-09-27T00:00:00Z", cpu: () => ({ user: 0, system: 0 }),
    jump(milliseconds, fire = true) {
      current += milliseconds
      if (fire) for (const [id, timer] of [...timers]) if (timer.at <= current) { timers.delete(id); timer.callback() }
    }, timers }
}
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

function harness(behavior = {}) {
  const events = [], clock = fakeClock(), native = fakeBinding(events, behavior), options = input(native.binding)
  if (behavior.budgets) options.budgets = behavior.budgets
  const arms = [], runners = [], observers = [], transports = [], results = []
  const dependencies = {
    clock: { now: clock.now, setTimeout: clock.setTimeout, clearTimeout: clock.clearTimeout },
    createArm(binding, armInput) {
      const index = arms.length, arm = createOwnedLayoutArm(binding, armInput)
      events.push({ event: "arm-create", index, cohort: arm.cohort })
      const wrapper = {
        prepare: async () => {
          events.push({ event: "prepare", index })
          const receipt = await arm.prepare()
          if (behavior.stage === "prepare" && index === 0) { behavior.entered = true; await behavior.gate.promise }
          return receipt
        },
        takeForRunner: () => { events.push({ event: "handoff", index }); return arm.takeForRunner() },
        cleanupOriginal: () => { events.push({ event: "coordinator-cleanup", index }); return arm.cleanupOriginal() },
        verifyPersistence: async () => {
          events.push({ event: "persistence", index })
          const receipt = await arm.verifyPersistence()
          if (behavior.stage === "persistence" && index === 0) { behavior.entered = true; await behavior.gate.promise }
          return receipt
        },
        snapshot: () => arm.snapshot(),
      }
      Object.defineProperty(wrapper, "cohort", { value: arm.cohort, enumerable: false })
      Object.freeze(wrapper)
      arms.push({ arm, wrapper, input: armInput })
      return wrapper
    },
    async providerById(environment) {
      events.push({ event: "provider-map" })
      assert.equal(environment.MOUNT_RS_RUSTFS_SECRET_ACCESS_KEY, secret)
      if (behavior.stage === "provider-map") { behavior.entered = true; await behavior.gate.promise }
      const definition = actualProviderById(environment).get(providerId)
      return new Map([[providerId, Object.freeze({ ...definition, create: () => { throw new Error("original provider constructor must be replaced") } })]])
    },
    createTransport(value) { const transport = Object.freeze({ marker: transports.length }); transports.push({ value, transport }); events.push({ event: "transport", index: transports.length - 1 }); return transport },
    createObserver(value) { const observer = Object.freeze({ marker: observers.length }); observers.push({ value, observer }); events.push({ event: "observer", index: observers.length - 1 }); return observer },
    async verifyBinding(value) {
      assert.equal(arguments[1], native.binding, "verify the exact supplied binding object")
      events.push({ event: "verify-binding", index: runners.length }); assert.equal(value.sha256, digest)
      if (behavior.stage === "verify") { behavior.entered = true; await behavior.gate.promise }
      return behavior.verifyIdentity ? behavior.verifyIdentity(runners.length) : nativeIdentity()
    },
    async runBenchmark(runnerOptions, environment, providers, runtime) {
      const index = runners.length
      runners.push({ options: runnerOptions, environment, providers, runtime })
      events.push({ event: "runner", index })
      assert.equal(providers instanceof Map, true); assert.deepEqual([...providers.keys()], [providerId])
      const opened = await providers.get(providerId).create({ layout: runnerOptions.layout, chunkSizeBytes: runnerOptions.chunkSizeBytes })
      assert.equal(opened.filesystem.closed, false)
      assert.equal(runtime.backingPilotIdentity.sha256, digest)
      assert.equal(runtime.backingObserver, observers[index].observer)
      const originalLayoutReceipt = await opened.filesystem.inspectCompactLayout()
      await opened.cleanup()
      const result = modeledRunner({ floor: behavior.floor ? behavior.floor(index) : true, layout: behavior.runnerLayout ? behavior.runnerLayout(index) : runnerOptions.layout })
      if (result.config.layout === "compact" && originalLayoutReceipt) result.providers[0].layoutSelection.persistedReceipt = originalLayoutReceipt
      result.providers[0].backingPilotIdentity = behavior.runnerIdentity ? behavior.runnerIdentity(index) : nativeIdentity()
      result.environment = { credentials: secret }
      if (behavior.mutateRunner) behavior.mutateRunner(result, index)
      results.push(result)
      if (behavior.stage === "runner" && index === 0) { behavior.entered = true; await behavior.gate.promise }
      return result
    },
  }
  return { options, dependencies, behavior, events, clock, native, arms, runners, observers, transports, results,
    construct: () => implementation()(options, dependencies) }
}

test("construction allocates all four private immutable cohorts before any dependency dispatch", () => {
  const calls = [], options = input()
  const unexpected = () => { calls.push("dispatch"); throw new Error(secret) }
  const comparison = implementation()(options, { createArm: unexpected, runBenchmark: unexpected, providerById: unexpected,
    createTransport: unexpected, createObserver: unexpected, verifyBinding: unexpected })
  assert.equal(calls.length, 0)
  assert.equal(Object.isFrozen(comparison), true)
  assert.equal(Object.keys(comparison).includes("cohorts"), false)
  assert.equal(comparison.cohorts.length, 4)
  assert.deepEqual(comparison.cohorts.map((cohort) => cohort.layout), layouts)
  deeplyFrozen(comparison.cohorts)
  for (const field of ["id", "metadataKey", "blockPrefix"]) assert.equal(new Set(comparison.cohorts.map((cohort) => cohort[field])).size, 4)
  for (const cohort of comparison.cohorts) {
    assert.equal(cohort.owner, options.scope.owner)
    assert.ok(cohort.metadataKey.startsWith(`${options.scope.metadataPrefix}/`))
    assert.ok(cohort.blockPrefix.startsWith(`${options.scope.blockPrefix}/`))
  }
  assert.equal(comparison.snapshot().status, "idle")
  assert.deepEqual(comparison.snapshot().order, order)
  assert.equal(comparison.snapshot().completed_arms, 0)
  assert.doesNotMatch(JSON.stringify(comparison), /PRIVATE_COMPARISON_SECRET/u)
})

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

test("modeled runner receipts independently satisfy the real outcome and raw RustFS contracts", () => {
  for (const layout of ["legacy", "compact"]) for (const floor of [true, false]) {
    const result = modeledRunner({ layout, floor })
    assert.deepEqual(validateRawPhaseDiagnostics(result.providers[0].storageDiagnostics.phases[0], "rustfs"), ["1"])
    assert.equal(assessOwnedLayoutRunnerOutcome(result).runner_safe_to_continue, true)
  }
})

test("ABBA completes through actual arm canary persistence and empty reopens", async () => {
  const fixture = harness(), comparison = fixture.construct(), record = await comparison.run()
  assert.equal(record.schema, "mount-rs.owned-layout-comparison.v1")
  assert.equal(record.status, "complete")
  assert.equal(record.completed_arms, 4)
  assert.equal(record.floor_qualified, true)
  assert.equal(record.comparable, true)
  assert.equal(record.native_uncertainty, false)
  assert.equal(record.pending_owned_call_count, 0)
  assert.deepEqual(record.order, order)
  assert.deepEqual(record.arms.map((arm) => arm.role), order)
  assert.deepEqual(record.arms.map((arm) => arm.layout), layouts)
  for (const arm of record.arms) {
    assert.equal(arm.outcome.runner_safe_to_continue, true)
    assert.equal(arm.persistence.complete, true)
    assert.equal(arm.persistence.canary.full_bytes_verified, true)
    assert.equal(arm.persistence.canary.eof_verified, true)
    assert.equal(arm.persistence.canary.remove_confirmed, true)
    assert.equal(arm.persistence.canary.logical_root_empty, true)
    assert.equal(arm.persistence.original_shutdown_confirmed, true)
    assert.equal(arm.persistence.reopen_shutdown_confirmed_count, 2)
    assert.equal(arm.native_identity_verified, true)
    assert.equal(arm.fixture_identity_stable, true)
    assert.deepEqual(arm.backing.caps, observerCaps)
    assert.equal(arm.backing.cost.requests, 33)
    assert.equal(arm.backing.cost.owner_lifetime_ms, arm.outcome.elapsed_ms + 4)
    assert.equal(arm.backing.journal_bytes, fixture.results[record.arms.indexOf(arm)].providers[0].backingObserver.backing_evidence.journal_bytes)
  }
  assert.equal(fixture.native.creates.length, 12)
  assert.deepEqual(fixture.events.filter(({ event }) => event === "prepare").map(({ index }) => index), [0, 1, 2, 3])
  assert.equal(fixture.events.filter(({ event }) => event === "coordinator-cleanup").length, 0, "the runner already owns original cleanup")
  deeplyFrozen(record)
  assert.deepEqual(comparison.snapshot(), record)
})

test("each arm receives exact immutable lifecycle options and one fresh observer after prepare", async () => {
  const fixture = harness(), comparison = fixture.construct()
  const pinned = structuredClone(fixture.options.environment)
  fixture.options.environment.MOUNT_RS_TIDB_URL = `mysql://${secret}@foreign.invalid/other`
  fixture.options.environment.MOUNT_RS_RUSTFS_SECRET_ACCESS_KEY = "replacement-secret"
  fixture.options.scope.metadataPrefix = "foreign/metadata"
  fixture.options.fixtures.entries[0].cid = "f".repeat(64)
  const record = await comparison.run()
  assert.equal(record.status, "complete")
  assert.equal(fixture.runners.length, 4)
  assert.equal(fixture.observers.length, 4)
  assert.equal(new Set(fixture.observers.map(({ observer }) => observer)).size, 4)
  assert.equal(new Set(fixture.transports.map(({ transport }) => transport)).size, 4)
  for (const [index, call] of fixture.runners.entries()) {
    deeplyFrozen(call.options)
    assert.deepEqual({ layout: call.options.layout, workload: call.options.workload, sizes: call.options.sizes,
      payloadBytes: call.options.payloadBytes, iterations: call.options.iterations, concurrency: call.options.concurrency,
      chunkSizeBytes: call.options.chunkSizeBytes, minIops: call.options.minIops, requireConfigured: call.options.requireConfigured,
      providers: call.options.providers }, { layout: layouts[index], workload: "lifecycle", sizes: [1], payloadBytes: 4096,
      iterations: 400, concurrency: 64, chunkSizeBytes: 65536, minIops: 1000, requireConfigured: true, providers: [providerId] })
    assert.equal(call.environment.MOUNT_RS_TIDB_URL, pinned.MOUNT_RS_TIDB_URL)
    assert.equal(call.runtime.backingObserverWindow, "workload")
    const nativeOptions = fixture.native.creates[index * 3]
    assert.equal(nativeOptions.metadata.kind, "tidb")
    assert.equal(nativeOptions.blocks.kind, "rustfs")
    assert.equal(nativeOptions.metadata.uri, pinned.MOUNT_RS_TIDB_URL)
    assert.equal(nativeOptions.blocks.secretAccessKey, pinned.MOUNT_RS_RUSTFS_SECRET_ACCESS_KEY)
    assert.equal(nativeOptions.metadata.key, comparison.cohorts[index].metadataKey)
    assert.equal(nativeOptions.blocks.key, comparison.cohorts[index].blockPrefix)
    for (const flag of ["concurrentWrites", "inodeUpdates", "compactInodeUpdates"]) assert.equal(nativeOptions[flag], layouts[index] === "compact")
    const preflightAt = fixture.events.findIndex(({ event, key }) => event === "preflight" && key === nativeOptions.metadata.key)
    const observerAt = fixture.events.findIndex(({ event, index: eventIndex }) => event === "observer" && eventIndex === index)
    const runnerAt = fixture.events.findIndex(({ event, index: eventIndex }) => event === "runner" && eventIndex === index)
    assert.ok(preflightAt < observerAt && observerAt < runnerAt)
    const observerOptions = fixture.observers[index].value
    assert.deepEqual(observerOptions.allowlist, fixtureReceipt().entries)
    assert.equal(observerOptions.transport, fixture.transports[index].transport)
    assert.equal(observerOptions.caps, undefined, "factory hard caps must remain unchanged")
    for (const method of ["now", "utc", "cpu", "setTimeout", "clearTimeout"]) assert.equal(typeof observerOptions.clock[method], "function")
    assert.deepEqual(fixture.transports[index].value.allowlistedCids, fixtureReceipt().entries.map(({ cid }) => cid))
  }
})

test("sole floor failures keep all original failed statuses while the safe ABBA sequence completes", async () => {
  const fixture = harness({ floor: (index) => index % 2 === 0 }), record = await fixture.construct().run()
  assert.equal(record.status, "complete")
  assert.equal(record.completed_arms, 4)
  assert.equal(record.comparable, true)
  assert.equal(record.floor_qualified, false)
  for (const index of [1, 3]) {
    const arm = record.arms[index]
    assert.equal(arm.outcome.floor_qualified, false)
    assert.equal(arm.outcome.sole_floor_failure, true)
    assert.equal(arm.outcome.runner_safe_to_continue, true)
    assert.deepEqual([arm.outcome.status, arm.outcome.provider_status, arm.outcome.result_status, arm.outcome.observer_status, arm.outcome.backing_status], Array(5).fill("failed"))
    assert.equal(arm.persistence.complete, true)
    assert.equal(fixture.results[index].results[0].failures[0].error.code, "IOPS_TARGET_NOT_MET")
    assert.equal(fixture.results[index].status, "failed")
  }
})

for (const [name, mutate] of [
  ["sample errors", (value) => value.results[0].rawSamples[0].errors.push({ operation: "read", error: { message: secret } })],
  ["setup error", (value) => { value.providers[0].setupError = { message: secret } }],
  ["configuration failure", (value) => value.configurationFailures.push({ reason: secret })],
  ["pending operations", (value) => value.providers[0].cleanup.pendingOperations.push({ path: secret })],
  ["deferred cleanup", (value) => { value.providers[0].cleanup.resource.status = "deferred" }],
  ["observer uncertainty", (value) => { value.providers[0].backingObserver.terminal.prior_native_uncertainty = true }],
]) test(`${name} stops before persistence or the next arm despite a nominally safe receipt`, async () => {
  const fixture = harness({ mutateRunner: mutate }), record = await fixture.construct().run()
  assert.equal(record.status, "stopped")
  assert.equal(record.completed_arms, 0)
  assert.equal(record.safe_to_continue, false)
  assert.equal(record.comparable, false)
  assert.equal(fixture.runners.length, 1)
  assert.equal(fixture.events.filter(({ event }) => event === "prepare").length, 1)
  assert.equal(fixture.events.filter(({ event }) => event === "persistence").length, 0)
  assert.match(record.stop_code, /^COORDINATOR_[A-Z_]+$/u)
  assert.doesNotMatch(JSON.stringify(record), /PRIVATE_COMPARISON_SECRET/u)
})

test("a clean runner of the wrong layout cannot substitute for its scheduled cohort", async () => {
  const fixture = harness({ runnerLayout: () => "compact" }), record = await fixture.construct().run()
  assert.equal(assessOwnedLayoutRunnerOutcome(fixture.results[0]).runner_safe_to_continue, true)
  assert.equal(record.status, "stopped")
  assert.equal(fixture.runners.length, 1)
  assert.equal(fixture.events.filter(({ event }) => event === "persistence").length, 0)
})

for (const source of ["prearm", "runner"]) test(`${source} native identity drift stops the sequence`, async () => {
  const behavior = source === "prearm" ? { verifyIdentity: () => ({ ...nativeIdentity(), native_sha256: "c".repeat(64) }) }
    : { runnerIdentity: () => ({ ...nativeIdentity(), native_sha256: "c".repeat(64) }) }
  const fixture = harness(behavior), record = await fixture.construct().run()
  assert.equal(record.status, "stopped")
  assert.equal(record.safe_to_continue, false)
  assert.equal(fixture.events.filter(({ event }) => event === "prepare").length, source === "prearm" ? 0 : 1)
  assert.equal(fixture.events.filter(({ event }) => event === "persistence").length, 0)
})

test("injected native identity proof accessors cannot execute or start an arm", async () => {
  let reads = 0
  const proof = nativeIdentity()
  Object.defineProperty(proof, "native_used_identity", { enumerable: true, get() { reads++; throw new Error(secret) } })
  const fixture = harness({ verifyIdentity: () => proof }), record = await fixture.construct().run()
  assert.equal(reads, 0)
  assert.equal(record.status, "stopped")
  assert.equal(record.stop_code, "COORDINATOR_NATIVE_IDENTITY_INVALID")
  assert.equal(fixture.arms.length, 0)
  assert.equal(fixture.native.creates.length, 0)
  assert.equal(fixture.observers.length, 0)
  assert.equal(fixture.runners.length, 0)
  assert.doesNotMatch(JSON.stringify(record), /PRIVATE_COMPARISON_SECRET/u)
})

test("private benchmark source accessors cannot execute during data-only capture", async () => {
  let reads = 0
  const fixture = harness({ mutateRunner: (result) => {
    Object.defineProperty(result, "status", { enumerable: true, get() { reads++; throw new Error(secret) } })
  } }), comparison = fixture.construct(), record = await comparison.run()
  assert.equal(reads, 0)
  assert.equal(record.status, "stopped")
  assert.equal(record.stop_code, "COORDINATOR_RUNNER_OUTCOME_INVALID")
  assert.equal(fixture.runners.length, 1)
  assert.equal(fixture.events.filter(({ event }) => event === "persistence").length, 0)
  assert.equal(fixture.events.filter(({ event }) => event === "prepare").length, 1)
  assert.equal(comparison.takePrivateEvidence().arms.length, 0)
  assert.doesNotMatch(JSON.stringify(record), /PRIVATE_COMPARISON_SECRET/u)
})

test("a corrupt persistence canary stops after runner cleanup and before B1", async () => {
  const fixture = harness({ corruptCanary: true }), record = await fixture.construct().run()
  assert.equal(record.status, "stopped")
  assert.equal(record.completed_arms, 0)
  assert.equal(fixture.runners.length, 1)
  assert.equal(fixture.arms[0].arm.snapshot().failure_code, "OWNED_LAYOUT_ARM_CANARY_READ_FAILED")
  assert.equal(fixture.events.filter(({ event }) => event === "prepare").length, 1)
  assert.equal(fixture.events.filter(({ event }) => event === "persistence").length, 1)
  assert.doesNotMatch(JSON.stringify(record), /PRIVATE_COMPARISON_SECRET/u)
})

test("four original floor qualifications remain qualified when final persistence fails", async () => {
  const behavior = { mutateRunner: (_result, index) => { if (index === 3) behavior.corruptCanary = true } }
  const fixture = harness(behavior), record = await fixture.construct().run()
  assert.equal(record.status, "stopped")
  assert.equal(record.completed_arms, 3)
  assert.equal(record.arms.length, 4)
  assert.equal(record.arms.every(({ outcome }) => outcome.floor_qualified === true), true)
  assert.equal(record.floor_qualified, true)
  assert.equal(record.complete, false)
  assert.equal(record.comparable, false)
  assert.equal(record.safe_to_continue, false)
  assert.equal(record.arms[3].persistence.failure_code, "OWNED_LAYOUT_ARM_CANARY_READ_FAILED")
})

test("prepare failure cannot start an observer or timed runner", async () => {
  const fixture = harness({ preflightError: true }), record = await fixture.construct().run()
  assert.equal(record.status, "stopped")
  assert.equal(record.completed_arms, 0)
  assert.equal(fixture.native.creates.length, 0)
  assert.equal(fixture.observers.length, 0)
  assert.equal(fixture.runners.length, 0)
  assert.equal(fixture.events.filter(({ event }) => event === "prepare").length, 1)
})

function mutateBackingIdentity(result, mutate) {
  const evidence = result.providers[0].backingObserver.backing_evidence
  const boundaries = evidence.journal.filter(({ type }) => type === "boundary")
  for (const boundary of boundaries) mutate(boundary.samples[0].identity)
  const elapsed = result.results[0].summary.elapsedMs
  const intervalIndex = evidence.journal.findIndex(({ type }) => type === "interval")
  evidence.journal[intervalIndex] = { type: "interval", ...summarizeInterval(evidence.allowlist, boundaries[0], boundaries[1], { workload_elapsed_ms: elapsed }),
    native_quiescent: true, owned_operations_settled: true, native_evidence_state: "complete" }
  evidence.journal_bytes = evidence.journal.reduce((sum, entry) => sum + Buffer.byteLength(JSON.stringify(entry)) + 1, 0)
}
function mutateBackingMeasurements(result, mutate) {
  const evidence = result.providers[0].backingObserver.backing_evidence
  const boundaries = evidence.journal.filter(({ type }) => type === "boundary")
  mutate(boundaries[0], boundaries[1])
  const elapsed = result.results[0].summary.elapsedMs
  const intervalIndex = evidence.journal.findIndex(({ type }) => type === "interval")
  evidence.journal[intervalIndex] = { type: "interval", ...summarizeInterval(evidence.allowlist, boundaries[0], boundaries[1], { workload_elapsed_ms: elapsed }),
    native_quiescent: true, owned_operations_settled: true, native_evidence_state: "complete" }
  evidence.journal_bytes = evidence.journal.reduce((sum, entry) => sum + Buffer.byteLength(JSON.stringify(entry)) + 1, 0)
}
function partialBlockMeasurements(before, after) {
  for (const boundary of [before, after]) {
    for (const sample of boundary.samples) sample.stats.block_operations = null
    boundary.samples[0].stats.block_bytes = null
  }
}

test("honest unavailable block metrics preserve a complete safe ABBA protocol and partial daemon accounting", async () => {
  const fixture = harness({ mutateRunner: (result) => mutateBackingMeasurements(result, partialBlockMeasurements) }), record = await fixture.construct().run()
  for (const result of fixture.results) assert.equal(assessOwnedLayoutRunnerOutcome(result).runner_safe_to_continue, true)
  assert.equal(record.status, "complete")
  assert.equal(record.complete, true)
  assert.equal(record.safe_to_continue, true)
  assert.equal(record.comparable, true)
  assert.equal(record.floor_qualified, true)
  assert.equal(record.completed_arms, 4)
  for (const arm of record.arms) {
    assert.equal(arm.outcome.floor_qualified, true)
    assert.equal(arm.backing.complete, true)
    assert.equal(arm.backing.interval.complete, false)
    assert.equal(arm.backing.interval.metrics.block_operations.total, null)
    assert.equal(arm.backing.interval.metrics.block_operations.partial_total, "0")
    assert.deepEqual(arm.backing.interval.metrics.block_operations.missing_members, fixtureReceipt().entries.map(({ cid }) => cid))
    assert.equal(arm.backing.interval.metrics.block_bytes.total, null)
    assert.equal(arm.backing.interval.metrics.block_bytes.partial_total, "14000000000")
    assert.deepEqual(arm.backing.interval.metrics.block_bytes.missing_members, [fixtureReceipt().entries[0].cid])
    for (const boundary of arm.backing.boundaries) {
      assert.equal(boundary.samples.every(({ stats }) => stats.block_operations === null), true)
      assert.equal(boundary.samples[0].stats.block_bytes, null)
    }
  }
})

test("nullable interface counters preserve their unavailable value while known interfaces increase", async () => {
  const fixture = harness({ mutateRunner: (result) => mutateBackingMeasurements(result, (before, after) => {
    for (const boundary of [before, after]) for (const sample of boundary.samples) sample.stats.network_rx_bytes = { a: null, b: sample.stats.read_ns }
  }) }), record = await fixture.construct().run()
  for (const result of fixture.results) assert.equal(assessOwnedLayoutRunnerOutcome(result).runner_safe_to_continue, true)
  assert.equal(record.status, "complete")
  assert.equal(record.comparable, true)
  assert.equal(record.safe_to_continue, true)
  assert.equal(record.floor_qualified, true)
  for (const arm of record.arms) {
    assert.equal(arm.backing.interval.complete, false)
    assert.equal(arm.backing.interval.metrics.network_rx_bytes.total, null)
    assert.deepEqual(arm.backing.interval.metrics.network_rx_bytes.missing_members, fixtureReceipt().entries.map(({ cid }) => cid))
    assert.deepEqual(arm.backing.boundaries[0].samples[0].stats.network_rx_bytes, { a: null, b: "1000000000" })
    assert.deepEqual(arm.backing.boundaries[1].samples[0].stats.network_rx_bytes, { a: null, b: "3000000000" })
  }
})

const partialMeasurementFaults = [
  ["known reset behind an earlier unavailable interface", (before, after) => {
    before.samples[0].stats.network_rx_bytes = { a: null, b: "10" }
    after.samples[0].stats.network_rx_bytes = { a: null, b: "9" }
  }],
  ["device key drift", (before, after) => {
    before.samples[1].stats.block_bytes = { "1:1:Read": "10" }
    after.samples[1].stats.block_bytes = { "1:2:Read": "20" }
  }],
  ["network interface key drift", (before, after) => {
    before.samples[0].stats.network_rx_bytes = { a: null, b: "10" }
    after.samples[0].stats.network_rx_bytes = { a: null, c: "20" }
  }],
  ["nonincreasing daemon timestamp", (before, after) => {
    after.samples[0].stats.read_ns = before.samples[0].stats.read_ns
    after.samples[0].stats.read = before.samples[0].stats.read
  }],
  ["missing daemon member", (_before, after) => { after.samples.pop() }],
  ["changed retained identity", (_before, after) => { after.samples[0].identity.image = `sha256:${"f".repeat(64)}` }],
]
for (const [name, mutate] of partialMeasurementFaults) test(`${name} stops despite other honestly unavailable daemon metrics`, async () => {
  const fixture = harness({ mutateRunner: (result) => mutateBackingMeasurements(result, (before, after) => {
    partialBlockMeasurements(before, after)
    mutate(before, after)
  }) }), record = await fixture.construct().run()
  assert.equal(assessOwnedLayoutRunnerOutcome(fixture.results[0]).runner_safe_to_continue, false)
  assert.equal(record.status, "stopped")
  assert.equal(record.safe_to_continue, false)
  assert.equal(record.comparable, false)
  assert.equal(record.completed_arms, 0)
  assert.equal(fixture.runners.length, 1)
  assert.equal(fixture.events.filter(({ event }) => event === "persistence").length, 0)
  assert.equal(fixture.events.filter(({ event }) => event === "prepare").length, 1)
})

for (const key of ["18446744073709551616:1:Read", "1:18446744073709551616:Read"]) test(`device component overflow ${key} is rejected even when counters increase`, async () => {
  const fixture = harness({ mutateRunner: (result) => mutateBackingMeasurements(result, (before, after) => {
    before.samples[0].stats.block_bytes = { [key]: "10" }
    after.samples[0].stats.block_bytes = { [key]: "20" }
  }) }), record = await fixture.construct().run()
  assert.equal(assessOwnedLayoutRunnerOutcome(fixture.results[0]).runner_safe_to_continue, false)
  assert.equal(record.status, "stopped")
  assert.equal(record.safe_to_continue, false)
  assert.equal(record.completed_arms, 0)
  assert.equal(fixture.runners.length, 1)
  assert.equal(fixture.events.filter(({ event }) => event === "persistence").length, 0)
})

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

test("actual public arms retain exact original SQL, RustFS raw API and local work counters", async () => {
  const fixture = harness({ mutateRunner: addMeasuredNativeCounters }), record = await fixture.construct().run()
  for (const result of fixture.results) {
    assert.equal(assessOwnedLayoutRunnerOutcome(result).runner_safe_to_continue, true)
    assert.equal(projectOwnedLayoutPhaseMetrics(result.providers[0].storageDiagnostics.phases[0]).status, "observed")
  }
  assert.equal(record.status, "complete")
  assert.equal(record.floor_qualified, true)
  for (const arm of record.arms) {
    const metrics = arm.native_metrics
    assert.equal(metrics?.status, "observed", "actual public arms must retain their reviewed phase metrics")
    assert.equal(metrics.phase, "workload-4096bytes")
    const sql = metrics.storage.entries.find(({ name }) => name === "tidb.sql.inode_read")
    assert.equal(sql.calls, "600")
    assert.equal(sql.returned_rows, "400")
    assert.equal(sql.returned_row_observations, "600")
    assert.equal(sql.elapsed_ns, "9007199254740993")
    assert.equal(sql.latency_log2_us[0], "600")
    const instance = metrics.rustfs.instances[0]
    assert.equal(instance.raw_api.entries[0].calls, "4")
    assert.equal(instance.raw_api.entries[0].confirmed_bytes, "16384")
    assert.equal(instance.raw_api.entries[0].latency_max_ns_end, "1000")
    assert.equal(instance.local_work.status, "observed")
    assert.equal(instance.local_work.entries[0].input_bytes, "16384")
    assert.equal(instance.local_work.entries[0].output_bytes, "128")
    deeplyFrozen(metrics)
  }
  assert.equal(fixture.native.creates.length, 12, "projection must not open additional native instances")
  assert.equal(fixture.observers.length, 4, "projection must not create additional observer factories")
})

test("absent optional native sections remain unavailable in actual public records", async () => {
  const fixture = harness(), record = await fixture.construct().run()
  assert.equal(record.status, "complete")
  for (const arm of record.arms) {
    const metrics = arm.native_metrics
    assert.equal(metrics?.status, "observed", "optional absence must not erase required native phase metrics")
    assert.equal(metrics.rustfs.instances[0].local_work.status, "unavailable")
    assert.equal(Object.hasOwn(metrics.rustfs.instances[0].local_work, "entries"), false)
    assert.equal(metrics.process.status, "unavailable")
    assert.equal(Object.hasOwn(metrics.process, "cpu_work"), false)
    assert.equal(metrics.core_profile.status, "unavailable")
    assert.equal(Object.hasOwn(metrics.core_profile, "entries"), false)
    assert.equal(metrics.forwarding_boxes.status, "unavailable")
    assert.equal(metrics.rustfs.instances[0].raw_api.entries[1].calls, "0", "observed zero backend GETs remain exact evidence")
    assert.equal(metrics.unavailable.physical_device_iops, true)
    assert.equal(metrics.unavailable.sql_wire_bytes, true)
  }
})

test("native projection omits arbitrary private fields while retaining the measured phase", async () => {
  const fixture = harness({ mutateRunner: (result) => {
    const phase = result.providers[0].storageDiagnostics.phases[0]
    addMeasuredNativeCounters(result)
    phase.native.private_url = secret
    phase.native.rustfs.instances[0].raw_api.private_error = secret
    phase.native.rustfs.instances[0].raw_api.entries[0].private_note = secret
    phase.native.rustfs.instances[0].local_work.entries[0].private_note = secret
  } }), record = await fixture.construct().run()
  assert.equal(record.status, "complete")
  for (const arm of record.arms) assert.equal(arm.native_metrics?.status, "observed", "public measured phase metrics are required")
  assert.doesNotMatch(JSON.stringify(record), /PRIVATE_COMPARISON_SECRET|private_url|private_error|private_note/u)
})

test("required native metrics exceeding the reviewed instance cap stop before persistence or another arm", async () => {
  const fixture = harness({ mutateRunner: (result) => {
    const registry = result.providers[0].storageDiagnostics.phases[0].native.rustfs, instance = registry.instances[0]
    registry.instances = Array.from({ length: 65 }, (_, index) => ({ ...structuredClone(instance), id: String(index + 1) }))
    registry.instance_ids_start = registry.instances.map(({ id }) => id)
    registry.instance_ids_end = [...registry.instance_ids_start]
  } }), record = await fixture.construct().run()
  const phase = fixture.results[0].providers[0].storageDiagnostics.phases[0]
  assert.equal(validateRawPhaseDiagnostics(phase, "rustfs").length, 65)
  assert.equal(assessOwnedLayoutRunnerOutcome(fixture.results[0]).runner_safe_to_continue, true)
  assert.equal(projectOwnedLayoutPhaseMetrics(phase).status, "unavailable")
  assert.equal(record.status, "stopped")
  assert.equal(record.stop_code, "COORDINATOR_NATIVE_METRICS_UNAVAILABLE")
  assert.equal(record.arms[0].native_metrics.status, "unavailable")
  assert.equal(record.arms[0].outcome.floor_qualified, true)
  assert.equal(record.arms[0].outcome.status, "ok")
  assert.equal(record.completed_arms, 0)
  assert.equal(fixture.runners.length, 1)
  assert.equal(fixture.events.filter(({ event }) => event === "persistence").length, 0)
  assert.equal(fixture.events.filter(({ event }) => event === "prepare").length, 1)
})
for (const [name, mutate] of [
  ["image", (value) => { value.image = `sha256:${"c".repeat(64)}` }],
  ["start time", (value) => { value.started_at = "2026-09-27T00:00:01Z" }],
  ["restart count", (value) => { value.restart_count = "1" }],
  ["limits", (value) => { value.limits.Memory = "1024" }],
]) test(`cross-arm ${name} drift stops B1 even when each original interval remains clean`, async () => {
  const fixture = harness({ mutateRunner: (result, index) => { if (index === 1) mutateBackingIdentity(result, mutate) } }), record = await fixture.construct().run()
  assert.equal(assessOwnedLayoutRunnerOutcome(fixture.results[1]).runner_safe_to_continue, true)
  assert.equal(record.status, "stopped")
  assert.equal(record.completed_arms, 1)
  assert.equal(fixture.runners.length, 2)
  assert.equal(fixture.events.filter(({ event }) => event === "persistence").length, 1)
  assert.equal(record.arms[1].fixture_identity_stable, false)
})

test("missing retained container limits are rejected even if both boundaries agree", async () => {
  const fixture = harness({ mutateRunner: (result) => mutateBackingIdentity(result, (value) => { delete value.limits }) }), record = await fixture.construct().run()
  assert.equal(assessOwnedLayoutRunnerOutcome(fixture.results[0]).runner_safe_to_continue, true)
  assert.equal(record.status, "stopped")
  assert.equal(fixture.runners.length, 1)
  assert.equal(fixture.events.filter(({ event }) => event === "persistence").length, 0)
})

test("valid original compact proof must join the prepared cohort's exact backing identity", async () => {
  const fixture = harness({ mutateRunner: (result, index) => {
    if (index === 1) result.providers[0].layoutSelection.persistedReceipt.backingId = "f".repeat(32)
  } }), record = await fixture.construct().run()
  assert.equal(assessOwnedLayoutRunnerOutcome(fixture.results[1]).runner_safe_to_continue, true)
  assert.equal(record.status, "stopped")
  assert.equal(record.completed_arms, 1)
  assert.equal(fixture.runners.length, 2)
  assert.equal(fixture.events.filter(({ event }) => event === "persistence").length, 1)
})

for (const [name, mutate] of [
  ["R2-only configuration", (value) => { delete value.environment.MOUNT_RS_RUSTFS_ENDPOINT; value.environment.R2_ENDPOINT = "http://127.0.0.1:19000" }],
  ["profiling disabled", (value) => { value.environment.MOUNT_RS_PROFILE_IO = "0" }],
  ["trace enabled", (value) => { value.environment.MOUNT_RS_TRACE_STORAGE = "1" }],
  ["missing effective TiDB URL", (value) => { delete value.environment.MOUNT_RS_TIDB_URL }],
  ["duplicate CID", (value) => { value.fixtures.entries[1].cid = value.fixtures.entries[0].cid }],
  ["missing role", (value) => { value.fixtures.entries.pop() }],
  ["extra role", (value) => value.fixtures.entries.push(structuredClone(value.fixtures.entries[0]))],
  ["stale fixture generation", (value) => { value.fixtures.generation = "2" }],
  ["fixture owner mismatch", (value) => { value.fixtures.tidb_owner = "foreign-owner" }],
  ["wrong owner label", (value) => { value.fixtures.entries[0].labels["mount-rs.tidb.run"] = "foreign-owner" }],
  ["mock identity", (value) => { value.identity.mock = true }],
  ["unmatched preflight digest", (value) => { value.identity.public.native_sha256 = "c".repeat(64) }],
  ["scope traversal", (value) => { value.scope.metadataPrefix = "comparison/../foreign" }],
  ["unsafe socket path", (value) => { value.engine.socketPath = "relative.sock" }],
]) test(`invalid ${name} is rejected before any dependency dispatch`, () => {
  const fixture = harness()
  mutate(fixture.options)
  assert.throws(fixture.construct, (error) => {
    assert.match(error.code, /^COORDINATOR_[A-Z_]+$/u)
    assert.doesNotMatch(error.message + JSON.stringify(error), /PRIVATE_COMPARISON_SECRET/u)
    return true
  })
  assert.equal(fixture.events.length, 0)
})

for (const value of [0, -1, 1.5, "1", NaN, Infinity, 245001]) test(`invalid prepare budget ${String(value)} is rejected`, () => {
  const fixture = harness({ budgets: { prepareMs: value } })
  const make = implementation()
  assert.throws(() => make(fixture.options, fixture.dependencies))
  assert.equal(fixture.events.length, 0)
})
for (const [key, ceiling] of [["runnerMs", 60000], ["persistenceMs", 310000]]) test(`${key} cannot raise its fixed ceiling`, () => {
  const fixture = harness({ budgets: { [key]: ceiling + 1 } })
  const make = implementation()
  assert.throws(() => make(fixture.options, fixture.dependencies))
  assert.equal(fixture.events.length, 0)
})

async function waitForStage(fixture) {
  for (let index = 0; index < 1000 && !fixture.behavior.entered; index++) await Promise.resolve()
  assert.equal(fixture.behavior.entered, true, "modeled owned call did not reach its held stage")
}
for (const stage of ["prepare", "runner", "persistence"]) {
  test(`${stage} elapsed deadline survives delayed timers and late successful settlement`, async () => {
    const fixture = harness({ stage, gate: deferred(), budgets: { prepareMs: 10, runnerMs: 10, persistenceMs: 10 } }), comparison = fixture.construct()
    const running = comparison.run()
    await waitForStage(fixture)
    fixture.clock.jump(11, false)
    fixture.behavior.gate.resolve()
    const record = await running
    assert.equal(record.status, "stopped")
    assert.equal(record.safe_to_continue, false)
    assert.equal(record.native_uncertainty, true)
    assert.equal(record.completed_arms, 0)
    assert.equal(fixture.events.filter(({ event }) => event === "prepare").length, 1)
    assert.equal(fixture.clock.timers.size, 0)
    assert.equal(record.stop_code, `COORDINATOR_${stage.toUpperCase()}_TIMEOUT`)
    if (stage !== "persistence") assert.equal(fixture.events.filter(({ event }) => event === "persistence").length, 0)
  })
  test(`${stage} pending deadline seals evidence and never resumes coordinator work after settlement`, async () => {
    const fixture = harness({ stage, gate: deferred(), budgets: { prepareMs: 10, runnerMs: 10, persistenceMs: 10 } }), comparison = fixture.construct()
    const running = comparison.run()
    await waitForStage(fixture)
    let ended = false
    running.then(() => { ended = true })
    fixture.clock.jump(11)
    await flush()
    assert.equal(ended, true, "deadline must finish without waiting for the native call")
    const record = await running, frozen = JSON.stringify(record), before = fixture.events.length
    assert.equal(record.status, "stopped")
    assert.equal(record.native_uncertainty, true)
    assert.ok(record.pending_owned_call_count > 0)
    assert.equal(record.safe_to_continue, false)
    assert.equal(fixture.events.filter(({ event }) => event === "prepare").length, 1)
    fixture.behavior.gate.resolve()
    await flush()
    assert.equal(fixture.events.length, before, "late settlement must not start cleanup, reopen or another cohort")
    assert.equal(JSON.stringify(record), frozen)
    assert.equal(comparison.snapshot().status, "stopped")
    assert.equal(comparison.snapshot().native_uncertainty, true)
    deeplyFrozen(record)
  })
}

test("concurrent and repeated run calls cannot dispatch a second ABBA sequence", async () => {
  const fixture = harness({ stage: "runner", gate: deferred() }), comparison = fixture.construct(), first = comparison.run()
  await waitForStage(fixture)
  const second = Promise.resolve().then(() => comparison.run()).catch(() => null)
  fixture.behavior.gate.resolve()
  await first
  await second
  try { await comparison.run() } catch (error) { assert.match(error.code, /^COORDINATOR_[A-Z_]+$/u) }
  assert.equal(fixture.runners.length, 4)
  assert.equal(fixture.events.filter(({ event }) => event === "prepare").length, 4)
})

test("final evidence is deeply frozen and excludes private configuration, paths, errors and live instances", async () => {
  const fixture = harness({ mutateRunner: (result) => {
    result.providers[0].backingObserver.backing_evidence.journal[0].private_note = secret
    const evidence = result.providers[0].backingObserver.backing_evidence
    evidence.journal_bytes = evidence.journal.reduce((sum, entry) => sum + Buffer.byteLength(JSON.stringify(entry)) + 1, 0)
    result.providers[0].storageDiagnostics.phases[0].private_note = secret
  } }), record = await fixture.construct().run(), serialized = JSON.stringify(record)
  deeplyFrozen(record)
  assert.doesNotMatch(serialized, /PRIVATE_COMPARISON_SECRET|mysql:\/\/|socketPath|metadataKey|blockPrefix|filesystem|foreign\.invalid/u)
  for (const arm of record.arms) assert.equal(arm.backing.original_schema, "mount-rs.backing-observer.v1", "retain the original factory schema inside the closed backing projection")
  assert.ok(serialized.includes(digest), "retain digest identity")
  for (const result of fixture.results) result.providers[0].backingObserver.backing_evidence.journal.length = 0
  assert.equal(JSON.stringify(record), serialized, "the artifact must capture evidence instead of retaining mutable runner objects")
})

test("deferred private evidence is inaccessible before termination and can be consumed only once", async () => {
  const fixture = harness({ stage: "runner", gate: deferred(), floor: () => false }), comparison = fixture.construct()
  assert.equal(typeof comparison.takePrivateEvidence, "function")
  assert.equal(Object.keys(comparison).includes("takePrivateEvidence"), false)
  assert.throws(() => comparison.takePrivateEvidence(), { code: "COORDINATOR_SINGLE_USE" })
  const running = comparison.run()
  await waitForStage(fixture)
  assert.throws(() => comparison.takePrivateEvidence(), { code: "COORDINATOR_SINGLE_USE" })
  fixture.behavior.gate.resolve()
  const record = await running, rawInputs = fixture.results.map((result) => JSON.stringify(result)), evidence = comparison.takePrivateEvidence()
  assert.equal(record.status, "complete")
  assert.equal(evidence.schema, "mount-rs.owned-layout-private-evidence.v1")
  assert.equal(evidence.complete, true)
  assert.deepEqual(evidence.arms.map(({ role }) => role), order)
  deeplyFrozen(evidence)
  for (const [index, arm] of evidence.arms.entries()) {
    assert.equal(JSON.stringify(arm.benchmark), rawInputs[index], "private evidence must retain the complete original runner input")
    assert.notEqual(arm.benchmark, fixture.results[index])
    assert.equal(arm.benchmark.status, "failed")
    assert.equal(arm.benchmark.results[0].failures[0].error.code, "IOPS_TARGET_NOT_MET")
  }
  const retained = JSON.stringify(evidence)
  fixture.results[0].status = "ok"
  fixture.results[0].providers[0].storageDiagnostics.phases.length = 0
  assert.equal(JSON.stringify(evidence), retained)
  assert.throws(() => comparison.takePrivateEvidence(), { code: "COORDINATOR_SINGLE_USE" })
  assert.doesNotMatch(JSON.stringify(record), /PRIVATE_COMPARISON_SECRET/u)
})

for (const [name, select, field] of [
  ["root environment", (fixture) => fixture.options, "environment"],
  ["environment credential", (fixture) => fixture.options.environment, "MOUNT_RS_RUSTFS_SECRET_ACCESS_KEY"],
  ["source provenance", (fixture) => fixture.options.identity.public.source_sha256, "binding.cjs"],
  ["fixture owner label", (fixture) => fixture.options.fixtures.entries[0].labels, "mount-rs.tidb.run"],
  ["injected verifier", (fixture) => fixture.dependencies, "verifyBinding"],
]) test(`${name} configuration accessor is rejected without executing its body`, () => {
  const fixture = harness()
  let reads = 0
  Object.defineProperty(select(fixture), field, { enumerable: true, configurable: true, get() { reads++; throw new Error(secret) } })
  assert.throws(fixture.construct, (error) => {
    assert.equal(error.code, "COORDINATOR_CONFIG_INVALID")
    assert.doesNotMatch(error.message + JSON.stringify(error), /PRIVATE_COMPARISON_SECRET/u)
    return true
  })
  assert.equal(reads, 0)
  assert.equal(fixture.events.length, 0)
})

test("required original runner input over the publication cap stops before persistence or B1", async () => {
  assert.equal(api.OUTPUT_CAP, 33_554_432)
  const fixture = harness({ mutateRunner: (result) => { result.results[0].rawSamples[0].path = secret + "p".repeat(api.OUTPUT_CAP) } }), comparison = fixture.construct()
  const record = await comparison.run()
  assert.equal(record.status, "stopped")
  assert.equal(record.stop_code, "COORDINATOR_PUBLICATION_INCOMPLETE")
  assert.equal(record.complete, false)
  assert.equal(record.comparable, false)
  assert.equal(record.safe_to_continue, false)
  assert.equal(record.completed_arms, 0)
  assert.equal(fixture.runners.length, 1)
  assert.equal(fixture.events.filter(({ event }) => event === "persistence").length, 0)
  assert.equal(fixture.events.filter(({ event }) => event === "prepare").length, 1)
  assert.ok(Buffer.byteLength(JSON.stringify(record)) < api.OUTPUT_CAP)
  assert.doesNotMatch(JSON.stringify(record), /PRIVATE_COMPARISON_SECRET/u)
})

const receiptRejections = [
  ["missing caps", (value) => { delete value.caps }],
  ["changed owner deadline cap", (value) => { value.caps.ownerTimeoutMs = 30000 }],
  ["changed request cap", (value) => { value.caps.maxRequests = 258 }],
  ["extra unrecognized cap", (value) => { value.caps.private_cap = secret }],
  ["owner lifetime above 60000ms", (value) => { value.cost.owner_lifetime_ms = 60001 }],
  ["requests above 257", (value) => { value.cost.requests = 258 }],
  ["request count inconsistent with two boundaries", (value) => { value.cost.requests = 32 }],
  ["header count inconsistent with successful requests", (value) => { value.cost.headers_received = 32 }],
  ["first-frame count inconsistent with sixteen captured stats", (value) => { value.cost.first_frames_received = 15 }],
  ["iterator retirement count inconsistent with sixteen captured stats", (value) => { value.cost.streamed_iterators_retired = 15 }],
  ["fractional request count", (value) => { value.cost.requests = 33.5 }],
  ["fractional response byte count", (value) => { value.cost.response_bytes = 24704.5 }],
  ["fractional peak in-flight count", (value) => { value.cost.peak_in_flight = 8.5 }],
  ["negative CPU count", (value) => { value.cost.cpu_user_us = -1 }],
  ["missing journal bytes", (value) => { delete value.journal_bytes }],
  ["fractional journal byte count", (value) => { value.journal_bytes = 1.5 }],
  ["journal bytes above the unchanged cap", (value) => { value.journal_bytes = observerCaps.maxJournalBytes + 1 }],
  ["journal byte count inconsistent with original entries", (value) => { value.journal_bytes++ }],
  ["missing negotiated API version", (value) => { delete value.api_version }],
  ["unrecognized negotiated API version", (value) => { value.api_version = "2.00" }],
  ["negotiated API version inconsistent with original version record", (value) => { value.api_version = "1.50" }],
  ["missing receipt retention scope", (value) => { delete value.retention }],
  ...["requests", "response_bytes", "wall_ms", "cpu_user_us", "cpu_system_us", "peak_in_flight", "headers_received",
    "first_frames_received", "streamed_iterators_retired", "consumed_trailing_bytes", "owner_lifetime_ms"].map((key) =>
    [`missing ${key} cost`, (value) => { delete value.cost[key] }]),
]
for (const [name, mutate] of receiptRejections) test(`original observer ${name} cannot be repaired by a fabricated complete projection`, async () => {
  const fixture = harness({ mutateRunner: (result, index) => { if (index === 0) mutate(result.providers[0].backingObserver.backing_evidence) } }), record = await fixture.construct().run()
  assert.equal(assessOwnedLayoutRunnerOutcome(fixture.results[0]).runner_safe_to_continue, true, "the existing outcome gate deliberately leaves this receipt dimension to the coordinator")
  assert.equal(record.status, "stopped")
  assert.equal(record.stop_code, "COORDINATOR_FIXTURE_IDENTITY_INVALID")
  assert.equal(record.complete, false)
  assert.equal(record.safe_to_continue, false)
  assert.equal(record.completed_arms, 0)
  assert.equal(fixture.runners.length, 1)
  assert.equal(fixture.events.filter(({ event }) => event === "persistence").length, 0)
  assert.equal(fixture.events.filter(({ event }) => event === "prepare").length, 1)
  assert.doesNotMatch(JSON.stringify(record), /PRIVATE_COMPARISON_SECRET/u)
})

for (const stage of ["verify", "provider-map"]) for (const fireTimer of [true, false]) {
  test(`${stage} settling after its enclosing timeout cannot dispatch subsequent owned work with timer ${fireTimer}`, async () => {
    const fixture = harness({ stage, gate: deferred(), budgets: { prepareMs: 10, runnerMs: 10 } }), comparison = fixture.construct(), running = comparison.run()
    await waitForStage(fixture)
    fixture.clock.jump(11, fireTimer)
    if (fireTimer) await flush()
    const before = fixture.events.length
    fixture.behavior.gate.resolve()
    const record = await running
    await flush()
    assert.equal(record.status, "stopped")
    assert.equal(record.stop_code, stage === "verify" ? "COORDINATOR_PREPARE_TIMEOUT" : "COORDINATOR_RUNNER_TIMEOUT")
    assert.equal(record.native_uncertainty, true)
    assert.equal(record.completed_arms, 0)
    assert.equal(fixture.events.length, before)
    assert.equal(fixture.events.filter(({ event }) => event === "arm-create").length, stage === "verify" ? 0 : 1)
    assert.equal(fixture.observers.length, 0)
    assert.equal(fixture.runners.length, 0)
    assert.equal(fixture.events.filter(({ event }) => event === "persistence").length, 0)
    assert.equal(fixture.events.filter(({ event }) => event === "coordinator-cleanup").length, 0)
  })
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
