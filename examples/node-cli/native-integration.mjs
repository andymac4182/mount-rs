#!/usr/bin/env node

// Bounded, opt-in process-level integration for the Node SDK CLI. This test
// deliberately uses a host-backed root so persistence can be checked without
// credentials or a remote provider. The CLI owns the native mount; a second
// Node process performs the mounted I/O.

import assert from "node:assert/strict";
import { execFile, spawn } from "node:child_process";
import { mkdir, mkdtemp, readFile, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);
const exampleDirectory = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = resolve(exampleDirectory, "../..");
const cliPath = resolve(exampleDirectory, "index.mjs");
const optIn = "MOUNT_RS_NODE_CLI_NATIVE_INTEGRATION";
const mountWaitMs = 60_000;
const stopWaitMs = 20_000;
const clientWaitMs = 15_000;
const pollMs = 100;
const cliKeepaliveSource = `
const keepAlive = setInterval(() => {}, 60_000);
try {
  await import(process.argv[1]);
} finally {
  clearInterval(keepAlive);
}
`;

const clientSource = `
import assert from "node:assert/strict";
import { readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";

const [mountpoint, filename, seed, first, second] = process.argv.slice(2);
const seedPath = join(mountpoint, "seed.txt");
const dataPath = join(mountpoint, filename);
assert.equal(await readFile(seedPath, "utf8"), seed, "mounted seed readback");
await writeFile(dataPath, first, { encoding: "utf8", flag: "wx" });
assert.equal(await readFile(dataPath, "utf8"), first, "mounted write readback");
await writeFile(dataPath, second, { encoding: "utf8" });
assert.equal(await readFile(dataPath, "utf8"), second, "mounted rewrite readback");
console.log("PASS independent Node client mounted read/write");
`;

function skip(reason) {
  console.log(`SKIP node-cli-native gate=${reason}`);
  console.log("SUMMARY node-cli-native pass=0 skip=1 fail=0");
  process.exitCode = 0;
}

function appendOutput(state, field, chunk) {
  state[field] += chunk;
  // A native mount should be quiet. Keep diagnostics bounded if a host helper
  // unexpectedly emits a large amount of output.
  if (state[field].length > 64 * 1024) {
    state[field] = state[field].slice(-64 * 1024);
  }
}

function startCaptured(command, args, options = {}) {
  const child = spawn(command, args, {
    cwd: repositoryRoot,
    env: { ...process.env, NO_COLOR: "1" },
    stdio: [options.stdin !== undefined || options.keepStdin ? "pipe" : "ignore", "pipe", "pipe"],
  });
  const output = { stdout: "", stderr: "" };
  child.stdout.setEncoding("utf8");
  child.stderr.setEncoding("utf8");
  child.stdout.on("data", (chunk) => appendOutput(output, "stdout", chunk));
  child.stderr.on("data", (chunk) => appendOutput(output, "stderr", chunk));
  const exit = new Promise((resolveExit, rejectExit) => {
    child.once("error", rejectExit);
    child.once("close", (code, signal) => resolveExit({ code, signal }));
  });
  if (options.stdin !== undefined) child.stdin.end(options.stdin);
  return { child, exit, output };
}

function withTimeout(promise, timeoutMs, label) {
  let timer;
  return Promise.race([
    promise,
    new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(`${label} exceeded ${timeoutMs}ms`)), timeoutMs);
    }),
  ]).finally(() => clearTimeout(timer));
}

async function waitFor(predicate, timeoutMs, label) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (await predicate()) return;
    await new Promise((resolvePromise) => setTimeout(resolvePromise, pollMs));
  }
  throw new Error(`${label} exceeded ${timeoutMs}ms`);
}

async function isMounted(mountpoint) {
  if (process.platform === "linux") {
    const mounts = await readFile("/proc/self/mounts", "utf8");
    return mounts.split("\n").some((line) => line.trim().split(/\s+/)[1] === mountpoint);
  }
  const { stdout } = await execFileAsync("mount", [], { timeout: 5_000 });
  return stdout.split("\n").some((line) => line.includes(` on ${mountpoint} (`));
}

