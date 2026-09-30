import assert from "node:assert/strict"
import { Buffer } from "node:buffer"
import { createHash } from "node:crypto"
import { readFile } from "node:fs/promises"
import test from "node:test"

const helperURL = new URL("./owned-layout-handoff.mjs", import.meta.url)
let implemented = false, api
try { await readFile(helperURL); implemented = true } catch (error) { if (error.code !== "ENOENT") throw error }
if (implemented) api = await import(helperURL.href)
else {
  // Deliberately inert missing-feature fallback: semantic assertions fail,
  // rather than import failures or TypeErrors masquerading as RED evidence.
  const absent = () => Object.freeze({})
  api = Object.freeze({ buildOwnedLayoutHandoff: absent, buildOwnedLayoutNativeSeal: absent,
    assessOwnedLayoutEngineCapacity: absent, classifyOwnedLayoutResourceObservation: absent, buildOwnedLayoutOwnerTerminal: absent })
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
const PRODUCER_PATHS = Object.freeze(["scripts/owned-layout-handoff.mjs", "scripts/owned_layout_process.py", "scripts/test-tidb.sh", "scripts/test-rustfs.sh",
  "scripts/rustfs-bounded-docker.py", "scripts/rustfs-combo-runner.py", ".github/workflows/remote-drives.yml"])
const ROLES = ["pd-1", "pd-2", "pd-3", "tikv-1", "tikv-2", "tikv-3", "tidb", "rustfs-service"]
const secret = "PRIVATE_CONTROLLER_SECRET", sha = (bytes) => createHash("sha256").update(bytes).digest("hex")
const bytes = (value) => Buffer.from(JSON.stringify(value) + "\n")
const code = (suffix) => `OWNED_LAYOUT_HANDOFF_${suffix}`
const publicKeys = ["schema", "kind", "status", "hosted_qualified", "receipt_schema", "sha256", "bytes", "count", "complete", "native_uncertainty", "failure_code", "evidence_scope"]
function deepFrozen(value) {
  if (!value || typeof value !== "object") return
  assert.equal(Object.isFrozen(value), true)
  for (const child of Object.values(value)) deepFrozen(child)
}
function accepted(result, kind, receiptSchema, status = "ready") {
  assert.equal(result.status, status)
  assert.deepEqual(Object.keys(result).sort(), ["schema", "kind", "status", "private_json", "public"].sort())
  assert.equal(result.schema, "mount-rs.owned-layout-producer-result.v1"); assert.equal(result.kind, kind)
  assert.deepEqual(Object.keys(result.public).sort(), [...publicKeys].sort())
  assert.equal(result.public.schema, "mount-rs.owned-layout-producer-public.v1"); assert.equal(result.public.kind, kind)
  assert.equal(result.public.status, status); assert.equal(result.public.hosted_qualified, false)
  assert.equal(result.public.receipt_schema, receiptSchema)
  assert.equal(typeof result.private_json, "string"); assert.equal(result.public.sha256, sha(Buffer.from(result.private_json)))
  assert.equal(result.public.bytes, Buffer.byteLength(result.private_json))
  assert.equal(result.public.evidence_scope, "provided_boundary_observations; not_live_qualification")
  assert.doesNotMatch(JSON.stringify(result.public), /PRIVATE_CONTROLLER_SECRET|mysql:|127\.0\.0\.1|mount-rs-owned-layout\/|model-tidb|model-rustfs|\/private\//u)
  deepFrozen(result)
  return JSON.parse(result.private_json)
}
function rejected(result, suffix) {
  assert.equal(result.status, "rejected")
  assert.equal(result.public.failure_code, code(suffix)); assert.equal(result.public.hosted_qualified, false)
  assert.equal(result.private_json, null); assert.equal(result.public.sha256, null); assert.equal(result.public.bytes, null)
  assert.equal(result.public.complete, false); deepFrozen(result)
  assert.doesNotMatch(JSON.stringify(result.public), /PRIVATE_CONTROLLER_SECRET|mysql:|\/private\//u)
}
function handoff({ tidbOwner = "model-tidb", rustfsOwner = "model-rustfs" } = {}) {
  const fixture = { schema: "mount-rs.owned-backing-cids.v1", tidb_owner: tidbOwner, rustfs_owner: rustfsOwner,
    generation: "1", entries: ROLES.map((role, index) => ({ role, cid: (index + 1).toString(16).padStart(64, "0"),
      labels: role === "rustfs-service" ? { "com.mount-rs.rustfs-test": "mount-rs-rustfs-test", "com.mount-rs.rustfs-test-run": rustfsOwner } : { "mount-rs.tidb.run": tidbOwner } })) }
  return { fixtures: Buffer.from(`  ${JSON.stringify(fixture)}\n`), engine: Buffer.from('{ "socket_path" : "/var/run/docker.sock" }\n'),
    created: { tidb_owner: tidbOwner, rustfs_owner: rustfsOwner, tidb_url: `mysql://root:${secret}@127.0.0.1:24000/test`,
      rustfs_endpoint: "http://127.0.0.1:24001", rustfs_bucket: "model-bucket" }, inherited: {}, scope_nonce: Buffer.alloc(16, 0x25) }
}
function controllerValue(input) {
  const scopeId = `layout-${input.scope_nonce.toString("hex")}`
  return { schema: "mount-rs.owned-layout-controller.v1", generation: "1", tidb_owner: input.created.tidb_owner,
    rustfs_owner: input.created.rustfs_owner, fixture_sha256: sha(input.fixtures), engine_capability_sha256: sha(input.engine),
    tidb_url: input.created.tidb_url, rustfs_endpoint: input.created.rustfs_endpoint, rustfs_bucket: input.created.rustfs_bucket,
    scope: { id: scopeId, owner: input.created.tidb_owner, metadataPrefix: `mount-rs-owned-layout/${input.created.tidb_owner}/${scopeId}`,
      blockPrefix: `mount-rs-owned-layout/${input.created.rustfs_owner}/${scopeId}` } }
}
function nativeBuild() {
  return { checkout: { revision: "a".repeat(40), clean_before: true, clean_after: true },
    build: { locked: true, release: true, no_js: true, exit_code: 0 }, native: Buffer.from("MODEL_INERT_NATIVE_BYTES"),
    sources: Object.fromEntries(SOURCE_PATHS.map((path) => [path, Buffer.from(`MODEL_SOURCE ${path}\n`)])) }
}
function processReceipt(actionId = "outer.comparison", raw = 0, pid = 21000) {
  return bytes({ schema: "mount-rs.owned-layout-process.v1", action_id: actionId, pid, pgid: pid, raw_returncode: raw,
    normalized_exit: raw < 0 ? 128 - raw : raw, supervisor_signal: null, child_return_signal: raw < 0 ? -raw : null, wait_unavailable_observed: false,
    deadline_exceeded: false, group_leak_observed: false, group_probe_unavailable_observed: false,
    retirement_signal_unavailable_observed: false, forced_process_retirement: false, retirement_signals: [],
    child_reaped: true, group_state: "absent", sticky_failure: raw !== 0, failure_code: raw === 0 ? null : "OWNED_LAYOUT_PROCESS_CHILD_FAILED" })
}
function changedProcess(source, changes) { return bytes({ ...JSON.parse(source), ...changes }) }
function capacity() {
  return { engine: handoff().engine, observation: Buffer.from("10737418240 4\n"), action: processReceipt("engine.capacity") }
}
function action(phase, kind, resource, index) {
  const target = kind === "container" ? resource.cid : kind === "network" ? resource.id : resource.name
  const output = phase === "absence" ? Buffer.from(kind === "network" ? `Error response from daemon: network ${target} not found\n` :
    kind === "volume" ? `Error response from daemon: get ${target}: no such volume\n` : `Error response from daemon: No such container: ${target}\n`) : phase === "remove" ? Buffer.from(`${target}\n`) : bytes(kind === "container" ? { id: resource.cid, name: `/${resource.name}`, labels: resource.labels } : kind === "volume" ? { name: resource.name, labels: resource.labels, created_at: resource.created_at, driver: resource.driver, scope: resource.scope } : { id: resource.id, name: resource.name, labels: resource.labels })
  return { phase, action_kind: phase === "absence" ? "inspect_absence" : phase === "inspect" ? "inspect_owner" : "remove", output,
    process: processReceipt(`${kind}.${phase}`, phase === "absence" ? 1 : 0, 22000 + index),
    container_delete_mode: kind === "container" && phase === "remove" ? "force_owned_resource" : null }
}
function changeAction(input, resourceIndex, actionIndex, changes) {
  const row = input.observations[resourceIndex].actions[actionIndex]
  row.process = changedProcess(row.process, changes); input.groups.actions[resourceIndex * 3 + actionIndex] = row.process
}
function terminal() {
  const h = handoff(), fixture = JSON.parse(h.fixtures), b = nativeBuild(), controller = controllerValue(h)
  const build = { schema: "mount-rs.owned-layout-native-build.v1", checkout_sha: b.checkout.revision, source_clean: true,
    locked: true, release: true, no_js: true, build_exit_code: 0, native_sha256: sha(b.native),
    source_sha256: Object.fromEntries(SOURCE_PATHS.map((path) => [path, sha(b.sources[path])])) }
  const resources = { schema: "mount-rs.owned-layout-resources.v1", generation: "1", tidb_owner: fixture.tidb_owner, rustfs_owner: fixture.rustfs_owner,
    containers: fixture.entries.map((entry) => ({ ...structuredClone(entry), name: entry.role === "rustfs-service" ? fixture.rustfs_owner : `mount-rs-tidb-${fixture.tidb_owner}-${entry.role.replace("-", "")}`, creation_state: "observed" })), volumes: ROLES.slice(0, 6).map((role) => ({ role, creation_state: "observed",
      name: `mount-rs-tidb-${fixture.tidb_owner}-${role.replace("-", "")}-data`, created_at: "2026-09-27T00:00:00Z", driver: "local", scope: "local", labels: { "mount-rs.tidb.run": fixture.tidb_owner } })),
    network: { id: "9".repeat(64), creation_state: "observed", name: `mount-rs-tidb-net-${fixture.tidb_owner}`, labels: { "mount-rs.tidb.run": fixture.tidb_owner } },
    rustfs_bind: { owner: fixture.rustfs_owner, marker_verified: true, path_validated: true,
      data_path: "/private/model/mount-rs-rustfs.abcdef/data", data_empty: true, run_dir_removed: true }, helper: null }
  const targets = [...resources.containers.map((resource) => ({ kind: "container", id: resource.cid, resource,
    owner: resource.role === "rustfs-service" ? fixture.rustfs_owner : fixture.tidb_owner })),
    ...resources.volumes.map((resource) => ({ kind: "volume", id: resource.name, resource, owner: fixture.tidb_owner })),
    { kind: "network", id: resources.network.id, resource: resources.network, owner: fixture.tidb_owner }]
  const observations = targets.map(({ kind, id, resource, owner }, index) => ({ kind, id, owner,
    before_owner_verified: true, after_absent: true,
    actions: ["inspect", "remove", "absence"].map((phase, actionIndex) => action(phase, kind, resource, index * 3 + actionIndex)) }))
  const artifacts = { fixtures: h.fixtures, engine: h.engine, controller: bytes(controller), build: bytes(build), native: b.native,
    capacity: bytes({ schema: "mount-rs.owned-layout-engine-capacity.v1", engine_capability_sha256: sha(h.engine), memory_bytes: "10737418240",
      cpu_count: 4, min_memory_bytes: "10737418240", min_cpu_count: 4, qualified: true, observation_sha256: sha(capacity().observation), observation_base64: capacity().observation.toString("base64"),
      action: JSON.parse(capacity().action), action_sha256: sha(capacity().action), action_base64: capacity().action.toString("base64") }),
    projection: bytes({ schema: "mount-rs.owned-layout-entry.v1", runtime_scope: "modeled_controls",
      comparison: { complete: true, safe_to_continue: true, native_uncertainty: false } }),
    originals: bytes({ schema: "mount-rs.owned-layout-originals.v1", runtime_scope: "modeled_controls", evidence: { complete: true, arms: [] } }) }
  const producer_sources = Object.fromEntries(PRODUCER_PATHS.map((path) => [path, Buffer.from(`MODEL_PRODUCER ${path}\n`)]))
  const producer_receipt = bytes({ schema: "mount-rs.owned-layout-producer-sources.v1", checkout_sha: b.checkout.revision,
    source_sha256: Object.fromEntries(PRODUCER_PATHS.map((path) => [path, sha(producer_sources[path])])) })
  return { artifacts, resources, observations, producer_sources, producer_receipt,
    groups: { outer: processReceipt(), actions: observations.flatMap((row) => row.actions.map((entry) => entry.process)) } }
}

test("fixtures model exact8roles/28seams/15ownedresources and make no live claim", () => {
  assert.equal(SOURCE_PATHS.length, 28); assert.equal(new Set(SOURCE_PATHS).size, 28)
  assert.deepEqual(JSON.parse(handoff().fixtures).entries.map((row) => row.role), ROLES)
  assert.equal(terminal().observations.length, 15); assert.equal(Object.keys(JSON.parse(terminal().groups.outer)).length, 19)
  assert.equal(Object.keys(terminal().producer_sources).length, 7)
  assert.equal(JSON.parse(terminal().artifacts.projection).runtime_scope, "modeled_controls")
  assert.notEqual(sha(handoff().fixtures), sha(bytes(JSON.parse(handoff().fixtures))))
})
test("controller preserves exact byte digests, actual endpoint spelling,32hexscope and public privacy", () => {
  const input = handoff(), result = api.buildOwnedLayoutHandoff(input)
  const receipt = accepted(result, "controller", "mount-rs.owned-layout-controller.v1")
  assert.deepEqual(receipt, controllerValue(input)); assert.equal(receipt.rustfs_endpoint, "http://127.0.0.1:24001")
  assert.equal(result.public.count, 8); assert.equal(result.public.complete, true)
  assert.match(result.private_json, /PRIVATE_CONTROLLER_SECRET/u)
})
test("controller accepts the exact leadingunderscore owner predicate", () => {
  const input = handoff({ tidbOwner: "_tidb", rustfsOwner: "_rustfs" })
  const receipt = accepted(api.buildOwnedLayoutHandoff(input), "controller", "mount-rs.owned-layout-controller.v1")
  assert.equal(receipt.tidb_owner, "_tidb"); assert.equal(receipt.scope.owner, "_tidb")
})
for (const [key, value] of [["MOUNT_RS_TIDB_URL", "mysql://root@127.0.0.1:24002/test"],
  ["TIDB_URL", "mysql://root@127.0.0.1:24003/test"], ["MOUNT_RS_RUSTFS_ENDPOINT", "http://127.0.0.1:24002"],
  ["MOUNT_RS_RUSTFS_BUCKET", "other-bucket"]]) test(`controller rejects inherited ${key} mismatch before assignment`, () => {
  const input = handoff(); input.inherited[key] = value; rejected(api.buildOwnedLayoutHandoff(input), "ENDPOINT_MISMATCH")
})
for (const [name, mutate, suffix] of [
  ["generation2", (input) => { const v = JSON.parse(input.fixtures); v.generation = "2"; input.fixtures = bytes(v) }, "CONTROLLER_INVALID"],
  ["duplicateCID", (input) => { const v = JSON.parse(input.fixtures); v.entries[1].cid = v.entries[0].cid; input.fixtures = bytes(v) }, "CONTROLLER_INVALID"],
  ["foreign ownerlabel", (input) => { const v = JSON.parse(input.fixtures); v.entries[0].labels["mount-rs.tidb.run"] = "foreign"; input.fixtures = bytes(v) }, "CONTROLLER_INVALID"],
  ["owner over120", (input) => { input.created.tidb_owner = "x".repeat(121) }, "CONTROLLER_INVALID"],
  ["nonliteral Engine", (input) => { input.engine = bytes({ socket_path: "/private/docker.sock" }) }, "ENGINE_INVALID"],
  ["NUL endpoint", (input) => { input.created.rustfs_endpoint += "\0" }, "CONTROLLER_INVALID"],
  ["nonloopback endpoint", (input) => { input.created.rustfs_endpoint = "http://example.invalid:24001" }, "CONTROLLER_INVALID"],
  ["wrong nonce length", (input) => { input.scope_nonce = Buffer.alloc(15) }, "SCOPE_INVALID"],
  ["duplicate decodedJSONkey", (input) => { input.engine = Buffer.from('{"socket_path":"/var/run/docker.sock","socket\\u005fpath":"/var/run/docker.sock"}') }, "JSON_INVALID"],
  ["fixture cap16384", (input) => { input.fixtures = Buffer.alloc(16385, 32) }, "INPUT_CAP"],
]) test(`controller rejects ${name} with fixed closed failure`, () => {
  const input = handoff(); mutate(input); rejected(api.buildOwnedLayoutHandoff(input), suffix)
})
test("controller rejects a getter without reading it or exposing its error", () => {
  const input = handoff(); let reads = 0
  Object.defineProperty(input, "created", { enumerable: true, get() { reads++; throw Error(secret) } })
  rejected(api.buildOwnedLayoutHandoff(input), "INPUT_INVALID"); assert.equal(reads, 0)
})
test("controller rejects a proxy before any trap", () => {
  let reads = 0
  const input = new Proxy(handoff(), { ownKeys() { reads++; throw Error(secret) }, get() { reads++; throw Error(secret) }, getPrototypeOf() { reads++; throw Error(secret) } })
  rejected(api.buildOwnedLayoutHandoff(input), "INPUT_INVALID"); assert.equal(reads, 0)
})
test("native seal joins actual native and exact28source bytes without loading native or claiming fullclosure", () => {
  const input = nativeBuild(), result = api.buildOwnedLayoutNativeSeal(input)
  const receipt = accepted(result, "native_build", "mount-rs.owned-layout-native-build.v1")
  assert.deepEqual(Object.keys(receipt).sort(), ["schema", "checkout_sha", "source_clean", "locked", "release", "no_js", "build_exit_code", "native_sha256", "source_sha256"].sort())
  assert.equal(receipt.checkout_sha, input.checkout.revision); assert.equal(receipt.native_sha256, sha(input.native))
  assert.deepEqual(receipt.source_sha256, Object.fromEntries(SOURCE_PATHS.map((path) => [path, sha(input.sources[path])])))
  assert.equal(result.public.count, 28); assert.equal(result.public.hosted_qualified, false)
})
for (const [name, mutate, suffix] of [
  ["failed actual build", (input) => { input.build.exit_code = 1 }, "BUILD_INVALID"],
  ["dirty afterbuild", (input) => { input.checkout.clean_after = false }, "BUILD_INVALID"],
  ["unlocked build", (input) => { input.build.locked = false }, "BUILD_INVALID"],
  ["missing seam", (input) => { delete input.sources["Cargo.toml"] }, "SOURCE_INVALID"],
  ["additional producer outside28", (input) => { input.sources["scripts/owned-layout-handoff.mjs"] = Buffer.from("inert") }, "SOURCE_INVALID"],
  ["source cap2MiB", (input) => { input.sources["Cargo.toml"] = Buffer.alloc(2097153) }, "INPUT_CAP"],
]) test(`native seal rejects ${name} instead of fabricating observedbuild`, () => {
  const input = nativeBuild(); mutate(input); rejected(api.buildOwnedLayoutNativeSeal(input), suffix)
})
test("Engine capacity preserves exact10GiB/fourCPUfloor and observed bytes", () => {
  const input = capacity(), result = api.assessOwnedLayoutEngineCapacity(input)
  const receipt = accepted(result, "engine_capacity", "mount-rs.owned-layout-engine-capacity.v1")
  assert.equal(receipt.engine_capability_sha256, sha(input.engine)); assert.equal(receipt.memory_bytes, "10737418240")
  assert.equal(receipt.cpu_count, 4); assert.equal(receipt.min_memory_bytes, "10737418240"); assert.equal(receipt.min_cpu_count, 4)
  assert.equal(receipt.qualified, true); assert.equal(result.public.hosted_qualified, false)
})
for (const [name, observation] of [["memory", "10737418239 4\n"], ["CPU", "10737418240 3\n"]]) test(`belowfloor ${name} remains observed incomplete without override`, () => {
  const input = capacity(); input.observation = Buffer.from(observation)
  const result = api.assessOwnedLayoutEngineCapacity(input), receipt = accepted(result, "engine_capacity", "mount-rs.owned-layout-engine-capacity.v1", "incomplete")
  assert.equal(result.public.failure_code, code("CAPACITY_FLOOR_NOT_MET")); assert.equal(result.public.complete, false)
  assert.equal(receipt.qualified, false); assert.notEqual(receipt.memory_bytes, null); assert.notEqual(receipt.cpu_count, null)
})
test("capacity rejects two rows instead of picking one convenient value", () => {
  const input = capacity(); input.observation = Buffer.from("10737418240 4\n1 1\n")
  rejected(api.assessOwnedLayoutEngineCapacity(input), "CAPACITY_INVALID")
})
test("capacity never normalizes an actiontimeout to a valid-looking info row", () => {
  const input = capacity(); input.action = changedProcess(input.action, { raw_returncode: -15, child_return_signal: 15, normalized_exit: 124, deadline_exceeded: true, forced_process_retirement: true, retirement_signals: [15], sticky_failure: true, failure_code: "OWNED_LAYOUT_PROCESS_TIMEOUT" })
  const result = api.assessOwnedLayoutEngineCapacity(input), receipt = accepted(result, "engine_capacity", "mount-rs.owned-layout-engine-capacity.v1", "incomplete")
  assert.equal(result.public.failure_code, code("CAPACITY_INVALID")); assert.equal(result.public.complete, false); assert.equal(receipt.qualified, false)
  assert.equal(receipt.action.normalized_exit, 124); assert.equal(receipt.action.deadline_exceeded, true)
})
test("terminal records actualbytejoins/8containers/6volumes/1network and expectedownerforce deletion without purgeclaim", () => {
  const input = terminal(), result = api.buildOwnedLayoutOwnerTerminal(input)
  const receipt = accepted(result, "owner_terminal", "mount-rs.owned-layout-owner-terminal.v1")
  assert.deepEqual(receipt.joins, Object.fromEntries(Object.entries({ ...input.artifacts, producer_source_receipt: input.producer_receipt }).map(([key, value]) => [`${key}_sha256`, sha(value)])))
  assert.equal(receipt.cleanup.kind, "owned_backing_resources_destroyed"); assert.equal(receipt.cleanup.namespace_purge, "not_performed")
  assert.equal(receipt.cleanup.container_count, 8); assert.equal(receipt.cleanup.tidb_volume_count, 6); assert.equal(receipt.cleanup.network_count, 1)
  assert.equal(receipt.cleanup.helper_state, "not_needed"); assert.equal(result.public.complete, true); assert.equal(result.public.count, 15)
})
for (const [name, mutate] of [
  ["removal124 before laterabsence", (input) => { changeAction(input, 0, 1, { raw_returncode: -15, child_return_signal: 15, normalized_exit: 124, deadline_exceeded: true, forced_process_retirement: true, retirement_signals: [15], sticky_failure: true, failure_code: "OWNED_LAYOUT_PROCESS_TIMEOUT" }) }],
  ["failedremoval before laterabsence", (input) => { changeAction(input, 0, 1, { raw_returncode: 1, normalized_exit: 1, sticky_failure: true, failure_code: "OWNED_LAYOUT_PROCESS_CHILD_FAILED" }) }],
  ["inspectfailure cannot meanabsence", (input) => { changeAction(input, 0, 0, { raw_returncode: 1, normalized_exit: 1, sticky_failure: true, failure_code: "OWNED_LAYOUT_PROCESS_CHILD_FAILED" }); input.observations[0].actions[0].output = Buffer.from("Engine unavailable\n") }],
  ["waitfailure before laterwait0/absence", (input) => { input.groups.outer = changedProcess(input.groups.outer, { wait_unavailable_observed: true, normalized_exit: 125, sticky_failure: true, failure_code: "OWNED_LAYOUT_PROCESS_WAIT_UNAVAILABLE" }) }],
  ["EPERMgroup unknown", (input) => { input.groups.outer = changedProcess(input.groups.outer, { group_probe_unavailable_observed: true, group_state: "unavailable", normalized_exit: 125, sticky_failure: true, failure_code: "OWNED_LAYOUT_PROCESS_GROUP_UNAVAILABLE" }) }],
  ["outer900 cutoff", (input) => { input.groups.outer = changedProcess(input.groups.outer, { deadline_exceeded: true, normalized_exit: 124, sticky_failure: true, failure_code: "OWNED_LAYOUT_PROCESS_TIMEOUT" }) }],
  ["forcedhostprocess retirement", (input) => { input.groups.outer = changedProcess(input.groups.outer, { forced_process_retirement: true, retirement_signals: [15, 9], group_leak_observed: true, normalized_exit: 125, sticky_failure: true, failure_code: "OWNED_LAYOUT_PROCESS_GROUP_LEAK" }) }],
  ["actualsupervisorsignal separate from childreturn", (input) => { input.groups.outer = changedProcess(input.groups.outer, { supervisor_signal: 15, normalized_exit: 143, sticky_failure: true, failure_code: "OWNED_LAYOUT_PROCESS_SUPERVISOR_SIGNAL" }) }],
  ["unreapedowned action", (input) => { changeAction(input, 0, 0, { child_reaped: false, group_state: "present", normalized_exit: 125, sticky_failure: true, failure_code: "OWNED_LAYOUT_PROCESS_UNREAPED" }) }],
]) test(`terminal keeps ${name} sticky although allresources laterabsent`, () => {
  const input = terminal(); mutate(input)
  const result = api.buildOwnedLayoutOwnerTerminal(input), receipt = accepted(result, "owner_terminal", "mount-rs.owned-layout-owner-terminal.v1", "incomplete")
  assert.equal(result.public.complete, false); assert.equal(result.public.failure_code, code("TERMINAL_INCOMPLETE"))
  assert.equal(receipt.cleanup.namespace_purge, "not_performed"); assert.equal(receipt.cleanup.complete, false)
  assert.ok(receipt.operation_error_count > 0 || receipt.native_uncertainty === true)
})
test("terminal rejects an observation for a foreignCID even with assertedownertrue", () => {
  const input = terminal(); input.observations[0].id = "f".repeat(64)
  rejected(api.buildOwnedLayoutOwnerTerminal(input), "TERMINAL_INVALID")
})
test("terminal refuses missing one of the sixowned volumes", () => {
  const input = terminal(); input.resources.volumes.pop()
  rejected(api.buildOwnedLayoutOwnerTerminal(input), "TERMINAL_INVALID")
})
test("unknown helpercreation stays incomplete without inventing an observedCID", () => {
  const input = terminal(); input.resources.helper = { creation_state: "unknown", cid: null, name: "model-rustfs-cleanup",
    labels: { "com.mount-rs.rustfs-test": "mount-rs-rustfs-test", "com.mount-rs.rustfs-test-run": "model-rustfs", "com.mount-rs.rustfs-test-purpose": "cleanup" } }
  const result = api.buildOwnedLayoutOwnerTerminal(input), receipt = accepted(result, "owner_terminal", "mount-rs.owned-layout-owner-terminal.v1", "incomplete")
  assert.equal(result.public.complete, false); assert.equal(result.public.failure_code, code("TERMINAL_INCOMPLETE"))
  assert.equal(receipt.cleanup.helper_state, "creation_unknown"); assert.equal(receipt.native_uncertainty, true)
})

for (const prefix of ["", "\n"]) test(`exacttarget notfound${prefix ? " with emptytemplate line" : ""} preserves rawstatus1/processCHILD_FAILED`, () => {
  const model = terminal(), target = model.resources.containers[0], source = model.observations[0].actions[2]
  source.output = Buffer.concat([Buffer.from(prefix), source.output])
  const receipt = accepted(api.classifyOwnedLayoutResourceObservation({ kind: "container", target,
    action_kind: "inspect_absence", output: source.output, process: source.process }), "resource_observation", "mount-rs.owned-layout-resource-observation.v1")
  assert.equal(receipt.typed_observation, "absent"); assert.equal(receipt.raw_command_returncode, 1)
  assert.equal(receipt.process.failure_code, "OWNED_LAYOUT_PROCESS_CHILD_FAILED"); assert.equal(receipt.process.sticky_failure, true)
  assert.equal(receipt.output_sha256, sha(source.output)); assert.equal(receipt.absence_basis, "exact_target_not_found")
  assert.deepEqual(Buffer.from(receipt.output_base64, "base64"), source.output)
  assert.deepEqual(Buffer.from(receipt.process_base64, "base64"), source.process); assert.equal(receipt.process_sha256, sha(source.process))
})
test("nonzero unknowninspect output remains unknown rather than matching an absent substring", () => {
  const model = terminal(), target = model.resources.containers[0], source = model.observations[0].actions[2]
  const output = Buffer.from(`Engine failure: No such container: ${target.cid}\n`)
  const result = api.classifyOwnedLayoutResourceObservation({ kind: "container", target,
    action_kind: "inspect_absence", output, process: source.process })
  const receipt = accepted(result, "resource_observation", "mount-rs.owned-layout-resource-observation.v1", "incomplete")
  assert.equal(receipt.typed_observation, "unknown"); assert.equal(receipt.raw_command_returncode, 1)
  assert.equal(receipt.absence_basis, "none"); assert.equal(receipt.output_sha256, sha(output))
})
test("exacttarget notfound cannot exempt a resource action waitfailure before later reap/absence", () => {
  const model = terminal(), target = model.resources.containers[0], source = model.observations[0].actions[2]
  const process = changedProcess(source.process, { wait_unavailable_observed: true, normalized_exit: 125,
    sticky_failure: true, failure_code: "OWNED_LAYOUT_PROCESS_WAIT_UNAVAILABLE" })
  const result = api.classifyOwnedLayoutResourceObservation({ kind: "container", target,
    action_kind: "inspect_absence", output: source.output, process })
  const receipt = accepted(result, "resource_observation", "mount-rs.owned-layout-resource-observation.v1", "incomplete")
  assert.equal(receipt.typed_observation, "unknown"); assert.equal(receipt.absence_basis, "none")
  assert.equal(receipt.raw_command_returncode, 1); assert.equal(receipt.process.wait_unavailable_observed, true)
  assert.equal(receipt.process.failure_code, "OWNED_LAYOUT_PROCESS_WAIT_UNAVAILABLE")
  assert.equal(receipt.process.child_reaped, true); assert.equal(receipt.process.group_state, "absent")
})
test("terminal source receipt uses separately retained actual7producerbytes outside28native seams", () => {
  const input = terminal(); input.producer_sources[PRODUCER_PATHS[0]] = Buffer.from("changed producer source")
  rejected(api.buildOwnedLayoutOwnerTerminal(input), "SOURCE_INVALID")
})
test("volume identity requires observedCreatedAt/driver/scope and never a fabricatedCID", () => {
  const input = terminal(); delete input.resources.volumes[0].created_at
  rejected(api.buildOwnedLayoutOwnerTerminal(input), "TERMINAL_INVALID")
})
test("earlycapacity failure retains null artifactjoins and uncreated intents without fake8CIDs", () => {
  const input = terminal()
  for (const key of ["projection", "originals", "fixtures", "controller"]) input.artifacts[key] = null
  input.resources.tidb_owner = null; input.resources.rustfs_owner = null
  for (const entry of input.resources.containers) Object.assign(entry, { cid: null, name: null, labels: null, creation_state: "not_created" })
  for (const entry of input.resources.volumes) Object.assign(entry, { name: null, labels: null, created_at: null, driver: null, scope: null, creation_state: "not_created" })
  Object.assign(input.resources.network, { id: null, name: null, labels: null, creation_state: "not_created" })
  const capacityRecord = JSON.parse(input.artifacts.capacity); capacityRecord.cpu_count = 3; capacityRecord.qualified = false; capacityRecord.observation_sha256 = sha(Buffer.from("10737418240 3\n")); capacityRecord.observation_base64 = Buffer.from("10737418240 3\n").toString("base64"); input.artifacts.capacity = bytes(capacityRecord)
  input.resources.rustfs_bind = null; input.observations = []; input.groups = { outer: null, actions: [] }
  const result = api.buildOwnedLayoutOwnerTerminal(input), receipt = accepted(result, "owner_terminal", "mount-rs.owned-layout-owner-terminal.v1", "incomplete")
  assert.equal(receipt.joins.projection_sha256, null); assert.equal(receipt.joins.originals_sha256, null)
  assert.equal(receipt.joins.fixtures_sha256, null); assert.equal(receipt.joins.controller_sha256, null)
  assert.equal(result.public.complete, false); assert.equal(result.public.count, 0)
})
test("terminal rejects processPGID not bound to the actualownedPID", () => {
  const input = terminal(); input.groups.outer = changedProcess(input.groups.outer, { pgid: 99999 })
  rejected(api.buildOwnedLayoutOwnerTerminal(input), "TERMINAL_INVALID")
})
