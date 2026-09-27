import assert from "node:assert/strict"
import { execFile } from "node:child_process"
import { chmod, mkdtemp, readFile, rm, symlink, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import test from "node:test"
import { fileURLToPath } from "node:url"
import { promisify } from "node:util"
import { summarizeInterval } from "../benchmarks/storage/backing-observer.mjs"
import { STORAGE_OPERATION_NAMES } from "../benchmarks/storage/diagnostics.mjs"
import { verifyPilot, readEvidence, SOURCE_PATHS } from "./verify-owned-backing-pilot.mjs"


const profileNames = [
  "wire.json_encode_bytes",
  "wire.json_decode_bytes",
  "catalog.load",
  "catalog.queue_wait",
  "catalog.pool_wait",
  "catalog.backing_verify",
  "catalog.connect_configure",
  "catalog.query_document_bytes",
  "catalog.decode_validate_bytes",
  "catalog.close",
  "catalog.pager_hits",
  "catalog.pager_misses",
  "catalog.pager_writes",
  "catalog.pager_unavailable",
  "service.dispatch",
  "service.authorization",
  "service.handle_lock_wait",
  "service.audit",
  "filesystem.gate_wait",
  "filesystem.mutation_batch_attempted_requests",
  "filesystem.snapshot_nodes",
  "filesystem.metadata_refresh",
  "filesystem.changed_namespace_nodes",
  "filesystem.write_fallback",
  "filesystem.old_chunk_read_bytes",
  "provider.metadata.load",
  "provider.metadata.load_if_changed",
  "provider.blocks.get_bytes",
  "provider.blocks.put_bytes",
  "provider.blocks.flush",
  "provider.blocks.verify_authority",
  "provider.metadata.publish_cas_nodes",
  "provider.metadata.cas_conflict",
  "provider.namespace_returned_bytes",
  "provider.namespace_serialized_bytes",
  "provider.inode.snapshot_if_changed",
  "provider.inode.snapshot_returned_nodes",
  "provider.inode.snapshot_unchanged",
  "filesystem.inode_path_guard",
  "provider.inode.load",
  "provider.inode.load_if_changed",
  "provider.inode.publish_cas",
  "provider.inode.cas_conflict",
  "provider.compact_anchor_returned_bytes",
  "provider.compact_anchor_serialized_bytes",
  "provider.inode_returned_bytes",
  "provider.inode_serialized_bytes"
]
const roles = ["pd-1", "pd-2", "pd-3", "tikv-1", "tikv-2", "tikv-3", "tidb", "rustfs-service"]
const rawNames = ["put_opts.block_create", "get.block_read", "body_read.block_read", "get.conflict_verify", "body_read.conflict_verify", "get.migration", "body_read.migration", "head.direct_delete", "delete.direct", "delete.reconcile"]
const claims = ["leader_claims", "leader_success", "leader_error", "leader_cancelled", "follower_claims", "follower_success", "follower_error", "follower_cancelled"]
const decimals = (keys, value = "0") => Object.fromEntries(keys.map((key) => [key, value]))
const terminal = () => ({ status: "ok", native_quiescent: true, owned_operations_settled: true, native_profiling_enabled: true,
  workload_native_evidence_complete: true, operation_deadline_failed: false, cleanup_complete: true, prior_native_uncertainty: false, safe_to_continue_pair: true })
function nativeWorkload(elapsed) {
  const row = (name, keys) => ({ name, ...decimals(keys), calls: "1", success: "1", elapsed_ns: "1000", latency_log2_us: ["1", ...Array(31).fill("0")] })
  return { name: "workload-4096bytes", status: "observed", validated_workload: true, projected_component_complete: true, quiescent: true,
    elapsed_ms: elapsed + 1, benchmark_measured_elapsed_ms: elapsed, observer_snapshot_ms: 1,
    process: { cpu_work: decimals(["user_us", "system_us"], "100"), cpu_observer: decimals(["user_us", "system_us"]),
      memory_end_bytes: decimals(["rss", "heapTotal", "heapUsed", "external", "arrayBuffers"], "1000"),
      resources_work: decimals(["voluntary_context_switches", "involuntary_context_switches"]) },
    core_profile: { status: "observed", entries: profileNames.map((name) => ({ name, calls: "1", elapsed_ns: "1000", units: "1" })) },
    storage: STORAGE_OPERATION_NAMES.map((name) => row(name, ["calls", "success", "error", "cancelled", "bytes", "returned_rows", "returned_row_observations", "elapsed_ns", "in_flight_start", "in_flight_end"])),
    raw_projection: { status: "observed", instance_count: 1, omitted_instances: 0 },
    raw_instances: [{ id: "1", claims: decimals(claims), cache_hits: "400", conditional_conflicts: "0",
      entries: rawNames.map((name) => ({ ...row(name, ["calls", "success", "error", "cancelled", "elapsed_ns", "attempted_bytes", "confirmed_bytes", "returned_bytes", "latency_max_ns_start", "latency_max_ns_end"]), exact_phase_max_ns: "unavailable" })) }] }
}
function backingModel(elapsed) {
  const allowlist = roles.map((role, index) => ({ role, cid: String(index + 1).repeat(64), labels: { "mount-rs.tidb.run": "modeled-owner" } }))
  const boundary = (id, read) => ({ type: "boundary", id, complete: true, issues: [], samples: allowlist.map((entry) => ({
    cid: entry.cid, identity: { ...entry, image: `sha256:${"d".repeat(64)}`, running: true, started_at: "2026-09-27T00:00:00Z", restart_count: "0" },
    inspect_body_sha256: "e".repeat(64), stats_body_sha256: "f".repeat(64),
    inspect_window: { dispatch_ms: 0, response_ms: 1 }, stats_window: { dispatch_ms: 1, response_ms: 2 },
    stats: { cid: entry.cid, read_ns: read, cpu_usage_ns: read, block_bytes: { "1:1:Read": read }, block_operations: { "1:1:Read": read },
      network_rx_bytes: { eth0: read }, network_tx_bytes: { eth0: read } },
  })) })
  const begin = boundary("workload-4096bytes:begin", "1000000000"), end = boundary("workload-4096bytes:end", "3000000000")
  const interval = { type: "interval", ...summarizeInterval(allowlist, begin, end, { workload_elapsed_ms: elapsed }),
    native_quiescent: true, owned_operations_settled: true, native_evidence_state: "complete" }
  return { schema: "mount-rs.runner-backing-observer.v1", complete: true, terminal: terminal(), issues: [],
    events: ["beginPhase", "endPhase", "finalize"].map((hook) => ({ hook, status: "ok" })),
    backing_evidence: { schema: "mount-rs.backing-observer.v1", complete: true, terminal: terminal(), issues: [], dropped_entries: 0,
      allowlist, journal: [begin, end, interval] } }
}

function model() {
  const digest = "a".repeat(64)
  const sources = Object.fromEntries(SOURCE_PATHS.map((path) => [path, "b".repeat(64)]))
  const build = { schema: "mount-rs.hosted-native-build.v1", checkout_sha: "c".repeat(40), source_clean: true,
    locked: true, release: true, no_js: true, build_exit_code: 0, native_sha256: digest, source_sha256: sources }
  const pilot = { schema: "mount-rs.owned-backing-pilot.v1", provider: "mount-rs-split-tidb-r2",
    identity: { kind: "selected_native_file", native_sha256: digest, source_sha256: { ...sources } },
    native_used_identity: "verified", owned_endpoint_binding: "verified", observation_status: "observed", live_measurement_qualified: true,
    config: { layout: "legacy", workload: "lifecycle", payload_bytes: 4096, iterations: 400, concurrency: 64, chunk_bytes: 65536, minimum_iops: 1000 },
    outcome: { status: "ok", provider_status: "ok", result_status: "ok", first_failure: null, elapsed_ms: 1200000 / 1001, iops: 1001, native_quiescent: true, owned_operations_settled: true,
      iops_target: 1000, iops_target_met: true, successful_iterations: 400, failed_iterations: 0,
      successful_operations: 1200, attempted_operations: 1200, verified_reads: 400, timeout_count: 0,
      cleanup_failure_count: 0, remaining_paths: 0, path_cleanup_failures: 0, resource_cleanup_status: "ok" },
    resource_coverage: { "workload-4096bytes": "captured" },
    native_phases: { status: "observed", phases: [nativeWorkload(1200000 / 1001)] },
    backing: backingModel(1200000 / 1001) }

  return { pilot, build }
}

test("complete modeled native evidence joins the clean locked build", () => {
  const { pilot, build } = model()
  assert.deepEqual(verifyPilot(pilot, build), { schema: "mount-rs.hosted-backing-check.v1", status: "verified",
    checkout_sha: build.checkout_sha, native_sha256: build.native_sha256, verified_reads: 400, successful_operations: 1200, iops: 1001 })
})

for (const [name, mutate] of [
  ["array checkout SHA", ({ build }) => { build.checkout_sha = [build.checkout_sha] }],
  ["missing duration", ({ pilot }) => { delete pilot.outcome.elapsed_ms }],
  ["inconsistent throughput", ({ pilot }) => { pilot.outcome.iops = 2000 }],
  ["missing core counters", ({ pilot }) => { delete pilot.native_phases.phases[0].core_profile.entries }],
  ["missing storage counters", ({ pilot }) => { delete pilot.native_phases.phases[0].storage }],
  ["missing raw counters", ({ pilot }) => { delete pilot.native_phases.phases[0].raw_instances }],
  ["missing process counters", ({ pilot }) => { delete pilot.native_phases.phases[0].process }],
  ["missing backing samples", ({ pilot }) => { delete pilot.backing.backing_evidence.journal[0].samples }],
  ["missing interval metrics", ({ pilot }) => { delete pilot.backing.backing_evidence.journal[2].metrics }],
  ["incorrect interval totals", ({ pilot }) => { pilot.backing.backing_evidence.journal[2].metrics.block_operations.total = "0" }],
  ["unsettled outcome", ({ pilot }) => { pilot.outcome.owned_operations_settled = false }],
  ["nonquiescent outcome", ({ pilot }) => { pilot.outcome.native_quiescent = false }],
  ["contradictory factory terminal", ({ pilot }) => { pilot.backing.backing_evidence.terminal.cleanup_complete = false }],
  ["unobserved interval", ({ pilot }) => { pilot.backing.backing_evidence.journal[2].native_evidence_state = "unobserved" }],
  ["mock identity", ({ pilot }) => { pilot.identity.kind = "capture_js_mock" }],
  ["different addon", ({ build }) => { build.native_sha256 = "d".repeat(64) }],
  ["different source", ({ pilot }) => { pilot.identity.source_sha256[SOURCE_PATHS[0]] = "d".repeat(64) }],
  ["dirty build", ({ build }) => { build.source_clean = false }],
  ["unlocked build", ({ build }) => { build.locked = false }],
  ["failed build", ({ build }) => { build.build_exit_code = 1 }],
  ["unexpected receipt fields", ({ build }) => { build.private_path = "/EXCLUDED_SECRET" }],
  ["wrong backing schema", ({ pilot }) => { pilot.backing.schema = "unrecognized" }],
  ["incomplete interval", ({ pilot }) => { pilot.backing.backing_evidence.journal[2].complete = false }],
  ["duplicate workload", ({ pilot }) => { pilot.native_phases.phases.push(pilot.native_phases.phases[0]) }],
  ["incomplete native", ({ pilot }) => { pilot.native_phases.phases[0].projected_component_complete = false }],
  ["truncated instances", ({ pilot }) => { pilot.native_phases.phases[0].raw_projection.status = "truncated_unavailable" }],
  ["zero operations", ({ pilot }) => { pilot.outcome.successful_operations = 0 }],
  ["unclean shutdown", ({ pilot }) => { pilot.outcome.resource_cleanup_status = "failed" }],
]) test(`rejects ${name} despite a positive qualification flag`, () => {
  const values = model(); mutate(values)
  assert.throws(() => verifyPilot(values.pilot, values.build), (error) => error.code === "pilot_evidence_rejected" && !String(error).includes("EXCLUDED_SECRET"))
})

for (const [field, invalid] of Object.entries({ native_quiescent: false, owned_operations_settled: false,
  native_profiling_enabled: false, workload_native_evidence_complete: false, cleanup_complete: false,
  operation_deadline_failed: true, prior_native_uncertainty: true, status: "failed" })) {
  test(`rejects contradictory terminal ${field}`, () => {
    const { pilot, build } = model(); pilot.backing.terminal[field] = invalid
    assert.throws(() => verifyPilot(pilot, build), (error) => error.code === "pilot_evidence_rejected")
  })
}

test("a completed below-floor profile remains rejected without altering its original outcome", () => {
  const { pilot, build } = model()
  pilot.outcome.status = "failed"
  pilot.outcome.iops = 394
  pilot.outcome.iops_target_met = false
  pilot.outcome.first_failure = { code: "iops_target_not_met", operation: "iops-target" }
  const original = JSON.stringify(pilot)
  assert.throws(() => verifyPilot(pilot, build), (error) => error.code === "pilot_evidence_rejected")
  assert.equal(JSON.stringify(pilot), original)
})

test("evidence reads reject oversized files, symlinks and malformed JSON", async () => {
  const root = await mkdtemp(join(tmpdir(), "mount-rs-pilot-check-"))
  try {
    await chmod(root, 0o700)
    const good = join(root, "good.json")
    await writeFile(good, '{"counter":1}\n', { mode: 0o600 })
    assert.deepEqual(await readEvidence(good, 32), { counter: 1 })
    const link = join(root, "link.json"); await symlink(good, link)
    await assert.rejects(readEvidence(link, 32), (error) => error.code === "pilot_evidence_rejected")
    await assert.rejects(readEvidence(good, 4), (error) => error.code === "pilot_evidence_rejected")
    const bad = join(root, "bad.json"); await writeFile(bad, "{", { mode: 0o600 })
    await assert.rejects(readEvidence(bad, 32), (error) => error.code === "pilot_evidence_rejected")
    assert.equal(await readFile(good, "utf8"), '{"counter":1}\n')
  } finally { await rm(root, { recursive: true, force: true }) }
})

test("the actual CLI returns a bounded fixed verdict and rejects untrusted build fields", async () => {
  const root = await mkdtemp(join(tmpdir(), "mount-rs-pilot-cli-"))
  try {
    const { pilot, build } = model()
    const pilotPath = join(root, "pilot.json"), buildPath = join(root, "build.json")
    await writeFile(pilotPath, JSON.stringify(pilot), { mode: 0o600 })
    await writeFile(buildPath, JSON.stringify(build), { mode: 0o600 })
    const script = fileURLToPath(new URL("./verify-owned-backing-pilot.mjs", import.meta.url))
    const run = promisify(execFile)
    const accepted = await run(process.execPath, [script, pilotPath, buildPath], { timeout: 2000, maxBuffer: 4096 })
    assert.equal(JSON.parse(accepted.stdout).status, "verified")
    assert.equal(accepted.stderr, "")
    await writeFile(buildPath, JSON.stringify({ ...build, private_path: "EXCLUDED_PRIVATE_VALUE" }))
    await assert.rejects(run(process.execPath, [script, pilotPath, buildPath], { timeout: 2000, maxBuffer: 4096 }), (error) => {
      assert.equal(error.code, 1)
      assert.equal(error.stdout, "")
      assert.deepEqual(JSON.parse(error.stderr), { schema: "mount-rs.hosted-backing-check.v1", status: "rejected", reason: "build_receipt" })
      assert.equal(error.stderr.includes("EXCLUDED_PRIVATE_VALUE"), false)
      assert.equal(error.stderr.includes(root), false)
      return true
    })
  } finally { await rm(root, { recursive: true, force: true }) }
})