async function mountListing() {
  if (process.platform === "linux") return readFile("/proc/self/mounts", "utf8");
  return (await execFileAsync("mount", [], { timeout: 5_000 })).stdout;
}

async function tryCommand(command, args) {
  try {
    await execFileAsync(command, args, { timeout: 10_000 });
  } catch {
    // Cleanup is best-effort here. The caller still checks the mount table
    // before deciding whether the temporary directory may be removed.
  }
}

async function fallbackUnmount(mountpoint) {
  if (process.platform === "linux") {
    await tryCommand("fusermount3", ["-u", mountpoint]);
    if (await isMounted(mountpoint)) await tryCommand("fusermount", ["-u", mountpoint]);
    if (await isMounted(mountpoint)) await tryCommand("umount", [mountpoint]);
    return;
  }
  await tryCommand("umount", ["-f", mountpoint]);
  if (await isMounted(mountpoint)) await tryCommand("umount", [mountpoint]);
}

async function loadProbe() {
  const local = new URL("../../integrations/mount-rs-napi/index.js", import.meta.url).href;
  const specifiers = process.env.MOUNT_RS_NAPI_PACKAGE
    ? [process.env.MOUNT_RS_NAPI_PACKAGE]
    : [local, "@mount-rs/core"];
  const failures = [];
  for (const specifier of specifiers) {
    try {
      const namespace = await import(specifier);
      const scope = namespace.default ?? namespace;
      if (typeof scope.probeTransports !== "function") {
        throw new Error("the Node SDK does not export probeTransports");
      }
      return scope.probeTransports;
    } catch (error) {
      failures.push(`${specifier}: ${error instanceof Error ? error.message : String(error)}`);
    }
  }
  throw new Error(`unable to load the Node SDK for transport probing: ${failures.join("; ")}`);
}

