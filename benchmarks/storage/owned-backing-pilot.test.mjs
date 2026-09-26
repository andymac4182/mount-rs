import assert from "node:assert/strict"
import childProcess from "node:child_process"
import { EventEmitter } from "node:events"
import { mkdtemp, readFile, rm, writeFile, chmod } from "node:fs/promises"
import http from "node:http"
import https from "node:https"
import net from "node:net"
import tls from "node:tls"
import dgram from "node:dgram"
import Module, { createRequire, syncBuiltinESMExports } from "node:module"
import { tmpdir } from "node:os"
import { dirname, join, resolve } from "node:path"
import { PassThrough } from "node:stream"
import test from "node:test"
import { fileURLToPath } from "node:url"
import { promisify } from "node:util"

// Install guards before importing any benchmark/provider module. This suite
// exercises actual runner/transport/launch code against in-memory boundaries.
const repo = resolve(dirname(fileURLToPath(import.meta.url)), "../..")
const capturePath = fileURLToPath(new URL("./capture-native.cjs", import.meta.url))
assert.equal(process.env.NAPI_RS_NATIVE_LIBRARY_PATH, capturePath,
  "the launch command must select the explicit JS capture binding")
for (const key of ["NAPI_RS_FORCE_WASI", "NAPI_RS_WASI_FLAVOR", "NODE_PATH"]) {
  assert.equal(process.env[key], undefined, `${key} must be unset`)
}
const require = createRequire(import.meta.url)
const napiPath = join(repo, "bindings/mount-rs-napi/index.js")
assert.equal(require.cache[napiPath], undefined, "guard setup cannot preload the NAPI binding")
const initialProfile = process.env.MOUNT_RS_PROFILE_IO
assert.equal(initialProfile, undefined, "initial RED uses no synthetic or real native profiling latch")
assert.equal(process.env.MOUNT_RS_TRACE_STORAGE, undefined, "slow trace must be disabled")
const denied = { native: [], network: [], subprocess: [] }
const allowedSubprocesses = []
const permittedShells = new Map()
const nativeExtension = Module._extensions[".node"]
Module._extensions[".node"] = (_module, path) => {
  denied.native.push(path)
  throw new Error("pilot_native_load_forbidden")
}

const originals = []
function replace(object, key, value) {
  originals.push([object, key, object[key]])
  object[key] = value
}
function forbidNetwork() {
  denied.network.push("blocked")
  throw new Error("pilot_network_forbidden")
}
for (const [object, keys] of [
  [http, ["request", "get"]], [https, ["request", "get"]],
  [net, ["connect", "createConnection"]], [net.Socket.prototype, ["connect"]],
  [net.Server.prototype, ["listen"]], [tls, ["connect"]], [dgram, ["createSocket"]],
]) for (const key of keys) replace(object, key, forbidNetwork)
replace(globalThis, "fetch", forbidNetwork)

const originalExecFile = childProcess.execFile
function denyProcess(command) {
  denied.subprocess.push(command)
  throw new Error("pilot_subprocess_forbidden")
}
replace(childProcess, "execFile", (command, args, options, callback) => {
  const gitArgs = JSON.stringify(args)
  const readonlyGit = command === "git" && [repo, join(repo, "vendor/mountx")].includes(options?.cwd) &&
    [JSON.stringify(["rev-parse", "HEAD"]), JSON.stringify(["status", "--porcelain=v1", "--untracked-files=all"])].includes(gitArgs)
  const shell = command === "/bin/sh" && args?.length === 1 && permittedShells.has(args[0]) &&
    options?.cwd === permittedShells.get(args[0]).directory &&
    options?.env?.PATH === permittedShells.get(args[0]).fakePath
  if (!readonlyGit && !shell) return denyProcess(command)
  allowedSubprocesses.push(readonlyGit ? "readonly_git_metadata" : "isolated_fake_launch")
  const guardedOptions = readonlyGit ? { ...options, env: { ...process.env,
    GIT_OPTIONAL_LOCKS: "0", GIT_CONFIG_COUNT: "1", GIT_CONFIG_KEY_0: "core.fsmonitor", GIT_CONFIG_VALUE_0: "false",
  } } : options
  return originalExecFile(command, args, guardedOptions, callback)
})
for (const key of ["exec", "execSync", "execFileSync", "spawn", "spawnSync", "fork"]) {
  replace(childProcess, key, (command) => denyProcess(command))
}
syncBuiltinESMExports()
const execFileAsync = promisify(childProcess.execFile)

const { parseArgs, runBenchmark } = await import("./runner.mjs")
const { providerById } = await import("./providers.mjs")
const { createBackingObserver } = await import("./backing-observer.mjs")
const { createBackingEngineTransport } = await import("./backing-engine-transport.mjs")
const pilot = await import("./owned-backing-pilot.mjs")
const diagnostics = await import("./diagnostics.mjs")
const { STORAGE_OPERATION_NAMES, STORAGE_OPERATION_FAMILIES, STORAGE_INSTRUMENTED_OPERATION_NAMES, STORAGE_CALL_SEMANTICS, STORAGE_BYTE_SEMANTICS, STORAGE_ROW_SEMANTICS, TIDB_DIAGNOSTIC_COVERAGE } = diagnostics
const capture = require(capturePath)

test.after(() => {
  try {
    assert.equal(capture.__napiBindingTarget, "storage-benchmark-capture")
    assert.equal(Object.keys(require.cache).some((path) => path.endsWith(".node")), false)
    assert.deepEqual(denied.native, ["OWNED_MOCK_NATIVE_GUARD_PROBE.node"])
    assert.deepEqual(denied.network, ["blocked"])
    assert.deepEqual(denied.subprocess, ["cargo", "curl", "node"])
    assert.equal(process.env.MOUNT_RS_PROFILE_IO, initialProfile)
    process.stderr.write(`OWNED_BACKING_MOCK_GUARDS ${JSON.stringify({
      identity: "capture_js_mock", native_sha256: null, real_native_proof: "unverified",
      actual_network_calls: 0, actual_native_loads: 0, fixture_scripts_executed: 0,
      allowed_subprocesses: allowedSubprocesses,
    })}\n`)
  } finally {
    Module._extensions[".node"] = nativeExtension
    for (const [object, key, original] of originals.reverse()) object[key] = original
    syncBuiltinESMExports()
  }
})

test("capture selector and rejection guards precede any real native/network/fixture dispatch", () => {
  assert.equal(capture.__napiBindingTarget, "storage-benchmark-capture")
  assert.equal(require.cache[napiPath], undefined, "capture source identity must not load the binding")
  assert.throws(() => Module._extensions[".node"]({}, "OWNED_MOCK_NATIVE_GUARD_PROBE.node"), /pilot_native_load_forbidden/)
  assert.throws(() => http.request({ socketPath: "/MOCK_SOCKET_MUST_NEVER_OPEN" }), /pilot_network_forbidden/)
  for (const command of ["cargo", "curl", "node"]) {
    assert.throws(() => childProcess.execFileSync(command, []), /pilot_subprocess_forbidden/)
  }
})

test("private handoff rejects duplicate roles, stale generations and endpoint precedence before any dispatch", () => {
  const receipt = receiptFixture()
  assert.equal(pilot.validateHandoff(receipt, bindingEnvironment()).entries.length, 8)
  for (const mutate of [
    (value) => { value.entries[1] = value.entries[0] },
    (value) => { value.generation = "2" },
    (value) => { value.entries[7].labels["com.mount-rs.rustfs-test-purpose"] = "cleanup" },
    (value) => { value.entries.pop() },
    (value) => { value.extra = "EXCLUDED_MOCK_SECRET" },
  ]) {
    const invalid = structuredClone(receipt); mutate(invalid)
    assert.throws(() => pilot.validateHandoff(invalid, bindingEnvironment()), /pilot_handoff_rejected/)
  }
  assert.throws(() => pilot.validateHandoff(receipt, { ...bindingEnvironment(), MOUNT_RS_R2_ENDPOINT: "EXCLUDED_STALE_URL" }), /pilot_endpoint_binding_rejected/)
})

