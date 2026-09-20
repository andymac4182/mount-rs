#!/usr/bin/env node

import { promises as fs } from "node:fs";
import { join, resolve } from "node:path";

const DRIVERS = new Set(["memory", "host"]);
const TRANSPORTS = new Set(["auto", "fuse", "9p", "nfs"]);

class CliConfigError extends Error {}

function usage() {
  return `Usage:
  node examples/node-cli/index.mjs --mountpoint <path> [options]

Options:
  --driver <memory|host>  Select the public Node SDK driver (default: memory).
  --transport <auto|fuse|9p|nfs>  Select the native transport (default: auto).
  --mountpoint <path>     Directory to mount.
  --root <path>           Host-driver root (default: current directory).
  --self-test             Write and read one file through the mounted path, then exit.
  --check                 Validate arguments without loading the SDK or mounting.
  --help                  Show this help.

The CLI uses the public @mount-rs/core N-API SDK. From a checkout it loads the
local addon at integrations/mount-rs-napi; set MOUNT_RS_NAPI_PACKAGE to use a
published or otherwise externally-resolved package. It never reads credentials.
`;
}

function takeValue(argv, index, option) {
  const value = argv[index + 1];
  if (!value || value.startsWith("--")) {
    throw new CliConfigError(`${option} requires a value`);
  }
  return value;
}

function parseArgs(argv) {
  const parsed = {
    check: false,
    driver: "memory",
    help: false,
    mountpoint: undefined,
    root: undefined,
    selfTest: false,
    transport: "auto",
  };

  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    switch (argument) {
      case "-h":
      case "--help":
        parsed.help = true;
        break;
      case "--check":
        parsed.check = true;
        break;
      case "--self-test":
        parsed.selfTest = true;
        break;
      case "--driver":
        parsed.driver = takeValue(argv, index, argument).toLowerCase();
        index += 1;
        break;
      case "--mountpoint":
        parsed.mountpoint = takeValue(argv, index, argument);
        index += 1;
        break;
      case "--root":
        parsed.root = takeValue(argv, index, argument);
        index += 1;
        break;
      case "--transport":
        parsed.transport = takeValue(argv, index, argument).toLowerCase();
        index += 1;
        break;
      default:
        throw new CliConfigError(`unknown argument: ${argument}`);
    }
  }

  if (parsed.help) return parsed;
  if (!parsed.mountpoint) throw new CliConfigError("--mountpoint is required");
  if (!DRIVERS.has(parsed.driver)) {
    throw new CliConfigError("--driver must be memory or host");
  }
  if (!TRANSPORTS.has(parsed.transport)) {
    throw new CliConfigError("--transport must be auto, fuse, 9p, or nfs");
  }
  if (parsed.driver === "memory" && parsed.root !== undefined) {
    throw new CliConfigError("--root is only valid with --driver host");
  }
  if (parsed.check && parsed.selfTest) {
    throw new CliConfigError("--check and --self-test cannot be used together");
  }

  return {
    ...parsed,
    mountpoint: resolve(parsed.mountpoint),
    root: resolve(parsed.root ?? process.cwd()),
  };
}

async function importFirst(specifiers, label) {
  const failures = [];
  for (const specifier of specifiers) {
    try {
      return await import(specifier);
    } catch (error) {
      failures.push(`${specifier}: ${error instanceof Error ? error.message : String(error)}`);
    }
  }
  throw new Error(`unable to load ${label}; tried ${failures.join("; ")}`);
}

async function loadSdk() {
  const local = new URL("../../integrations/mount-rs-napi/index.js", import.meta.url).href;
  const specifiers = process.env.MOUNT_RS_NAPI_PACKAGE
    ? [process.env.MOUNT_RS_NAPI_PACKAGE]
    : [local, "@mount-rs/core"];
  const sdk = await importFirst(specifiers, "the public @mount-rs/core Node SDK");
  const mount = sdk.mount ?? sdk.default?.mount;
  if (typeof mount !== "function") {
    throw new Error("the Node SDK does not export the public mount(driver, mountpoint, options) function");
  }
  return { mount, sdk };
}