async function run() {
  if (process.env[optIn] !== "1") {
    skip(`${optIn}=1`);
    return;
  }
  if (process.platform !== "darwin" && process.platform !== "linux") {
    skip(`platform-${process.platform}-not-supported`);
    return;
  }

  const transport = process.platform === "darwin" ? "nfs" : "fuse";
  let probeTransports;
  try {
    probeTransports = await loadProbe();
  } catch (error) {
    skip(`node-sdk-addon-unavailable (${error instanceof Error ? error.message : String(error)})`);
    return;
  }

  const probe = await probeTransports();
  const availability = probe[transport];
  if (!availability?.usable) {
    skip(`${transport}-unavailable (${availability?.reason ?? "the SDK probe did not report a reason"})`);
    return;
  }
  console.log(`GATE node-cli-native platform=${process.platform} transport=${transport} credentials=none`);

  const runDirectory = await mkdtemp(join(tmpdir(), "mount-rs-node-cli-native-"));
  const mountpoint = join(runDirectory, "mount");
  const backing = join(runDirectory, "backing");
  const configPath = join(runDirectory, "config.json");
  const filename = `node-cli-${process.pid}.txt`;
  const seed = "seed written before the Node CLI mount\n";
  const first = "Node client wrote through the mounted Node SDK CLI path\n";
  const second = "Node client rewrote through the mounted Node SDK CLI path\n";
  let cli;
  let mounted = false;
  let failure;

  try {
    await mkdir(mountpoint);
    await mkdir(backing);
    await writeFile(join(backing, "seed.txt"), seed, { encoding: "utf8", mode: 0o600 });
    await writeFile(
      configPath,
      `${JSON.stringify({
        version: 1,
        mountpoint: "./mount",
        transport,
        driver: { kind: "host", root: "./backing" },
      }, null, 2)}\n`,
      { encoding: "utf8", mode: 0o600 },
    );

    // The CLI waits for SIGINT with a top-level await. Load the exact CLI
    // module through a tiny ephemeral keepalive wrapper so Node does not exit
    // while that promise is pending before this test can send SIGINT.
    cli = startCaptured(process.execPath, [
      "--input-type=module",
      "--eval",
      cliKeepaliveSource,
      cliPath,
      "--config",
      configPath,
    ]);
    const resolvedMountpoint = await realpath(mountpoint);
    try {
      await waitFor(
        async () => cli.child.exitCode !== null
          || ((cli.output.stdout.includes(`mounted ${mountpoint} `)
            || cli.output.stdout.includes(`mounted ${resolvedMountpoint} `))
            && await isMounted(resolvedMountpoint)),
        mountWaitMs,
        `Node CLI ${transport} mount`,
      );
    } catch (error) {
      throw new Error(
        `${error.message}\nNode CLI stdout=${cli.output.stdout}\nNode CLI stderr=${cli.output.stderr}\nMount listing=${await mountListing()}`,
      );
    }
    if (cli.child.exitCode !== null) {
      throw new Error(
        `Node CLI exited before mounting: stdout=${cli.output.stdout} stderr=${cli.output.stderr}`,
      );
    }
    assert.ok(
      cli.output.stdout.includes(`mounted ${mountpoint} `)
        || cli.output.stdout.includes(`mounted ${resolvedMountpoint} `),
    );
    assert.match(cli.output.stdout, new RegExp(`transport=${transport}`));
    mounted = true;
    console.log(`PASS Node SDK CLI mounted through ${transport}`);

    const client = startCaptured(
      process.execPath,
      ["--input-type=module", "-", resolvedMountpoint, filename, seed, first, second],
      { stdin: clientSource },
    );
    const clientExit = await withTimeout(client.exit, clientWaitMs, "independent Node client");
    assert.equal(clientExit.code, 0, `${client.output.stdout}\n${client.output.stderr}`);
    assert.match(client.output.stdout, /PASS independent Node client mounted read\/write/);
    console.log("PASS independent Node client mounted read/write");

    cli.child.kill("SIGINT");
    const cliExit = await withTimeout(cli.exit, stopWaitMs, "Node CLI unmount");
    assert.equal(cliExit.code, 0, `${cli.output.stdout}\n${cli.output.stderr}`);
    assert.match(cli.output.stdout, /unmounted after SIGINT/);
    await waitFor(async () => !(await isMounted(resolvedMountpoint)), stopWaitMs, "native unmount");
    mounted = false;
    console.log("PASS Node SDK CLI unmounted cleanly");

    assert.equal(await readFile(join(backing, filename), "utf8"), second);
    console.log("PASS backing root retained Node client bytes after unmount");
  } catch (error) {
    failure = error;
  } finally {
    if (cli && cli.child.exitCode === null) {
      cli.child.kill("SIGINT");
      try {
        await withTimeout(cli.exit, stopWaitMs, "emergency Node CLI shutdown");
      } catch {
        cli.child.kill("SIGTERM");
        cli.child.kill("SIGKILL");
      }
    }

    let stillMounted = mounted;
    let resolvedMountpoint;
    try {
      resolvedMountpoint = await realpath(mountpoint);
      stillMounted = await isMounted(resolvedMountpoint);
    } catch (error) {
      stillMounted = true;
      failure ??= new Error(`could not inspect native mount during cleanup: ${error.message}`);
    }
    if (stillMounted && resolvedMountpoint) {
      await fallbackUnmount(resolvedMountpoint);
      try {
        stillMounted = await isMounted(resolvedMountpoint);
      } catch {
        stillMounted = true;
      }
    }
    if (stillMounted) {
      console.error(`PRESERVE native integration directory for manual cleanup: ${runDirectory}`);
      failure ??= new Error(`native ${transport} mount remains at ${mountpoint}`);
    } else {
      await rm(runDirectory, { recursive: true, force: true });
    }
  }

  if (failure) throw failure;
  console.log(`SUMMARY node-cli-native pass=1 skip=0 fail=0 transport=${transport}`);
}

try {
  await run();
} catch (error) {
  console.error(`FAIL node-cli-native: ${error instanceof Error ? error.stack ?? error.message : String(error)}`);
  console.log("SUMMARY node-cli-native pass=0 skip=0 fail=1");
  process.exitCode = 1;
}