test("inert capture identity does not load binding or change profiling, and cannot qualify real native", async () => {
  assert.equal(require.cache[napiPath], undefined)
  const identity = await pilot.preflightIdentity(process.env, { mock: true })
  assert.equal(identity.public.kind, "capture_js_mock")
  assert.equal(identity.public.native_sha256, null)
  assert.equal(identity.public.real_native_proof, "unverified")
  assert.equal(require.cache[napiPath], undefined)
  assert.equal(process.env.MOUNT_RS_PROFILE_IO, initialProfile)
  await assert.rejects(pilot.preflightIdentity(process.env), /pilot_native_selection_rejected/)
})

function fakeClock() {
  let now = 0, next = 0
  const timers = new Map()
  return {
    now: () => now, utc: () => new Date(1_800_000_000_000 + now).toISOString(),
    cpu: () => ({ user: now * 2, system: now }),
    setTimeout(callback, milliseconds) { const id = ++next; timers.set(id, { at: now + milliseconds, callback }); return id },
    clearTimeout(id) { timers.delete(id) },
    advance(milliseconds) {
      now += milliseconds
      for (const [id, timer] of [...timers]) if (timer.at <= now) { timers.delete(id); timer.callback() }
    },
    timers,
  }
}

const roles = ["pd-1", "pd-2", "pd-3", "tikv-1", "tikv-2", "tikv-3", "tidb", "rustfs-service"]
const allowlist = roles.map((role, index) => ({
  cid: (index + 1).toString(16).repeat(64), role,
  labels: role === "rustfs-service"
    ? { "com.mount-rs.rustfs-test": "mount-rs-rustfs-test", "com.mount-rs.rustfs-test-run": "mock-rustfs" }
    : { "mount-rs.tidb.run": "mock-tidb-run" },
}))
function engineResponse(path, clock) {
  if (path === "/version") return { ApiVersion: "1.51", MinAPIVersion: "1.24", Os: "linux" }
  const cid = path.split("/")[3], entry = allowlist.find((item) => item.cid === cid)
  assert.ok(entry, "the actual transport must retain an exact allowlisted CID")
  if (path.endsWith("/json")) return {
    Id: cid, Image: `sha256:${"a".repeat(64)}`, RestartCount: 0,
    State: { Running: true, StartedAt: "2026-09-27T00:00:00Z" },
    Config: { Labels: entry.labels, Env: ["EXCLUDED_MOCK_SECRET"], Cmd: ["EXCLUDED_MOCK_SECRET"] },
    HostConfig: { Memory: 0, MemorySwap: -1, NanoCpus: 0, CpuQuota: -1, CpuPeriod: 0, CpuShares: 0, CpusetCpus: "", PidsLimit: 0 },
    Mounts: [{ Source: "EXCLUDED_MOCK_SECRET" }],
  }
  assert.match(path, /\/stats\?stream=false$/)
  return {
    id: cid, read: clock.utc(),
    cpu_stats: { cpu_usage: { total_usage: 1000 + clock.now() * 1000 }, system_cpu_usage: 1000 + clock.now() * 10000, online_cpus: 4 },
    memory_stats: { usage: 1024, limit: 10737418240 },
    blkio_stats: {
      io_service_bytes_recursive: [{ major: 8, minor: 0, op: "Read", value: clock.now() * 2 }, { major: 8, minor: 0, op: "Write", value: clock.now() * 3 }],
      io_serviced_recursive: [{ major: 8, minor: 0, op: "Read", value: clock.now() }, { major: 8, minor: 0, op: "Write", value: clock.now() }],
    },
    networks: { eth0: { rx_bytes: clock.now() * 4, tx_bytes: clock.now() * 5 } },
  }
}
function fakeHttp(clock) {
  const calls = []
  return {
    calls,
    requestImpl(options, onResponse) {
      const request = new EventEmitter()
      request.destroyed = false
      request.end = () => queueMicrotask(() => {
        if (request.destroyed) return
        const response = new PassThrough()
        response.statusCode = 200; response.headers = {}; response.complete = true
        request.response = response
        onResponse(response)
        response.end(Buffer.from(JSON.stringify(engineResponse(options.path, clock))))
      })
      request.destroy = () => {
        if (request.destroyed) return request
        request.destroyed = true; request.response?.destroy(); request.emit("close")
        return request
      }
      calls.push({ method: options.method, path: options.path })
      return request
    },
  }
}
function controlledProvider(clock, lifecycle) {
  const definition = { ...providerById({}).get("mount-rs-memory"), id: "mount-rs-split-tidb-r2" }
  const files = new Map()
  definition.availability = () => ({ configured: true })
  definition.create = async () => {
    lifecycle.created++; clock.advance(1100)
    return {
      filesystem: {
        async writeFile(path, payload) { clock.advance(1); lifecycle.writes++; files.set(path, Buffer.from(payload)) },
        async readFile(path) { clock.advance(1); lifecycle.reads++; return files.get(path) },
        async unlink(path) { clock.advance(1); lifecycle.deletes++; files.delete(path) },
      },
      async cleanup() { lifecycle.cleaned++; clock.advance(1100) },
    }
  }
  return new Map([[definition.id, definition]])
}

test("owned receipt and inert identity feed the actual transport/factory/runner once with bounded sanitized output", async () => {
  const directory = await mkdtemp(join(tmpdir(), "mount-rs-pilot-cell-mock-"))
  const clock = fakeClock(), fake = fakeHttp(clock), lifecycle = { created: 0, cleaned: 0, writes: 0, reads: 0, deletes: 0 }
  const environment = { ...bindingEnvironment(), NAPI_RS_NATIVE_LIBRARY_PATH: capturePath,
    MOUNT_RS_BACKING_CID_DIR: directory, MOUNT_RS_BACKING_RUSTFS_RECEIPT: join(directory, "rustfs.json"),
    MOUNT_RS_BACKING_CID_RECEIPT: join(directory, "cids.json"), MOUNT_RS_BACKING_ENGINE_CAPABILITY: join(directory, "engine.json"), MOUNT_RS_BACKING_PILOT_OUTPUT: join(directory, "pilot.json") }
  try {
    for (const entry of allowlist) await writeFile(join(directory, `${entry.role}-1.cid`), `${entry.cid}\n`, { mode: 0o600 })
    await writeFile(environment.MOUNT_RS_BACKING_ENGINE_CAPABILITY, JSON.stringify({ socket_path: "/MOCK_SOCKET_MUST_NEVER_OPEN" }), { mode: 0o600 })
    await pilot.writeFixtureReceipt("rustfs", environment)
    await pilot.writeFixtureReceipt("tidb", environment)
    const beforeProfile = process.env.MOUNT_RS_PROFILE_IO
    const rejected = { ...environment, MOUNT_RS_R2_ENDPOINT: "EXCLUDED_STALE_URL" }
    await assert.rejects(pilot.runOwnedPilot(rejected, { mock: true, clock, requestImpl: fake.requestImpl, providers: controlledProvider(clock, lifecycle) }), /pilot_endpoint_binding_rejected/)
    assert.equal(fake.calls.length, 0); assert.equal(lifecycle.created, 0)
    await chmod(environment.MOUNT_RS_BACKING_CID_RECEIPT, 0o644)
    await assert.rejects(pilot.runOwnedPilot(environment, { mock: true, clock, requestImpl: fake.requestImpl }), /pilot_handoff_rejected/)
    assert.equal(fake.calls.length, 0)
    await chmod(environment.MOUNT_RS_BACKING_CID_RECEIPT, 0o600)
    const outcome = await pilot.runOwnedPilot(environment, { mock: true, clock, requestImpl: fake.requestImpl, providers: controlledProvider(clock, lifecycle) })
    assert.equal(outcome.status, "ok"); assert.equal(outcome.live_measurement_qualified, false)
    assert.equal(fake.calls.length, 33); assert.equal(lifecycle.cleaned, 1)
    const bytes = await readFile(environment.MOUNT_RS_BACKING_PILOT_OUTPUT), record = JSON.parse(bytes)
    assert.equal(record.identity.kind, "capture_js_mock"); assert.equal(record.identity.native_sha256, null)
    assert.equal(record.native_used_identity, "unverified")
    assert.equal(record.config.minimum_iops, 1000); assert.equal(record.outcome.successful_operations, 1200)
    assert.equal(record.resource_coverage.create, "not_selected")
    for (const secret of [directory, "EXCLUDED", "mysql://", "http://", "MOCK_SOCKET", "private_file"]) assert.equal(bytes.includes(secret), false)
    assert.equal(process.env.MOUNT_RS_PROFILE_IO, beforeProfile)
    const { stat } = await import("node:fs/promises")
    assert.equal((await stat(environment.MOUNT_RS_BACKING_PILOT_OUTPUT)).mode & 0o777, 0o600)
  } finally { await rm(directory, { recursive: true, force: true }) }
})