function findExport(namespace, names) {
  for (const scope of [namespace, namespace?.default]) {
    if (!scope || (typeof scope !== "object" && typeof scope !== "function")) continue;
    for (const name of names) {
      if (scope[name] !== undefined) return scope[name];
    }
  }
  return undefined;
}

async function selectDriver(sdk, configuration) {
  if (configuration.driver === "memory") {
    const createMemoryDriver = findExport(sdk, ["createMemoryDriver"]);
    if (typeof createMemoryDriver === "function") return createMemoryDriver();
    const Filesystem = findExport(sdk, ["Filesystem"]);
    if (typeof Filesystem?.memory === "function") return Filesystem.memory();
  } else {
    const createNodeFsDriver = findExport(sdk, ["createNodeFsDriver"]);
    if (typeof createNodeFsDriver === "function") {
      return createNodeFsDriver(configuration.root);
    }
  }
  throw new Error(`the Node SDK has no ${configuration.driver} driver factory`);
}

function cleanupFunction(mounted) {
  if (typeof mounted === "function") return mounted;
  if (typeof mounted?.unmount === "function") return () => mounted.unmount();
  throw new Error("the Node SDK mount() result has no unmount() method");
}

async function runSelfTest(mountpoint, driver) {
  const filename = `.mount-rs-node-sdk-${process.pid}-${Date.now()}.txt`;
  const path = join(mountpoint, filename);
  const expected = `mount-rs Node SDK wrote this file (${driver})\n`;
  try {
    await fs.writeFile(path, expected, { encoding: "utf8", flag: "wx" });
    const actual = await fs.readFile(path, "utf8");
    if (actual !== expected) throw new Error("Node SDK self-test readback mismatch");
    console.log(`self-test passed: Node SDK wrote and read ${filename}`);
  } finally {
    await fs.rm(path, { force: true });
  }
}

async function waitForSigint(closeMount) {
  await new Promise((resolvePromise, rejectPromise) => {
    let stopping = false;
    process.once("SIGINT", async () => {
      if (stopping) return;
      stopping = true;
      try {
        await closeMount();
        console.log("unmounted after SIGINT");
        resolvePromise();
      } catch (error) {
        rejectPromise(error);
      }
    });
  });
}

async function run(configuration) {
  if (configuration.check) {
    console.log(
      `configuration valid: driver=${configuration.driver} transport=${configuration.transport} mountpoint=${configuration.mountpoint} root=${configuration.root} (no SDK loaded; no mount attempted)`,
    );
    return;
  }

  const { mount, sdk } = await loadSdk();
  const driver = await selectDriver(sdk, configuration);
  const mountOptions = configuration.transport === "auto"
    ? undefined
    : { transport: configuration.transport };
  const mounted = await mount(driver, configuration.mountpoint, mountOptions);
  const closeMount = cleanupFunction(mounted);
  let closed = false;
  const closeOnce = async () => {
    if (closed) return;
    closed = true;
    await closeMount();
  };

  try {
    if (configuration.selfTest) {
      await runSelfTest(configuration.mountpoint, configuration.driver);
      await closeOnce();
      return;
    }
    console.log(
      `mounted ${configuration.mountpoint} with Node SDK driver=${configuration.driver} transport=${mounted.transport ?? configuration.transport}; press Ctrl-C to unmount`,
    );
    await waitForSigint(closeOnce);
  } finally {
    await closeOnce();
  }
}

async function main() {
  let configuration;
  try {
    configuration = parseArgs(process.argv.slice(2));
  } catch (error) {
    if (error instanceof CliConfigError) {
      console.error(`error: ${error.message}`);
      console.error("Try --help for usage.");
      process.exitCode = 2;
      return;
    }
    throw error;
  }
  if (configuration.help) {
    process.stdout.write(usage());
    return;
  }
  try {
    await run(configuration);
  } catch (error) {
    console.error(`error: ${error instanceof Error ? error.message : String(error)}`);
    process.exitCode = 1;
  }
}

await main();
