import assert from "node:assert/strict"
import { Buffer } from "node:buffer"
import { execFile } from "node:child_process"
import { createHash } from "node:crypto"
import { chmod, link, mkdir, mkdtemp, readFile, realpath, rm, symlink, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"
import test from "node:test"
import { assessOwnedLayoutEngineCapacity, buildOwnedLayoutHandoff, buildOwnedLayoutNativeSeal, buildOwnedLayoutOwnerTerminal } from "./owned-layout-handoff.mjs"

const moduleURL = new URL("./owned-layout-handoff.mjs", import.meta.url), modulePath = fileURLToPath(moduleURL)
const posix = { skip: !["linux", "darwin"].includes(process.platform) && "actual POSIX private-file evidence is supported only on Linux/macOS" }
const sha = (bytes) => createHash("sha256").update(bytes).digest("hex"), bytes = (value) => Buffer.from(`${JSON.stringify(value)}\n`)
const code = (suffix) => `OWNED_LAYOUT_HANDOFF_${suffix}`
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
function processBytes(action_id = "outer.comparison", raw_returncode = 0, pid = 22000) {
  return bytes({ schema: "mount-rs.owned-layout-process.v1", action_id, pid, pgid: pid, raw_returncode, normalized_exit: raw_returncode,
    supervisor_signal: null, child_return_signal: null, wait_unavailable_observed: false, deadline_exceeded: false, group_leak_observed: false,
    group_probe_unavailable_observed: false, retirement_signal_unavailable_observed: false, forced_process_retirement: false,
    retirement_signals: [], child_reaped: true, group_state: "absent", sticky_failure: raw_returncode !== 0,
    failure_code: raw_returncode === 0 ? null : "OWNED_LAYOUT_PROCESS_CHILD_FAILED" })
}
async function rootFixture(t) {
  const root = await realpath(await mkdtemp(join(tmpdir(), "mount-rs-handoff-cli-"))); await chmod(root, 0o700)
  t.after(() => rm(root, { force: true, recursive: true }))
  const evidence = join(root, "evidence"); await mkdir(evidence, { mode: 0o700 })
  async function retain(name, value, mode = 0o600) {
    const path = join(evidence, name); await writeFile(path, value, { mode }); return path
  }
  return { root, evidence, retain }
}
async function controllerFixture(t) {
  const model = await rootFixture(t), tidb_owner = "cli-tidb", rustfs_owner = "cli-rustfs"
  const fixture = { schema: "mount-rs.owned-backing-cids.v1", generation: "1", tidb_owner, rustfs_owner,
    entries: ROLES.map((role, index) => ({ role, cid: (index + 1).toString(16).padStart(64, "0"), labels: index === 7 ?
      { "com.mount-rs.rustfs-test": "mount-rs-rustfs-test", "com.mount-rs.rustfs-test-run": rustfs_owner } : { "mount-rs.tidb.run": tidb_owner } })) }
  const input = { fixtures: Buffer.from(`  ${JSON.stringify(fixture)}\n`), engine: Buffer.from('{ "socket_path" : "/var/run/docker.sock" }\n'),
    created: { tidb_owner, rustfs_owner, tidb_url: "mysql://root:PRIVATE_CLI_SECRET@127.0.0.1:24100/test", rustfs_endpoint: "http://127.0.0.1:24101", rustfs_bucket: "cli-bucket" },
    inherited: {}, scope_nonce: Buffer.alloc(16, 0x37) }
  const paths = { fixtures: await model.retain("fixtures.json", input.fixtures), engine: await model.retain("engine.json", input.engine),
    observations: await model.retain("controller-observations.json", bytes({ created: input.created, inherited: input.inherited, scope_nonce_hex: input.scope_nonce.toString("hex") })) }
  return { ...model, input, paths, expected: buildOwnedLayoutHandoff(input), command: "controller" }
}
async function sourceFiles(root, paths) {
  const directory = join(root, "checkout"); await mkdir(directory, { recursive: true, mode: 0o755 })
  const sources = {}
  for (const path of paths) {
    const target = join(directory, path); await mkdir(dirname(target), { recursive: true, mode: 0o755 })
    sources[path] = Buffer.from(`MODEL_READONLY_SOURCE ${path}\n`); await writeFile(target, sources[path], { mode: 0o644 })
  }
  return { directory, sources }
}
async function buildFixture(t) {
  const model = await rootFixture(t), source = await sourceFiles(model.root, SOURCE_PATHS)
  const input = { checkout: { revision: "b".repeat(40), clean_before: true, clean_after: true },
    build: { locked: true, release: true, no_js: true, exit_code: 0 }, native: Buffer.from("MODEL_NEVER_LOADED_NATIVE"), sources: source.sources }
  return { ...model, input, expected: buildOwnedLayoutNativeSeal(input), command: "build", paths: {
    checkout: await model.retain("checkout.json", bytes(input.checkout)), action: await model.retain("build-action.json", bytes(input.build)),
    native: await model.retain("selected.node", input.native, 0o644), "checkout-root": source.directory } }
}
async function capacityFixture(t, row = "10737418240 4\n") {
  const model = await rootFixture(t), input = { engine: Buffer.from('{ "socket_path" : "/var/run/docker.sock" }\n'), observation: Buffer.from(row), action: processBytes("engine.capacity") }
  return { ...model, input, expected: assessOwnedLayoutEngineCapacity(input), command: "capacity", paths: {
    engine: await model.retain("engine.json", input.engine), observation: await model.retain("capacity-output", input.observation), action: await model.retain("capacity-action.json", input.action) } }
}
function rawAction(phase, kind, target, index) {
  const id = kind === "container" ? target.cid : kind === "volume" ? target.name : target.id
  const output = phase === "absence" ? Buffer.from(kind === "container" ? `Error response from daemon: No such container: ${id}\n` : kind === "volume" ?
    `Error response from daemon: get ${id}: no such volume\n` : `Error response from daemon: network ${id} not found\n`) : phase === "remove" ? Buffer.from(`${id}\n`) :
    bytes(kind === "container" ? { id, name: `/${target.name}`, labels: target.labels } : kind === "volume" ? { name: id, labels: target.labels, created_at: target.created_at, driver: target.driver, scope: target.scope } : { id, name: target.name, labels: target.labels })
  return { phase, action_kind: phase === "absence" ? "inspect_absence" : phase === "remove" ? "remove" : "inspect_owner", output,
    process: processBytes(`${kind}.${phase}`, phase === "absence" ? 1 : 0, 23000 + index), container_delete_mode: kind === "container" && phase === "remove" ? "force_owned_resource" : null }
}
async function terminalFixture(t, { emptyFailure = false, early = false } = {}) {
  const model = await controllerFixture(t), built = await buildFixture(t), capacity = await capacityFixture(t), fixture = JSON.parse(model.input.fixtures)
  const producer = await sourceFiles(join(model.root, "producer"), PRODUCER_PATHS)
  const producer_receipt = bytes({ schema: "mount-rs.owned-layout-producer-sources.v1", checkout_sha: "b".repeat(40), source_sha256: Object.fromEntries(PRODUCER_PATHS.map((path) => [path, sha(producer.sources[path])])) })
  const resources = { schema: "mount-rs.owned-layout-resources.v1", generation: "1", tidb_owner: fixture.tidb_owner, rustfs_owner: fixture.rustfs_owner,
    containers: fixture.entries.map((row) => ({ ...row, name: row.role === "rustfs-service" ? fixture.rustfs_owner : `mount-rs-tidb-${fixture.tidb_owner}-${row.role.replace("-", "")}`, creation_state: "observed" })),
    volumes: ROLES.slice(0, 6).map((role) => ({ role, creation_state: "observed", name: `mount-rs-tidb-${fixture.tidb_owner}-${role.replace("-", "")}-data`, created_at: "2026-09-27T00:00:00Z", driver: "local", scope: "local", labels: { "mount-rs.tidb.run": fixture.tidb_owner } })),
    network: { id: "9".repeat(64), creation_state: "observed", name: `mount-rs-tidb-net-${fixture.tidb_owner}`, labels: { "mount-rs.tidb.run": fixture.tidb_owner } },
    rustfs_bind: { owner: fixture.rustfs_owner, marker_verified: true, path_validated: true, data_path: "/private/model/mount-rs-rustfs.test/data", data_empty: true, run_dir_removed: true }, helper: null }
  const targets = [...resources.containers.map((target) => ({ kind: "container", target, id: target.cid, owner: target.role === "rustfs-service" ? fixture.rustfs_owner : fixture.tidb_owner })),
    ...resources.volumes.map((target) => ({ kind: "volume", target, id: target.name, owner: fixture.tidb_owner })), { kind: "network", target: resources.network, id: resources.network.id, owner: fixture.tidb_owner }]
  const observations = targets.map((row, index) => ({ kind: row.kind, id: row.id, owner: row.owner, before_owner_verified: true, after_absent: true,
    actions: ["inspect", "remove", "absence"].map((phase, actionIndex) => rawAction(phase, row.kind, row.target, index * 3 + actionIndex)) }))
  if (emptyFailure) { observations[0].actions[1].output = Buffer.alloc(0); observations[0].actions[1].process = processBytes("container.remove", 1, 23001) }
  const artifacts = { projection: bytes({ schema: "mount-rs.owned-layout-entry.v1", runtime_scope: "modeled_controls", comparison: { complete: true, safe_to_continue: true, native_uncertainty: false } }),
    originals: bytes({ schema: "mount-rs.owned-layout-originals.v1", runtime_scope: "modeled_controls", evidence: { complete: true, arms: [] } }),
    fixtures: model.input.fixtures, controller: Buffer.from(model.expected.private_json), engine: model.input.engine, build: Buffer.from(built.expected.private_json), native: built.input.native, capacity: Buffer.from(capacity.expected.private_json) }
  if (early) {
    for (const key of ["projection", "originals", "fixtures", "controller"]) artifacts[key] = null
    resources.tidb_owner = resources.rustfs_owner = null
    for (const row of resources.containers) Object.assign(row, { creation_state: "not_created", cid: null, name: null, labels: null })
    for (const row of resources.volumes) Object.assign(row, { creation_state: "not_created", name: null, labels: null, created_at: null, driver: null, scope: null })
    Object.assign(resources.network, { creation_state: "not_created", id: null, name: null, labels: null }); resources.rustfs_bind = null; observations.length = 0
  }
  const groups = { outer: early ? null : processBytes(), actions: observations.flatMap((row) => row.actions.map((action) => action.process)) }
  const input = { artifacts, resources, observations, groups, producer_sources: producer.sources, producer_receipt }, expected = buildOwnedLayoutOwnerTerminal(input)
  assert.equal(expected.status, emptyFailure || early ? "incomplete" : "ready", "controlled terminal fixture must satisfy the reviewed pure source contract")
  const artifactPaths = {}
  for (const [key, value] of Object.entries(artifacts)) artifactPaths[key] = value === null ? null : await model.retain(`terminal-${key}`, value, key === "native" ? 0o644 : 0o600)
  const processPaths = [], observationPaths = []
  for (const [index, row] of observations.entries()) {
    const actions = []
    for (const [actionIndex, action] of row.actions.entries()) {
      const process = await model.retain(`process-${index}-${actionIndex}.json`, action.process), output = await model.retain(`output-${index}-${actionIndex}`, action.output)
      processPaths.push(process); actions.push({ ...action, process, output })
    }
    observationPaths.push({ ...row, actions })
  }
  const groupPaths = { outer: groups.outer === null ? null : await model.retain("outer-process.json", groups.outer), actions: processPaths }
  return { ...model, input, expected, command: "terminal", artifacts: artifactPaths, observationPaths, groupPaths, paths: {
    artifacts: await model.retain("artifacts-descriptor.json", bytes(artifactPaths)), resources: await model.retain("resources.json", bytes(resources)),
    observations: await model.retain("observations-descriptor.json", bytes(observationPaths)), groups: await model.retain("groups-descriptor.json", bytes(groupPaths)),
    "producer-receipt": await model.retain("producer-sources.json", producer_receipt), "producer-checkout": producer.directory } }
}
const argv = (model) => [model.command, ...Object.entries(model.paths).flatMap(([key, value]) => [`--${key}`, value])]

async function child(t, model, { args = argv(model), importOnly = false, directPath = modulePath, mutateAfterRead = null } = {}) {
  const guardRoot = await realpath(await mkdtemp(join(tmpdir(), "mount-rs-handoff-deny-"))); await chmod(guardRoot, 0o700)
  t.after(() => rm(guardRoot, { force: true, recursive: true }))
  const guard = join(guardRoot, "deny.cjs"), prefix = `${model.root}/`
  await writeFile(guard, `const Module=require('node:module'),cp=require('node:child_process'),fs=require('node:fs'),fsp=require('node:fs/promises');
const root=${JSON.stringify(prefix)},mutation=${JSON.stringify(mutateAfterRead)},stats={opened:0,closed:0,unsafe_flags:0,changed:false};
const deny=()=>{process.stderr.write('FORBIDDEN_DISPATCH\\n');throw Error('FORBIDDEN_DISPATCH')};
const open=fsp.open.bind(fsp),read=fsp.readFile.bind(fsp),write=fsp.writeFile.bind(fsp);let firstClosed=false;
const watched=p=>typeof p==='string'&&p.startsWith(root);
fsp.open=async(p,flags,...rest)=>{if(firstClosed&&mutation&&!stats.changed&&p!==mutation){stats.changed=true;await write(mutation,Buffer.concat([await read(mutation),Buffer.from('changed-after-own-read')]));}
if(watched(p)&&(!Number.isInteger(flags)||(flags&fs.constants.O_NOFOLLOW)!==fs.constants.O_NOFOLLOW||(flags&fs.constants.O_NONBLOCK)!==fs.constants.O_NONBLOCK||(flags&(fs.constants.O_WRONLY|fs.constants.O_RDWR|fs.constants.O_CREAT|fs.constants.O_TRUNC|fs.constants.O_APPEND))!==0)){stats.unsafe_flags++;deny();}
const h=await open(p,flags,...rest);if(watched(p)){stats.opened++;const close=h.close.bind(h);h.close=async()=>{await close();stats.closed++;if(p===mutation)firstClosed=true;};}return h;};
for(const key of ['writeFile','appendFile','rename','unlink','rm','mkdir','chmod','chown','truncate','copyFile','link','symlink']){if(fsp[key])fsp[key]=deny;if(fs[key])fs[key]=deny;if(fs[key+'Sync'])fs[key+'Sync']=deny;}
fsp.readFile=(p,...rest)=>watched(p)?deny():read(p,...rest);const readSync=fs.readFileSync.bind(fs);fs.readFileSync=(p,...rest)=>watched(p)?deny():readSync(p,...rest);
Module._extensions['.node']=deny;for(const key of ['exec','execSync','execFile','execFileSync','spawn','spawnSync','fork'])cp[key]=deny;
for(const [name,keys] of [['node:http',['request','get']],['node:https',['request','get']],['node:net',['connect','createConnection']],['node:tls',['connect']],['node:dgram',['createSocket']]]){const m=require(name);for(const key of keys)m[key]=deny;}
require('node:net').Socket.prototype.connect=deny;require('node:net').Server.prototype.listen=deny;global.fetch=deny;Module.syncBuiltinESMExports();
Module.registerHooks({resolve(s,c,n){if(/owned-layout-entry\\.mjs|storage\\/(runner|providers)\\.mjs|mount-rs-napi|\\.node$/.test(s))deny();return n(s,c);}});
process.on('exit',()=>process.stderr.write('HANDOFF_GUARD '+JSON.stringify(stats)+'\\n'));
`, { mode: 0o600 })
  const launch = importOnly ? ["--require", guard, "--input-type=module", "-e", `await import(${JSON.stringify(moduleURL.href)});console.log('INERT_IMPORT_OK')`] : ["--require", guard, directPath, ...args]
  return new Promise((resolveResult) => execFile(process.execPath, launch, { timeout: 5000, maxBuffer: 1048576, env: { PATH: process.env.PATH, LANG: "C" } },
    (error, stdout, stderr) => {
      const match = /^HANDOFF_GUARD (.+)\n$/mu.exec(stderr)
      resolveResult({ code: error ? error.code : 0, signal: error?.signal ?? null, killed: error?.killed ?? false, stdout,
        stderr: match ? stderr.replace(match[0], "") : stderr, guard: match ? JSON.parse(match[1]) : null })
    }))
}
function settled(result) {
  assert.equal(result.signal, null, "owned CLI child signal is failure"); assert.equal(result.killed, false, "owned CLI child timeout is failure")
  assert.notEqual(result.guard, null, "owned child must retain its final denied-dispatch/handle accounting")
  assert.equal(result.guard.opened, result.guard.closed, "every owned CLI handle must close on success/failure")
  assert.equal(result.guard.unsafe_flags, 0, "CLI reads must use NOFOLLOW|NONBLOCK")
  assert.doesNotMatch(result.stderr, /FORBIDDEN_DISPATCH|PRIVATE_CLI_SECRET|mysql:|127\.0\.0\.1|\/private\//u)
}
function receipt(result, expected, minimumReads = 3) {
  settled(result)
  assert.notEqual(result.stdout, "", "actual CLI must publish its nonempty private receipt; empty exit0 is not success")
  assert.equal(result.code, expected.status === "ready" ? 0 : 1)
  assert.equal(result.stderr, expected.status === "ready" ? "" : `${expected.public.failure_code}\n`)
  assert.equal(sha(Buffer.from(result.stdout)), expected.public.sha256, "CLI must emit exact original private_json bytes without a wrapper/extra newline")
  assert.ok(result.guard.opened >= minimumReads, "CLI must actually read its bounded evidence")
  return JSON.parse(result.stdout)
}
function refusal(result, suffix) {
  settled(result)
  assert.equal(result.stderr, `${code(suffix)}\n`, "actual CLI must emit its fixed refusal code")
  assert.equal(result.code, 1); assert.equal(result.stdout, "", "invalid inputs must not emit private receipt fragments")
}

test("controlled CLI fixture has exact28seams/7producers and retains modeled scope only", posix, async (t) => {
  const model = await terminalFixture(t)
  assert.equal(SOURCE_PATHS.length, 28); assert.equal(PRODUCER_PATHS.length, 7)
  assert.equal(model.expected.public.hosted_qualified, false); assert.equal(JSON.parse(model.input.artifacts.projection).runtime_scope, "modeled_controls")
  assert.equal(model.observationPaths[0].actions[0].process, model.groupPaths.actions[0]); assert.notEqual(sha(model.input.artifacts.fixtures), sha(bytes(JSON.parse(model.input.artifacts.fixtures))))
})
test("normal import stays inert under native/network/process/filesystem-write deny guards", posix, async (t) => {
  const model = await controllerFixture(t), result = await child(t, model, { importOnly: true }); settled(result)
  assert.equal(result.code, 0); assert.equal(result.stdout, "INERT_IMPORT_OK\n"); assert.equal(result.stderr, ""); assert.equal(result.guard.opened, 0)
})
for (const [name, make, reads] of [["controller", controllerFixture, 3], ["build", buildFixture, 31], ["capacity", capacityFixture, 3], ["terminal", terminalFixture, 20]])
  test(`actual ${name} CLI joins readonly bytes and publishes exact private receipt`, posix, async (t) => {
    const model = await make(t), observed = receipt(await child(t, model), model.expected, reads)
    assert.equal(observed.schema, JSON.parse(model.expected.private_json).schema)
  })
test("actual capacity CLI retains belowfloor values/incomplete status without weakening floors", posix, async (t) => {
  const model = await capacityFixture(t, "10737418240 3\n"), observed = receipt(await child(t, model), model.expected)
  assert.equal(observed.cpu_count, 3); assert.equal(observed.qualified, false); assert.equal(observed.min_cpu_count, 4)
})
test("actual terminal CLI permits empty failed-command output and retains original failure/incomplete", posix, async (t) => {
  const model = await terminalFixture(t, { emptyFailure: true }), observed = receipt(await child(t, model), model.expected, 20)
  assert.equal(observed.cleanup.complete, false); assert.ok(observed.operation_error_count > 0)
  assert.equal(observed.observations[0].actions[1].observation.output_base64, "")
  assert.equal(observed.observations[0].actions[1].observation.process.failure_code, "OWNED_LAYOUT_PROCESS_CHILD_FAILED")
})
test("actual terminal CLI keeps explicit unavailable early artifactjoins as incomplete", posix, async (t) => {
  const model = await terminalFixture(t, { early: true }), observed = receipt(await child(t, model), model.expected, 16)
  assert.equal(observed.joins.projection_sha256, null); assert.equal(observed.joins.originals_sha256, null); assert.equal(observed.cleanup.complete, false)
})
for (const kind of ["leaf", "ancestor"]) test(`direct ${kind} executable alias invokes main and emits exact receipt`, posix, async (t) => {
  const model = await controllerFixture(t); let directPath
  if (kind === "leaf") { directPath = join(model.root, "handoff-link.mjs"); await symlink(modulePath, directPath) }
  else { const alias = join(model.root, "scripts-alias"); await symlink(dirname(modulePath), alias); directPath = join(alias, "owned-layout-handoff.mjs") }
  receipt(await child(t, model, { directPath }), model.expected)
})
for (const [name, change] of [["missing", () => []], ["duplicate", (model) => [...argv(model), "--engine", model.paths.engine]], ["unknown", (model) => [...argv(model), "--other", model.paths.engine]]])
  test(`actual CLI refuses ${name} arguments with fixed redacted error`, posix, async (t) => {
    const model = await capacityFixture(t); refusal(await child(t, model, { args: change(model) }), "ARGUMENTS_INVALID")
    if (name === "unknown") refusal(await child(t, model, { args: ["constructor", "--engine", model.paths.engine] }), "ARGUMENTS_INVALID")
  })
for (const [name, mutate] of [
  ["nonprivate JSON mode", async (model) => chmod(model.paths.engine, 0o644)],
  ["nonprivate direct parent", async (model) => chmod(model.evidence, 0o755)],
  ["leaf symlink", async (model) => { await rm(model.paths.action); await symlink(model.paths.engine, model.paths.action) }],
  ["ancestor symlink", async (model) => { const alias = join(model.root, "evidence-alias"); await symlink(model.evidence, alias); model.paths.action = join(alias, "capacity-action.json") }],
  ["hardlink alias", async (model) => { await rm(model.paths.action); await link(model.paths.engine, model.paths.action) }],
  ["nonregular input", async (model) => { await rm(model.paths.action); await mkdir(model.paths.action, { mode: 0o700 }) }],
  ["raw observation cap", async (model) => writeFile(model.paths.observation, Buffer.alloc(16385, 32))],
]) test(`actual CLI refuses ${name} and closes owned handles`, posix, async (t) => {
  const model = await capacityFixture(t); await mutate(model); refusal(await child(t, model), "FILE_INVALID")
})
test("actual controller CLI rejects duplicate decoded observations key before value assignment", posix, async (t) => {
  const model = await controllerFixture(t), original = JSON.parse(await readFile(model.paths.observations))
  await writeFile(model.paths.observations, `{"created":${JSON.stringify(original.created)},"creat\\u0065d":${JSON.stringify(original.created)},"inherited":{},"scope_nonce_hex":"${original.scope_nonce_hex}"}`)
  refusal(await child(t, model), "JSON_INVALID")
})
test("actual build CLI rejects a source ancestor escape without loading native", posix, async (t) => {
  const model = await buildFixture(t), directory = join(model.paths["checkout-root"], "src"), external = join(model.root, "external-src")
  await mkdir(join(external, "diagnostics"), { recursive: true, mode: 0o755 })
  for (const path of ["src/diagnostics/storage.rs", "src/diagnostics/profile.rs"]) await writeFile(join(external, path.slice(4)), model.input.sources[path], { mode: 0o644 })
  await rm(directory, { recursive: true }); await symlink(external, directory)
  refusal(await child(t, model), "FILE_INVALID")
})
test("actual build CLI rejects addon/source identity alias across all read roles", posix, async (t) => {
  const model = await buildFixture(t); await rm(model.paths.native); await link(join(model.paths["checkout-root"], "Cargo.toml"), model.paths.native)
  refusal(await child(t, model), "FILE_INVALID")
})
test("actual build CLI rechecks source changed after its own read and closes full owned readset", posix, async (t) => {
  const model = await buildFixture(t), mutation = join(model.paths["checkout-root"], "Cargo.toml")
  const result = await child(t, model, { mutateAfterRead: mutation }); refusal(result, "FILE_INVALID")
  assert.equal(result.guard.changed, true, "owned interleaving must mutate the already-read source")
})
test("actual terminal CLI checks exact7producer bytes beyond the native28seam inventory", posix, async (t) => {
  const model = await terminalFixture(t); await writeFile(join(model.paths["producer-checkout"], PRODUCER_PATHS[0]), "changed-producer-boundary")
  refusal(await child(t, model), "SOURCE_INVALID")
})