test("selected workload captures only two original factory boundaries without changing the lifecycle or floor", async () => {
  const clock = fakeClock(), fake = fakeHttp(clock)
  const lifecycle = { created: 0, cleaned: 0, writes: 0, reads: 0, deletes: 0 }
  const transport = createBackingEngineTransport({
    socketPath: "/MOCK_SOCKET_MUST_NEVER_OPEN", allowlistedCids: allowlist.map((item) => item.cid), requestImpl: fake.requestImpl,
  })
  const observer = createBackingObserver({ allowlist, transport, clock })
  const options = parseArgs(["--providers", "mount-rs-split-tidb-r2", "--sizes", "1", "--payload-bytes", "4096",
    "--iterations", "400", "--concurrency", "64", "--min-iops", "1000", "--require-configured", "--layout", "legacy"])
  const result = await runBenchmark(options, {}, controlledProvider(clock, lifecycle), {
    backingObserver: observer, backingObserverWindow: "workload", observerClock: clock,
  })
  assert.deepEqual(lifecycle, { created: 1, cleaned: 1, writes: 400, reads: 400, deletes: 400 })
  assert.equal(result.results[0].summary.successfulIterations, 400)
  assert.equal(result.results[0].summary.successfulOperations, 1200)
  assert.equal(result.results[0].summary.iopsTarget, 1000)
  assert.equal(result.results[0].summary.timeoutCount, 0)
  assert.equal(result.results[0].summary.cleanupFailureCount, 0)
  // This control deliberately has unobserved native proof, even though Engine
  // replies and provider operations are synthetic and successful.
  const wrapper = result.providers[0].backingObserver
  assert.equal(wrapper.terminal.native_quiescent, null)
  assert.equal(wrapper.terminal.safe_to_continue_pair, false)
  assert.ok(wrapper.backing_evidence, "the original claimed/finalized factory must be exported")
  const boundaries = wrapper.backing_evidence.journal.filter((item) => item.type === "boundary").map((item) => item.id)
  assert.deepEqual(boundaries, ["workload-4096bytes:begin", "workload-4096bytes:end"],
    "actual current runner must honor selected resource dispatch rather than collect every phase")
  assert.deepEqual(result.providers[0].backingResourceCoverage, {
    create: "not_selected", "workload-4096bytes": "captured", cleanup: "not_selected", shutdown: "not_selected",
  })
  assert.equal(fake.calls.length, 33)
  assert.ok(fake.calls.every((call) => call.method === "GET"))
  assert.equal(JSON.stringify(wrapper).includes("EXCLUDED_MOCK_SECRET"), false)
  assert.equal(clock.timers.size, 0)
})

