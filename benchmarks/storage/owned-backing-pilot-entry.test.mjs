import assert from "node:assert/strict"
import { execFile } from "node:child_process"
import { chmod, mkdtemp, readFile, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { dirname, join, resolve } from "node:path"
import { fileURLToPath, pathToFileURL } from "node:url"
import { promisify } from "node:util"
import test from "node:test"

const execute = promisify(execFile)
const directory = dirname(fileURLToPath(import.meta.url))
const entry = join(directory, "owned-backing-pilot.mjs")
const providerUrl = pathToFileURL(join(directory, "providers.mjs")).href

test("actual pilot CLI finishes module evaluation before dynamically importing its runner", async (context) => {
  assert.match(process.version, /^v24\./u, "entry regression requires the hosted Node 24 runtime")
  const root = await mkdtemp(join(tmpdir(), "mount-rs-pilot-entry-"))
  await chmod(root, 0o700)
  const privateFile = async (name, contents) => {
    const path = join(root, name)
    await writeFile(path, contents, { mode: 0o600 })
    return path
  }
  try {
    const roles = ["pd-1", "pd-2", "pd-3", "tikv-1", "tikv-2", "tikv-3", "tidb", "rustfs-service"]
    const receipt = await privateFile("receipt.json", JSON.stringify({
      schema: "mount-rs.owned-backing-cids.v1", tidb_owner: "mock-tidb", rustfs_owner: "mock-rustfs", generation: "1",
      entries: roles.map((role, index) => ({ cid: (index + 1).toString(16).padStart(64, "0"), role,
        labels: role === "rustfs-service" ? { "com.mount-rs.rustfs-test": "mount-rs-rustfs-test", "com.mount-rs.rustfs-test-run": "mock-rustfs" } : { "mount-rs.tidb.run": "mock-tidb" } })),
    }))
    const capability = await privateFile("engine.json", JSON.stringify({ socket_path: join(root, "socket-must-never-open") }))
    // This selected file is only hashed. The child denies every native load.
    const selection = await privateFile("explicit-inert-mock.node", "mock selection; must never be loaded as a native addon\n")
    const output = join(root, "pilot.json")
    const mockProviders = `
      export const DEFAULT_CHUNK_SIZE_BYTES = 65536;
      export const MOUNTX_PINNED_REVISION = 'explicit-inert-mock';
      export const loadedNativeAddonPaths = () => [];
      export const mountxSourcePath = () => ${JSON.stringify(root)};
      export const nativeStorageDiagnostics = () => null;
      export const providerSummary = (definition) => ({ id: definition.id, chunking: definition.chunking });
      export const providerById = () => new Map([['mount-rs-split-tidb-r2', {
        id: 'mount-rs-split-tidb-r2', chunking: { algorithm: 'fixed-size' }, requiredEnvVars: [],
        availability() { process.stderr.write('PILOT_ENTRY_INERT_PROVIDER_REACHED\\n'); return { configured: false, missing: ['explicit_inert_mock'] }; },
        create() { throw new Error('PILOT_ENTRY_CONSTRUCTOR_MUST_NEVER_RUN'); }
      }]]);
    `
    const preload = await privateFile("inert-preload.mjs", `
      import Module, { registerHooks, syncBuiltinESMExports } from 'node:module';
      import childProcess from 'node:child_process';
      import http from 'node:http';
      import https from 'node:https';
      import net from 'node:net';
      import tls from 'node:tls';
      import dgram from 'node:dgram';
      const forbidden = { native: 0, network: 0, child_process: 0 };
      const childProcessAttempts = [];
      Module._extensions['.node'] = () => { forbidden.native++; throw new Error('PILOT_ENTRY_NATIVE_FORBIDDEN'); };
      const denyNetwork = () => { forbidden.network++; throw new Error('PILOT_ENTRY_NETWORK_FORBIDDEN'); };
      for (const [object, keys] of [[http, ['request', 'get']], [https, ['request', 'get']], [net, ['connect', 'createConnection']], [net.Socket.prototype, ['connect']], [net.Server.prototype, ['listen']], [tls, ['connect']], [dgram, ['createSocket']]]) {
        for (const key of keys) object[key] = denyNetwork;
      }
      globalThis.fetch = denyNetwork;
      for (const key of ['exec', 'execSync', 'execFile', 'execFileSync', 'spawn', 'spawnSync', 'fork']) {
        childProcess[key] = (file, args, options) => {
          forbidden.child_process++;
          childProcessAttempts.push({ api: key, file, args, cwd: options?.cwd });
          throw new Error('PILOT_ENTRY_CHILD_PROCESS_FORBIDDEN');
        };
      }
      syncBuiltinESMExports();
      registerHooks({ load(url, context, nextLoad) {
        if (url === ${JSON.stringify(providerUrl)}) return { format: 'module', shortCircuit: true, source: ${JSON.stringify(mockProviders)} };
        return nextLoad(url, context);
      }});
      process.on('exit', () => process.stderr.write('PILOT_ENTRY_INERT_GUARDS ' + JSON.stringify({ ...forbidden, child_process_attempts: childProcessAttempts, actual_native_loads: 0, actual_network_calls: 0, actual_child_processes: 0 }) + '\\n'));
    `)
    let result
    try {
      result = await execute(process.execPath, ["--import", preload, entry, "run"], {
        cwd: resolve(directory, "../.."), timeout: 15_000, maxBuffer: 1024 * 1024,
        env: {
          PATH: process.env.PATH, TMPDIR: root,
          NAPI_RS_NATIVE_LIBRARY_PATH: selection, MOUNT_RS_PROFILE_IO: "1", MOUNT_RS_TRACE_STORAGE: "0",
          MOUNT_RS_BACKING_CID_RECEIPT: receipt, MOUNT_RS_BACKING_ENGINE_CAPABILITY: capability,
          MOUNT_RS_BACKING_PILOT_OUTPUT: output, MOUNT_RS_BACKING_GENERATION: "1",
          MOUNT_RS_BACKING_TIDB_OWNER: "mock-tidb", MOUNT_RS_BACKING_RUSTFS_OWNER: "mock-rustfs",
          MOUNT_RS_BACKING_EXPECT_TIDB_URL: "mysql://127.0.0.1:4000/mock", MOUNT_RS_TIDB_URL: "mysql://127.0.0.1:4000/mock",
          MOUNT_RS_BACKING_EXPECT_R2_ENDPOINT: "http://127.0.0.1:9000/", MOUNT_RS_R2_ENDPOINT: "http://127.0.0.1:9000/",
          MOUNT_RS_BACKING_EXPECT_R2_BUCKET: "mock-bucket", MOUNT_RS_R2_BUCKET: "mock-bucket",
        },
      })
      result.code = 0
    } catch (error) { result = error }
    assert.equal(result.code, 1, `an unavailable inert provider must fail with an artifact, not leave the CLI import promise unsettled: ${result.stderr}`)
    assert.match(result.stderr, /PILOT_ENTRY_INERT_PROVIDER_REACHED/u)
    assert.doesNotMatch(result.stderr, /PILOT_ENTRY_CONSTRUCTOR_MUST_NEVER_RUN/u)
    const guards = JSON.parse(result.stderr.match(/PILOT_ENTRY_INERT_GUARDS (\{[^\n]+\})/u)?.[1] || "null")
    assert.equal(guards.native, 0)
    assert.equal(guards.network, 0)
    const metadataArguments = [["rev-parse", "HEAD"], ["status", "--porcelain=v1", "--untracked-files=all"]]
    assert.equal(guards.child_process, 4)
    assert.deepEqual(guards.child_process_attempts, [resolve(directory, "../.."), root].flatMap((cwd) =>
      metadataArguments.map((args) => ({ api: "execFile", file: "git", args, cwd }))))
    assert.equal(guards.actual_native_loads, 0)
    assert.equal(guards.actual_network_calls, 0)
    assert.equal(guards.actual_child_processes, 0)
    const artifact = JSON.parse(await readFile(output, "utf8"))
    assert.equal(artifact.outcome.provider_status, "skipped")
    assert.equal(artifact.native_used_identity, "unverified")
    assert.equal(artifact.identity.real_native_proof, "unverified")
    assert.equal(artifact.live_measurement_qualified, false)
    assert.match(result.stdout, /^OWNED_BACKING_PILOT /mu)
    context.diagnostic(JSON.stringify({
      scope: "actual_repository_entry_with_inert_provider_and_guarded_boundaries",
      child_exit_code: result.code, provider_constructor_started: false,
      actual_native_loads: guards.actual_native_loads, actual_network_calls: guards.actual_network_calls,
      denied_git_metadata_probes: guards.child_process,
      actual_child_processes: guards.actual_child_processes,
      provider_status: artifact.outcome.provider_status, live_measurement_qualified: artifact.live_measurement_qualified,
    }))
  } finally { await rm(root, { recursive: true, force: true }) }
})