function namedLaunch(source, name) {
  const start = source.indexOf(`\n${name}() {\n`)
  assert.notEqual(start, -1, `expected exact private launch function ${name}`)
  const end = source.indexOf("\n}\n", start)
  assert.notEqual(end, -1)
  return source.slice(start + 1, end + 3)
}
function rustfsLaunch(source) {
  if (source.includes("\nstart_rustfs_service() {\n")) return `${namedLaunch(source, "start_rustfs_service")}\nstart_rustfs_service\n`
  const start = source.indexOf('if bounded_docker_startup_command "run-service" docker run --detach \\\n')
  assert.notEqual(start, -1, "expected current exact inline RustFS service launch")
  const end = source.indexOf("\nfi\n\nrefresh_endpoint()", start)
  assert.notEqual(end, -1)
  return source.slice(start, end + 4)
}
function assertIsolatedLaunch(body) {
  assert.ok(body.length < 8192, "only a narrow launch body can execute")
  assert.doesNotMatch(body, /(?:^|\n)\s*(?:\.\s|source\s|exec\s|eval\s|curl\s|node\s|python\S*\s|cargo\s|sh\s|bash\s|docker\s+(?:info|ps|logs|restart|stop|rm|pull))/u)
  assert.doesNotMatch(body, /`|\$\{![^}]*\}|(?:^|\n)\s*(?:\/|"\$\{?repo_dir)/u)
  for (const match of body.matchAll(/\$\(([^\n]*?)\)/gu)) {
    assert.match(match[1], /^(?:pd_ip_for_number|tikv_ip_for_number|docker inspect --format) /u)
    assert.doesNotMatch(match[1], /[;`]|\$\(/u)
  }
  // Only literal known statements can begin a command. Continuation lines
  // belong to the fixed docker argv and never become separately evaluated text.
  let continuation = false
  for (const raw of body.split("\n")) {
    const line = raw.trim()
    if (!line || line.startsWith("#")) continue
    if (!continuation) {
      const allowed = /^(?:(?:start_pd|start_tikv|start_tidb|start_rustfs_service)\(\) \{|[a-z_][a-z0-9_]*=|docker (?:run|inspect) |set --(?: --cidfile "\$run_dir\/[A-Za-z0-9$_.:-]+")?$|if \[ "\$backing_pilot" = 1 \]; then$|if bounded_docker_startup_command "run-service" docker run |: ?$|else$|fi$|\}$|echo "Could not start RustFS container |exit 1$|(?:start_pd|start_tikv) [1-3] mock-|start_tidb mock-tidb$|start_rustfs_service$)/u
      assert.match(line, allowed, "a launch-only harness cannot execute an unrecognized statement")
      if (/^[a-z_][a-z0-9_]*=/u.test(line)) {
        assert.match(line, /^[a-z_][a-z0-9_]*=(?:\$[1-3?]|"[^"\n]*"|\$\((?:pd_ip_for_number|tikv_ip_for_number|docker inspect --format) [^\n]*\))$/u,
          "an assignment cannot prefix another executable or an unchecked shell expression")
      }
    }
    continuation = line.endsWith("\\")
  }
}
const fakeDockerSource = `#!/bin/sh
set -eu
verb=$1
shift
case "$verb" in
  run)
    name=""; cidfile=""
    printf 'RUN_BEGIN\\n' >>"$MOCK_DOCKER_LOG"
    for arg do printf 'ARG=%s\\n' "$arg" >>"$MOCK_DOCKER_LOG"; done
    printf 'RUN_END\\n' >>"$MOCK_DOCKER_LOG"
    while [ "$#" -gt 0 ]; do
      case "$1" in
        --name) shift; name=$1 ;;
        --cidfile) shift; cidfile=$1 ;;
      esac
      shift
    done
    case "$name" in
      mock-pd1) digit=1 ;; mock-pd2) digit=2 ;; mock-pd3) digit=3 ;;
      mock-tikv1) digit=4 ;; mock-tikv2) digit=5 ;; mock-tikv3) digit=6 ;;
      mock-tidb) digit=7 ;; mock-rustfs) digit=8 ;; *) exit 90 ;;
    esac
    cid=""; count=0
    while [ "$count" -lt 64 ]; do cid="$cid$digit"; count=$((count + 1)); done
    if [ -n "$cidfile" ]; then
      case "$cidfile" in "$MOCK_CID_ROOT"/*) ;; *) exit 91 ;; esac
      [ ! -e "$cidfile" ] || exit 92
      (umask 077; printf '%s\\n' "$cid" >"$cidfile")
    fi
    printf '%s\\n' "$cid"
    ;;
  inspect)
    [ "$#" -eq 3 ] && [ "$1" = --format ] && [ "$3" = mock-tidb ] || exit 93
    case "$2" in
      '{{(index (index .NetworkSettings.Ports "4000/tcp") 0).HostPort}}') printf '40000\\n' ;;
      '{{(index (index .NetworkSettings.Ports "10080/tcp") 0).HostPort}}') printf '40080\\n' ;;
      *) exit 94 ;;
    esac
    ;;
  *) exit 95 ;;
esac
`
function launchPrelude(directory) {
  assert.doesNotMatch(directory, /['\n]/u)
  return `set -eu
umask 077
run_dir='${directory}'
pilot_cid_dir="$run_dir"
backing_pilot=1
pilot_enabled=1
backing_generation=1
tidb_generation=1
run_id=mock-tidb-run
resource_value=mock-tidb-run
resource_label=mount-rs.tidb.run=mock-tidb-run
docker_platform=linux/arm64
network_name=mock-network
pd_cluster=mock-pd-cluster
pd_endpoints=mock-pd-endpoints
pd_image=mock-pd-image
tikv_image=mock-tikv-image
tidb_image=mock-tidb-image
tikv_nofile_limit=200000
tikv_config=mock-tikv-config
tidb_config=mock-tidb-config
tidb_ip=127.0.0.1
created_containers=""
container_name=mock-rustfs
ownership_label=mount-rs-rustfs-test
data_dir=mock-data
rustfs_access_key=EXCLUDED_MOCK_KEY
rustfs_secret_key=EXCLUDED_MOCK_SECRET
rustfs_region=mock-region
rustfs_image=mock-rustfs-image
pd_ip_for_number() { printf '127.0.0.1\\n'; }
tikv_ip_for_number() { printf '127.0.0.1\\n'; }
bounded_docker_startup_command() {
  [ "$1" = run-service ] || return 96
  shift
  [ "$1" = docker ] || return 97
  "$@"
}
`
}

for (const [role, name, args] of [
  ["pd-1", "start_pd", "1 mock-pd1 mock-volume"], ["pd-2", "start_pd", "2 mock-pd2 mock-volume"], ["pd-3", "start_pd", "3 mock-pd3 mock-volume"],
  ["tikv-1", "start_tikv", "1 mock-tikv1 mock-volume"], ["tikv-2", "start_tikv", "2 mock-tikv2 mock-volume"], ["tikv-3", "start_tikv", "3 mock-tikv3 mock-volume"],
  ["tidb", "start_tidb", "mock-tidb"], ["rustfs-service", null, null],
]) test(`actual ${role} creation retains its exact CID through an owned cidfile`, async () => {
  const directory = await mkdtemp(join(tmpdir(), "mount-rs-pilot-launch-mock-"))
  const fakePath = join(directory, "fake-bin"), harness = join(directory, "launch.sh"), log = join(directory, "argv.log")
  const { mkdir } = await import("node:fs/promises")
  await mkdir(fakePath, { mode: 0o700 })
  await writeFile(join(fakePath, "docker"), fakeDockerSource, { mode: 0o700 })
  await chmod(join(fakePath, "docker"), 0o700)
  const source = await readFile(join(repo, "scripts", name ? "test-tidb.sh" : "test-rustfs.sh"), "utf8")
  const launch = name ? `${namedLaunch(source, name)}\n${name} ${args}\n` : rustfsLaunch(source)
  assertIsolatedLaunch(launch)
  await writeFile(harness, `${launchPrelude(directory)}\n${launch}`, { mode: 0o600 })
  permittedShells.set(harness, { directory, fakePath })
  try {
    await execFileAsync("/bin/sh", [harness], {
      cwd: directory, env: { PATH: fakePath, MOCK_DOCKER_LOG: log, MOCK_CID_ROOT: directory, MOUNT_RS_BACKING_PILOT: "1" },
      timeout: 2000, maxBuffer: 16384,
    })
    const records = (await readFile(log, "utf8")).trim().split("\n")
    assert.equal(records[0], "RUN_BEGIN")
    assert.equal(records.at(-1), "RUN_END")
    const argv = records.slice(1, -1).map((line) => line.slice(4))
    const labels = argv.flatMap((arg, index) => arg === "--label" ? [argv[index + 1]] : [])
    assert.deepEqual(labels, name ? ["mount-rs.tidb.run=mock-tidb-run"]
      : ["com.mount-rs.rustfs-test=mount-rs-rustfs-test", "com.mount-rs.rustfs-test-run=mock-rustfs"])
    const cidIndex = argv.indexOf("--cidfile")
    assert.notEqual(cidIndex, -1, `actual ${role} detached launch must retain CID at creation, not discard stdout`)
    const cidfile = argv[cidIndex + 1]
    assert.ok(cidfile.startsWith(`${directory}/`))
    assert.match((await readFile(cidfile, "utf8")).trim(), /^[1-8]{64}$/u)
    const { stat } = await import("node:fs/promises")
    assert.equal((await stat(cidfile)).mode & 0o777, 0o600)
    // Default-off retains the original detached argv and makes no CID file.
    await rm(log)
    const disabledPrelude = launchPrelude(directory).replace("backing_pilot=1", "backing_pilot=0")
    await writeFile(harness, `${disabledPrelude}\n${launch}`, { mode: 0o600 })
    await execFileAsync("/bin/sh", [harness], { cwd: directory, env: { PATH: fakePath, MOCK_DOCKER_LOG: log, MOCK_CID_ROOT: directory }, timeout: 2000, maxBuffer: 16384 })
    const disabled = await readFile(log, "utf8")
    assert.equal(disabled.includes("ARG=--cidfile"), false)
  } finally {
    permittedShells.delete(harness)
    await rm(directory, { recursive: true, force: true })
  }
})

function bindingEnvironment() {
  return { MOUNT_RS_BACKING_TIDB_OWNER: "mock-tidb-run", MOUNT_RS_BACKING_RUSTFS_OWNER: "mock-rustfs", MOUNT_RS_BACKING_GENERATION: "1",
    MOUNT_RS_BACKING_EXPECT_TIDB_URL: "mysql://root@127.0.0.1:40000/test", MOUNT_RS_BACKING_EXPECT_R2_ENDPOINT: "http://127.0.0.1:9000", MOUNT_RS_BACKING_EXPECT_R2_BUCKET: "mock-bucket",
    TIDB_URL: "mysql://root@127.0.0.1:40000/test", R2_ENDPOINT: "http://127.0.0.1:9000", R2_BUCKET: "mock-bucket" }
}
function receiptFixture() {
  return { schema: "mount-rs.owned-backing-cids.v1", tidb_owner: "mock-tidb-run", rustfs_owner: "mock-rustfs", generation: "1", entries: structuredClone(allowlist) }
}
function resultFixture() {
  return { status: "failed", runId: "mock-run", environment: { secret: "EXCLUDED_MOCK_SECRET" },
    results: [{ status: "failed", summary: { elapsedMs: 2000, iops: 600, iopsTarget: 1000, iopsTargetMet: false, successfulIterations: 399, failedIterations: 1,
      successfulOperations: 1197, attemptedOperations: 1200, timeoutCount: 1, cleanupFailureCount: 1, operationSuccess: { verifiedReads: 399 } },
      failures: [{ operation: "iops-target", error: { code: "IOPS_TARGET_NOT_MET", message: "EXCLUDED_MOCK_SECRET" } }],
      rawSamples: [{ errors: [{ operation: "read", error: { message: "EXCLUDED_MOCK_SECRET" } }] }] }],
    providers: [{ status: "failed", cleanup: { pathsAttempted: 1, remainingPaths: 1, failures: [{ error: "EXCLUDED_MOCK_SECRET" }], resource: { status: "failed" } },
      backingResourceCoverage: { create: "not_selected", "workload-4096bytes": "incomplete", cleanup: "not_selected", shutdown: "not_selected" } }] }
}

test("combined output cap atomically falls back and preserves first failure, floor and cleanup counts", async () => {
  const directory = await mkdtemp(join(tmpdir(), "mount-rs-pilot-output-mock-"))
  try {
    const record = pilot.projectPilotRecord(resultFixture(), { kind: "capture_js_mock", native_sha256: null, real_native_proof: "unverified" })
    assert.equal(record.outcome.first_failure.code, "provider_operation_failed")
    assert.equal(record.outcome.first_failure.operation, "read")
    assert.equal(JSON.stringify(record).includes("EXCLUDED_MOCK_SECRET"), false)
    record.native_phases = { excessive: "a".repeat(pilot.OUTPUT_CAP) }
    const output = join(directory, "pilot.json")
    const written = await pilot.writePilotRecord(output, record)
    const bytes = await readFile(output), fallback = JSON.parse(bytes)
    assert.ok(bytes.length <= pilot.OUTCOME_RESERVE)
    assert.equal(written.omitted, true)
    assert.equal(fallback.native_phases.status, "omitted_output_cap")
    assert.equal(fallback.backing.status, "omitted_output_cap")
    assert.equal(fallback.outcome.first_failure.code, "provider_operation_failed")
    assert.equal(fallback.outcome.first_failure.operation, "read")
    assert.equal(fallback.outcome.status, "failed")
    assert.equal(fallback.outcome.iops_target, 1000)
    assert.equal(fallback.outcome.successful_operations, 1197)
    assert.equal(fallback.outcome.timeout_count, 1)
    assert.equal(fallback.outcome.cleanup_failure_count, 1)
    assert.equal(fallback.outcome.remaining_paths, 1)
    assert.equal(fallback.live_measurement_qualified, false)
    const { readdir } = await import("node:fs/promises")
    assert.deepEqual(await readdir(directory), ["pilot.json"])
    const floorOnly = resultFixture(); floorOnly.results[0].rawSamples = []
    const floorProjection = pilot.projectPilotRecord(floorOnly, {})
    assert.equal(floorProjection.outcome.first_failure.code, "iops_target_not_met")
    floorProjection.backing = { excessive: "a".repeat(pilot.OUTPUT_CAP) }
    await pilot.writePilotRecord(output, floorProjection)
    assert.equal(JSON.parse(await readFile(output, "utf8")).outcome.first_failure.code, "iops_target_not_met")
    const missing = pilot.projectPilotRecord({ status: "failed", providers: [], results: [] }, {})
    assert.equal(missing.outcome.successful_operations, null)
    assert.equal(missing.outcome.elapsed_ms, null)
    assert.equal(pilot.encodeBoundedJSON({ value: "x" }, 14).byteLength, 14)
    assert.throws(() => pilot.encodeBoundedJSON({ value: "xx" }, 14), /pilot_output_cap/)
    for (const [operation, expected] of [["read", "provider_operation_failed"], ["cleanup", "cleanup_failed"]]) {
      const failure = resultFixture(); delete failure.results[0].failures
      failure.results[0].rawSamples = operation === "read" ? [{ errors: [{ operation, error: { message: "EXCLUDED_MOCK_SECRET" } }] }] : []
      const projected = pilot.projectPilotRecord(failure, {})
      assert.equal(projected.outcome.first_failure.code, expected)
      assert.equal(JSON.stringify(projected).includes("EXCLUDED_MOCK_SECRET"), false)
    }
  } finally { await rm(directory, { recursive: true, force: true }) }
})

test("short selected window retains cadence incompleteness without pacing or extra captures", async () => {
  const clock = fakeClock(), fake = fakeHttp(clock), lifecycle = { created: 0, cleaned: 0, writes: 0, reads: 0, deletes: 0 }
  const observer = createBackingObserver({ allowlist, clock, transport: createBackingEngineTransport({ socketPath: "/MOCK_SOCKET_MUST_NEVER_OPEN", allowlistedCids: allowlist.map((item) => item.cid), requestImpl: fake.requestImpl }) })
  const options = parseArgs(["--providers", "mount-rs-split-tidb-r2", "--sizes", "1", "--payload-bytes", "4096", "--iterations", "1", "--concurrency", "1", "--min-iops", "1000"])
  const result = await runBenchmark(options, {}, controlledProvider(clock, lifecycle), { backingObserver: observer, backingObserverWindow: "workload", observerClock: clock })
  const receipt = result.providers[0].backingObserver
  assert.equal(clock.now(), 2203)
  assert.equal(lifecycle.cleaned, 1)
  assert.equal(fake.calls.length, 17)
  assert.ok(receipt.backing_evidence.issues.includes("sample_interval_cap"))
  assert.equal(result.providers[0].backingResourceCoverage["workload-4096bytes"], "incomplete")
  assert.equal(receipt.terminal.safe_to_continue_pair, false)
})

for (const boundary of ["begin", "end"]) test(`late selected ${boundary} cannot repair original finalized export`, async () => {
  const clock = fakeClock(), fake = fakeHttp(clock), delayed = [], lifecycle = { created: 0, cleaned: 0, writes: 0, reads: 0, deletes: 0 }
  let dispatches = 0
  const requestImpl = (options, callback) => {
    dispatches++
    if (dispatches !== (boundary === "begin" ? 1 : 18)) return fake.requestImpl(options, callback)
    const request = new EventEmitter(); request.destroyed = false
    request.destroy = () => { request.destroyed = true; request.emit("close"); return request }
    request.end = () => { delayed.push(() => { const stream = new PassThrough(); stream.statusCode = 200; stream.headers = {}; stream.complete = true; callback(stream); stream.end(Buffer.from(JSON.stringify(engineResponse(options.path, clock)))) }); queueMicrotask(() => clock.advance(2001)) }
    return request
  }
  const observer = createBackingObserver({ allowlist, clock, transport: createBackingEngineTransport({ socketPath: "/MOCK_SOCKET_MUST_NEVER_OPEN", allowlistedCids: allowlist.map((item) => item.cid), requestImpl }) })
  const options = parseArgs(["--providers", "mount-rs-split-tidb-r2", "--sizes", "1", "--payload-bytes", "4096", "--iterations", "400", "--concurrency", "64"])
  const result = await runBenchmark(options, {}, controlledProvider(clock, lifecycle), { backingObserver: observer, backingObserverWindow: "workload", observerClock: clock })
  const exported = JSON.stringify(result.providers[0].backingObserver), original = JSON.stringify(observer.receipt()), before = dispatches
  assert.equal(lifecycle.cleaned, 1)
  assert.equal(result.providers[0].backingObserver.complete, false)
  assert.equal(result.providers[0].backingObserver.terminal.safe_to_continue_pair, false)
  assert.ok(result.providers[0].backingObserver.events.some((event) => event.hook === "finalize"))
  for (const settle of delayed) settle()
  for (let step = 0; step < 30; step++) await Promise.resolve()
  assert.equal(JSON.stringify(observer.receipt()), original)
  assert.equal(JSON.stringify(result.providers[0].backingObserver), exported)
  assert.equal(dispatches, before)
})

test("owner deadline remains incomplete and configuration rejection does not claim a session", async () => {
  const clock = fakeClock(), fake = fakeHttp(clock), lifecycle = { created: 0, cleaned: 0, writes: 0, reads: 0, deletes: 0 }
  const observer = createBackingObserver({ allowlist, clock, transport: createBackingEngineTransport({ socketPath: "/MOCK_SOCKET_MUST_NEVER_OPEN", allowlistedCids: allowlist.map((item) => item.cid), requestImpl: fake.requestImpl }) })
  const options = parseArgs(["--providers", "mount-rs-split-tidb-r2", "--sizes", "1", "--payload-bytes", "4096", "--iterations", "1", "--concurrency", "1"])
  await assert.rejects(runBenchmark({ ...options, sizes: [1, 4] }, {}, controlledProvider(clock, lifecycle), { backingObserver: observer, backingObserverWindow: "workload", observerClock: clock }), /one observer, provider and size/)
  assert.equal(fake.calls.length, 0)
  clock.advance(60001)
  const result = await runBenchmark(options, {}, controlledProvider(clock, lifecycle), { backingObserver: observer, backingObserverWindow: "workload", observerClock: clock })
  assert.equal(fake.calls.length, 0)
  assert.equal(lifecycle.cleaned, 1)
  assert.ok(result.providers[0].backingObserver.backing_evidence.issues.includes("owner_deadline"))
  assert.equal(result.providers[0].backingObserver.terminal.safe_to_continue_pair, false)
})

test("selected resources retain all four enabled native phases with unavailable synthetic counters", async () => {
  const clock = fakeClock(), fake = fakeHttp(clock), lifecycle = { created: 0, cleaned: 0, writes: 0, reads: 0, deletes: 0 }
  const observer = createBackingObserver({ allowlist, clock, transport: createBackingEngineTransport({ socketPath: "/MOCK_SOCKET_MUST_NEVER_OPEN", allowlistedCids: allowlist.map((item) => item.cid), requestImpl: fake.requestImpl }) })
  const options = parseArgs(["--providers", "mount-rs-split-tidb-r2", "--sizes", "1", "--payload-bytes", "4096", "--iterations", "400", "--concurrency", "64"])
  try {
    const result = await runBenchmark(options, { MOUNT_RS_PROFILE_IO: "1" }, controlledProvider(clock, lifecycle), { backingObserver: observer, backingObserverWindow: "workload", observerClock: clock })
    assert.deepEqual(result.providers[0].storageDiagnostics.phases.map((phase) => phase.name), ["create", "workload-4096bytes", "cleanup", "shutdown"])
    assert.ok(result.providers[0].storageDiagnostics.phases.every((phase) => phase.native.complete === false))
    assert.equal(fake.calls.length, 33)
    assert.equal(lifecycle.cleaned, 1)
    const projected = pilot.projectPilotRecord(result, { kind: "capture_js_mock", native_sha256: null, real_native_proof: "unverified" })
    assert.equal(projected.live_measurement_qualified, false)
    assert.equal(projected.native_phases.phases[1].validated_workload, false)
    assert.equal(projected.native_phases.phases[1].storage[0].calls, null)
  } finally {
    if (initialProfile === undefined) delete process.env.MOUNT_RS_PROFILE_IO
    else process.env.MOUNT_RS_PROFILE_IO = initialProfile
  }
})

test("native projection keeps fixed core counters and Node resources, rejecting false live qualification", () => {
  const fixture = resultFixture(); fixture.status = "ok"; fixture.providers[0].status = "ok"
  fixture.providers[0].backingPilotIdentity = { native_used_identity: "verified", native_sha256: "a".repeat(64) }
  fixture.providers[0].backingObserver = { schema: "mount-rs.runner-backing-observer.v1", complete: true, terminal: { safe_to_continue_pair: true } }
  fixture.providers[0].backingResourceCoverage["workload-4096bytes"] = "captured"
  fixture.providers[0].storageDiagnostics = { phases: [{ name: "workload-4096bytes", quiescent: true, native: { complete: true,
    profile: { entries: [{ name: "filesystem.gate_wait", calls: "2", elapsed_ns: "345", units: "4" }, { name: "EXCLUDED_MOCK_SECRET", calls: "1" }] } },
    process: { cpu_work: { user_us: "7", system_us: "8" }, cpu_observer: { user_us: "9", system_us: "10" },
      memory_end_bytes: { rss: "11", heapTotal: "12", heapUsed: "13", external: "14", arrayBuffers: "15", secret: "EXCLUDED_MOCK_SECRET" },
      resources_work: { voluntary_context_switches: "16", involuntary_context_switches: "17" } } }] }
  const projected = pilot.projectPilotRecord(fixture, { kind: "selected_native_file", native_sha256: "a".repeat(64) })
  const phase = projected.native_phases.phases[1]
  assert.equal(projected.native_used_identity, "verified", "native rejection is exercised after digest join")
  assert.deepEqual(phase.core_profile.entries.find((row) => row.name === "filesystem.gate_wait"), { name: "filesystem.gate_wait", calls: "2", elapsed_ns: "345", units: "4" })
  assert.equal(phase.process.cpu_observer.user_us, "9")
  assert.equal(phase.process.memory_end_bytes.rss, "11")
  assert.equal(phase.process.resources_work.voluntary_context_switches, "16")
  assert.equal(phase.core_profile.entries.find((row) => row.name === "filesystem.snapshot_nodes").calls, null)
  assert.equal(JSON.stringify(projected).includes("EXCLUDED_MOCK_SECRET"), false)
  assert.equal(projected.live_measurement_qualified, false, "joined identity cannot qualify missing/malformed workload evidence")
  assert.equal(projected.observation_status, "incomplete")
  fixture.providers[0].backingObserver.backing_evidence = { complete: true, journal: [
    { type: "boundary", id: "workload-4096bytes:begin", complete: true },
    { type: "boundary", id: "workload-4096bytes:end", complete: true }, { type: "interval", complete: true },
  ] }
  assert.equal(pilot.projectPilotRecord(fixture, { kind: "selected_native_file", native_sha256: "a".repeat(64) }).observation_status, "incomplete", "resource completion cannot repair malformed projected workload")
})

test("clean committed core row coverage excludes protected compact additions and unrelated labels", () => {
  // Independent frozen committed profile contract, rather than the dirty
  // recorder or the projection's own label list.
  const committedNames = modelCommittedProfileNames
  const fixture = resultFixture()
  fixture.providers[0].storageDiagnostics = { phases: [{ name: "workload-4096bytes", native: { complete: false,
    profile: { entries: [...committedNames.map((name) => ({ name, calls: "0", elapsed_ns: "0", units: "0" })),
      ...["compact.capture.guard_clones", "compact.capture.file_layout_clones", "compact.capture.extent_clones", "compact.create.namespace_cow_nodes", "compact.create.namespace_cow_layouts", "compact.create.namespace_cow_extents", "EXCLUDED_MOCK_SECRET"].map((name) => ({ name, calls: "99", elapsed_ns: "99", units: "99" }))] } } }] }
  const phase = pilot.projectPilotRecord(fixture, {}).native_phases.phases[1]
  assert.deepEqual(phase.core_profile.entries.map((row) => row.name), committedNames)
  assert.equal(phase.core_profile.status, "observed")
  assert.ok(phase.core_profile.entries.every((row) => row.calls === "0" && row.elapsed_ns === "0" && row.units === "0"))
  for (const row of phase.core_profile.entries) assert.equal(row.name.startsWith("compact.capture.") || row.name.startsWith("compact.create."), false)
  assert.equal(JSON.stringify(phase).includes("EXCLUDED_MOCK_SECRET"), false)
  fixture.providers[0].storageDiagnostics.phases[0].native.profile.entries.push({ name: "filesystem.gate_wait", calls: "1", elapsed_ns: "1", units: "1" })
  const duplicate = pilot.projectPilotRecord(fixture, {}).native_phases.phases[1]
  assert.equal(duplicate.core_profile.status, "unavailable_duplicate_required_labels")
  assert.equal(duplicate.core_profile.entries.find((row) => row.name === "filesystem.gate_wait").calls, null)
  assert.equal(duplicate.projected_component_complete, false)
  fixture.providers[0].storageDiagnostics.phases[0].native.r2 = { instances: Array.from({ length: 17 }, (_, index) => ({ id: String(index + 1) })) }
  const capped = pilot.projectPilotRecord(fixture, {}).native_phases.phases[1]
  assert.deepEqual(capped.raw_projection, { status: "truncated_unavailable", instance_count: 17, omitted_instances: 1 })
  assert.equal(capped.raw_instances.length, 16)
  assert.equal(capped.projected_component_complete, false)
})

// Pure schema models below never select or load a binary. The real test
// process remains capture_js_mock under the existing .node/network guards.
const modelDigest = "a".repeat(64)
const modelCommittedProfileNames = [
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

function modelNativeSnapshot(live) {
  const zero = (fields) => Object.fromEntries(fields.map((field) => [field, "0"]))
  const rawNames = ["put_opts.block_create", "get.block_read", "body_read.block_read", "get.conflict_verify", "body_read.conflict_verify", "get.migration", "body_read.migration", "head.direct_delete", "delete.direct", "delete.reconcile"]
  const rawFields = ["calls", "success", "error", "cancelled", "elapsed_ns", "attempted_bytes", "confirmed_bytes", "returned_bytes", "latency_max_ns"]
  const instance = {
    id: "7", ...zero(["puts", "gets", "deletes", "reconciles", "successes", "errors", "duration_ms_total", "bytes_read", "bytes_written", "conditional_conflicts", "id_collision_exhausted", "retry_exhausted", "cache_hits"]),
    raw_api: { schema: "mount-rs.object-store-api.v1", scope: "one_object_store_block_store_instance", saturated: false, in_flight: "0", pending_claims: "0", claims: zero(["leader_claims", "leader_success", "leader_error", "leader_cancelled", "follower_claims", "follower_success", "follower_error", "follower_cancelled"]), entries: rawNames.map((name) => ({ name, ...zero(rawFields), latency_log2_us: Array(32).fill("0") })) },
  }
  return JSON.stringify({
    schema_version: "mount-rs.storage-diagnostics.v3", enabled: true, scope: "process", quiescent_snapshot_required: true, elapsed_semantics: "inclusive_wall_nanoseconds",
    measurement: {
      storage_calls: STORAGE_CALL_SEMANTICS, storage_bytes: STORAGE_BYTE_SEMANTICS, storage_rows: STORAGE_ROW_SEMANTICS,
      storage_operations: STORAGE_OPERATION_NAMES, storage_families: STORAGE_OPERATION_FAMILIES, storage_instrumented_operations: STORAGE_INSTRUMENTED_OPERATION_NAMES, tidb_coverage: TIDB_DIAGNOSTIC_COVERAGE,
      storage_duration: "inclusive_wall_nanoseconds; nested_and_parallel_spans_overlap", forwarding_boxes: "enabled_napi_dynamic_provider_box_pin_site_calls_and_requested_future_object_bytes; excludes_allocator_overhead_and_other_allocations", profile: "existing_core_profile_counters", sqlite: "live_connection_pager_and_sql_category_counters; pager_bytes_are_page_size_estimates", r2: "live_store_logical_calls_and_cache_hits; not_http_attempts",
      r2_api: { schema: "mount-rs.object-store-api.v1", scope: "live_registered_split_r2_block_store_instances", calls: "object_store_adapter_method_invocations; not_http_attempts_or_internal_retries", duration: "inclusive_wall_nanoseconds_at_invoked_adapter_await; excludes_argument_preparation", upload_bytes: "attempted=submitted_payload; confirmed=put_opts_ok_only", returned_bytes: "successful_body_materialization_before_integrity_validation", latency_max: "cumulative_per_instance; exact_phase_max_unavailable", reconcile_listing: "unavailable", excluded: ["backing_marker_prepare_and_verify", "concurrent_prefix_probes", "qualification_and_preflight", "unregistered_rust_factories_and_mount_r2", "internal_client_retries"] },
      unavailable: { http_attempts: "unavailable", internal_successful_retries: "unavailable", physical_device_iops: "unavailable", tidb_pool_wait: "isolated_queue_only_wait_unavailable", native_allocation_count: "unavailable", js_allocation_count: "unavailable" },
      latency_histogram: { unit: "microseconds", intervals: Array.from({ length: 32 }, (_, bucket) => bucket === 0 ? { lower_inclusive_us: "0", upper_exclusive_us: "1" } : bucket === 31 ? { lower_inclusive_us: "1073741824", upper_exclusive_us: null, terminal_overflow: true } : { lower_inclusive_us: String(2 ** (bucket - 1)), upper_exclusive_us: String(2 ** bucket) }) },
    },
    backend_waits: { pglite_client_lock: "instrumented", tidb_pool: "instrumented_inclusive_checkout_including_lazy_connect_and_session_configuration" }, http_attempts: "unavailable", physical_device_iops: "unavailable",
    storage: { in_flight: "0", forwarding_boxes: { sites: "napi_dynamic_provider_forwarding_future", calls: "0", requested_object_bytes: "0" }, entries: STORAGE_OPERATION_NAMES.map((name) => ({ name, ...zero(["calls", "success", "error", "cancelled", "bytes", "returned_rows", "returned_row_observations", "in_flight", "elapsed_ns"]), latency_log2_us: Array(32).fill("0") })) },
    profile: { entries: modelCommittedProfileNames.map((name) => ({ name, ...zero(["calls", "elapsed_ns", "units"]) })) }, sqlite: { connections: [] }, r2: { scope: "process_live_instances", internal_successful_retries: "unavailable", instances: live ? [instance] : [] },
  })
}

function healthyModeledProjectionInput() {
  const native = JSON.parse(modelNativeSnapshot(true))
  const endpoint = { started: 0, ended: 0, cpuStart: { user: 0, system: 0 }, cpuEnd: { user: 0, system: 0 },
    resourcesStart: { voluntaryContextSwitches: 0, involuntaryContextSwitches: 0 }, resourcesEnd: { voluntaryContextSwitches: 0, involuntaryContextSwitches: 0 },
    memory: { rss: 0, heapTotal: 0, heapUsed: 0, external: 0, arrayBuffers: 0 }, native }
  const phase = diagnostics.finishPhase("workload-4096bytes", endpoint, structuredClone(endpoint))
  diagnostics.validateRawPhaseDiagnostics(phase)
  const benchmark = resultFixture()
  benchmark.status = "ok"; benchmark.results[0].status = "ok"; delete benchmark.results[0].failures; benchmark.results[0].rawSamples = []
  Object.assign(benchmark.results[0].summary, { elapsedMs: 1000, iops: 1200, iopsTargetMet: true, successfulIterations: 400, failedIterations: 0, successfulOperations: 1200,
    timeoutCount: 0, cleanupFailureCount: 0, operationSuccess: { verifiedReads: 400 } })
  const provider = benchmark.providers[0]
  provider.status = "ok"; provider.cleanup = { pathsAttempted: 0, remainingPaths: 0, failures: [], resource: { status: "ok" } }
  provider.backingPilotIdentity = { native_used_identity: "verified", native_sha256: modelDigest }
  provider.backingResourceCoverage["workload-4096bytes"] = "captured"
  provider.storageDiagnostics = { phases: [phase] }
  provider.backingObserver = { schema: "mount-rs.runner-backing-observer.v1", complete: true,
    terminal: { safe_to_continue_pair: true, native_quiescent: true, owned_operations_settled: true },
    backing_evidence: { schema: "mount-rs.backing-observer.v1", complete: true, journal: [
      { type: "boundary", id: "workload-4096bytes:begin", complete: true },
      { type: "boundary", id: "workload-4096bytes:end", complete: true }, { type: "interval", complete: true },
    ] } }
  return { benchmark, identity: { kind: "selected_native_file", native_sha256: modelDigest } }
}

test("pure healthy modeled projector reaches native join and complete evidence without any binary load", () => {
  const { benchmark, identity } = healthyModeledProjectionInput()
  const baseline = pilot.projectPilotRecord(benchmark, identity)
  assert.equal(baseline.native_used_identity, "verified")
  assert.equal(baseline.native_phases.phases[1].validated_workload, true)
  assert.equal(baseline.native_phases.phases[1].projected_component_complete, true)
  assert.equal(baseline.observation_status, "observed")
  assert.equal(baseline.live_measurement_qualified, true, "pure modeled branch coverage only; no real native proof")
  assert.equal(capture.__napiBindingTarget, "storage-benchmark-capture")
  assert.equal(Object.keys(require.cache).some((path) => path.endsWith(".node")), false)
})

test("pilot retains optional local work with fixed labels independently of raw qualification", () => {
  const { benchmark, identity } = healthyModeledProjectionInput()
  const instance = benchmark.providers[0].storageDiagnostics.phases[0].native.r2.instances[0]
  const names = ["sha256.digest", "block_id.encode", "copy.upload_payload", "copy.cache_insert", "copy.return_vec", "cache.lock_acquire", "put.follower_wait"]
  const baseline = pilot.projectPilotRecord(benchmark, identity)
  benchmark.providers[0].storageDiagnostics.phases[0].native.measurement.r2_local = structuredClone(diagnostics.OBJECT_STORE_LOCAL_MEASUREMENT)
  instance.local_work = { status: "observed", complete: true, schema: "mount-rs.object-store-local.v1", scope: "one_object_store_block_store_instance",
    saturated_start: false, saturated_end: false, in_flight_start: "0", in_flight_end: "0", private: "EXCLUDED_LOCAL_SECRET",
    entries: names.map((name, index) => {
      const calls = 2
      return { name, calls: String(calls), success: index === 6 ? "0" : String(calls), error: index === 6 ? "1" : "0", cancelled: index === 6 ? "1" : "0", elapsed_ns: String(calls * 1000), input_bytes: String(calls * (index === 1 ? 32 : index < 5 ? 4096 : 0)), output_bytes: String(calls * (index === 0 ? 32 : index === 1 ? 65 : index < 5 ? 4096 : 0)),
        latency_max_ns_start: "0", latency_max_ns_end: calls ? "1000" : "0", exact_phase_max_ns: "unavailable", latency_log2_us: ["0", String(calls), ...Array(30).fill("0")], private: "EXCLUDED_LOCAL_SECRET" }
    }) }
  const projected = pilot.projectPilotRecord(benchmark, identity)
  const local = projected.native_phases.phases[1].raw_instances[0].local_work
  assert.equal(local?.status, "observed", "pilot must retain optional local counters")
  assert.equal(local.complete, true)
  assert.deepEqual(local.entries.map((row) => row.name), names)
  assert.equal(local.entries[0].input_bytes, "8192")
  assert.equal(local.entries[0].output_bytes, "64")
  assert.equal(local.entries[1].output_bytes, "130")
  assert.equal(local.entries[4].output_bytes, "8192")
  assert.deepEqual([local.entries[6].error, local.entries[6].cancelled], ["1", "1"])
  assert.equal(JSON.stringify(projected).includes("EXCLUDED_LOCAL_SECRET"), false)
  assert.equal(projected.live_measurement_qualified, baseline.live_measurement_qualified, "local metrics do not alter the established raw qualification gate")
  const raw = projected.native_phases.phases[1].raw_instances[0]
  const { local_work: _local, ...existingRaw } = raw
  const { local_work: _baselineLocal, ...baselineRaw } = baseline.native_phases.phases[1].raw_instances[0]
  assert.deepEqual(existingRaw, baselineRaw)
  for (const metadata of [undefined, { schema: "EXCLUDED_LOCAL_SECRET" }]) {
    benchmark.providers[0].storageDiagnostics.phases[0].native.measurement.r2_local = metadata
    const missingMetadata = pilot.projectPilotRecord(benchmark, identity)
    assert.equal(missingMetadata.native_phases.phases[1].raw_instances[0].local_work.status, "invalid", "local rows alone cannot establish their measurement provenance")
    assert.equal(missingMetadata.native_phases.phases[1].raw_instances[0].local_work.complete, false)
    assert.equal(missingMetadata.live_measurement_qualified, baseline.live_measurement_qualified)
    assert.equal(JSON.stringify(missingMetadata).includes("EXCLUDED_LOCAL_SECRET"), false)
  }
  benchmark.providers[0].storageDiagnostics.phases[0].native.measurement.r2_local = structuredClone(diagnostics.OBJECT_STORE_LOCAL_MEASUREMENT)
  instance.local_work.entries.push({ ...instance.local_work.entries[0], name: names[0], private: "EXCLUDED_LOCAL_SECRET" })
  const duplicate = pilot.projectPilotRecord(benchmark, identity)
  assert.equal(duplicate.native_phases.phases[1].raw_instances[0].local_work.status, "invalid")
  assert.equal(duplicate.native_phases.phases[1].raw_instances[0].local_work.complete, false)
  assert.equal(duplicate.native_phases.phases[1].raw_instances[0].local_work.entries[0].calls, null)
  assert.equal(duplicate.live_measurement_qualified, baseline.live_measurement_qualified)
  assert.equal(JSON.stringify(duplicate).includes("EXCLUDED_LOCAL_SECRET"), false)
  instance.local_work = { status: "unavailable", complete: false, issues: ["EXCLUDED_LOCAL_SECRET"] }
  const unavailable = pilot.projectPilotRecord(benchmark, identity)
  assert.equal(unavailable.native_phases.phases[1].raw_instances[0].local_work.status, "unavailable")
  assert.equal(unavailable.native_phases.phases[1].raw_instances[0].local_work.entries, undefined)
  assert.equal(JSON.stringify(unavailable).includes("EXCLUDED_LOCAL_SECRET"), false)
})

for (const schema of ["unsupported", "missing"]) test(`pure modeled ${schema} backing schema stays unavailable and cannot qualify`, () => {
  const { benchmark, identity } = healthyModeledProjectionInput()
  assert.equal(pilot.projectPilotRecord(benchmark, identity).live_measurement_qualified, true, "healthy model prerequisite")
  if (schema === "missing") delete benchmark.providers[0].backingObserver.schema
  else benchmark.providers[0].backingObserver.schema = "EXCLUDED_UNSUPPORTED_SCHEMA"
  const projected = pilot.projectPilotRecord(benchmark, identity)
  assert.deepEqual(projected.backing, { status: "unavailable" })
  assert.equal(projected.observation_status, "incomplete", "unsupported exported schema cannot be observed")
  assert.equal(projected.live_measurement_qualified, false, "unsupported exported schema cannot qualify")
})

for (const mutation of ["native_incomplete", "duplicate_profile", "instance_cap"]) test(`pure modeled ${mutation} rejects native qualification with otherwise valid digest join`, () => {
  const { benchmark, identity } = healthyModeledProjectionInput()
  const phase = benchmark.providers[0].storageDiagnostics.phases[0]
  assert.equal(pilot.projectPilotRecord(benchmark, identity).live_measurement_qualified, true, "healthy model prerequisite")
  if (mutation === "native_incomplete") phase.native.complete = false
  else if (mutation === "duplicate_profile") phase.native.profile.entries.push(structuredClone(phase.native.profile.entries[0]))
  else {
    phase.native.r2.instances = Array.from({ length: 17 }, (_, index) => ({ ...structuredClone(phase.native.r2.instances[0]), id: String(index + 1) }))
    phase.native.r2.instance_ids_start = phase.native.r2.instances.map((entry) => entry.id)
    phase.native.r2.instance_ids_end = [...phase.native.r2.instance_ids_start]
    diagnostics.validateRawPhaseDiagnostics(phase)
  }
  const projected = pilot.projectPilotRecord(benchmark, identity)
  assert.equal(projected.native_used_identity, "verified", "digest join must remain independently valid")
  assert.equal(projected.observation_status, "incomplete")
  assert.equal(projected.live_measurement_qualified, false)
})

for (const seam of ["storage_row", "profile_row", "raw_instance"]) {
  for (const failureKind of ["recorded_read", "floor_only"]) test(`pure modeled null ${seam} retains ${failureKind} outcome before bounded optional fallback`, async () => {
    const { benchmark, identity } = healthyModeledProjectionInput()
    const failed = resultFixture()
    benchmark.status = "failed"; benchmark.results = failed.results
    const provider = benchmark.providers[0]
    provider.status = "failed"; provider.cleanup = failed.providers[0].cleanup
    if (failureKind === "floor_only") benchmark.results[0].rawSamples = []
    const original = pilot.projectPilotRecord(benchmark, identity)
    assert.equal(original.native_used_identity, "verified", "matching pure modeled digest join is a prerequisite")
    const expectedCode = failureKind === "recorded_read" ? "provider_operation_failed" : "iops_target_not_met"
    assert.equal(original.outcome.first_failure.code, expectedCode)
    const native = provider.storageDiagnostics.phases[0].native
    if (seam === "storage_row") native.storage.entries = [null]
    else if (seam === "profile_row") native.profile.entries = [null]
    else native.r2.instances = [null]
    const directory = await mkdtemp(join(tmpdir(), "mount-rs-pilot-null-projection-mock-"))
    try {
      const projected = pilot.projectPilotRecord(benchmark, identity)
      assert.equal(projected.native_used_identity, "verified")
      assert.ok(["unavailable", "incomplete"].includes(projected.native_phases.status), "malformed optional native rows must stay unavailable/incomplete")
      assert.equal(projected.observation_status, "incomplete")
      assert.equal(projected.live_measurement_qualified, false)
      assert.deepEqual(projected.outcome, original.outcome, "projection cannot replace first failure/counts/cleanup")
      const output = join(directory, "pilot.json")
      await pilot.writePilotRecord(output, projected)
      const bytes = await readFile(output), record = JSON.parse(bytes)
      assert.ok(bytes.length <= pilot.OUTPUT_CAP)
      assert.deepEqual(record.outcome, original.outcome)
      assert.equal(record.outcome.first_failure.code, expectedCode)
      assert.equal(record.outcome.successful_operations, 1197)
      assert.equal(record.outcome.timeout_count, 1)
      assert.equal(record.outcome.cleanup_failure_count, 1)
      assert.equal(record.outcome.remaining_paths, 1)
      assert.equal(bytes.includes("EXCLUDED_MOCK_SECRET"), false)
    } finally { await rm(directory, { recursive: true, force: true }) }
  })
}
