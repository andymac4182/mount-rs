#!/usr/bin/env node

import { promises as fs } from "node:fs";
import { dirname, join, resolve } from "node:path";

const DRIVERS = new Set(["memory", "host"]);
const TRANSPORTS = new Set(["auto", "fuse", "9p", "nfs"]);

class CliConfigError extends Error {}

function usage() {
  return `Usage:
  node examples/node-cli/index.mjs [--mountpoint <path>] [options]

Options:
  --driver <memory|host>  Select the public Node SDK driver (default: memory).
  --transport <auto|fuse|9p|nfs>  Select the native transport (default: auto).
  --mountpoint <path>     Directory to mount.
  --root <path>           Host-driver root (default: current directory).
  --config <path>          Load a versioned provider configuration file.
  --sdk-self-test         Load the Node SDK, write/read through its driver,
                          and exit without creating a native mount.
  --reopen                Recreate a configured SDK driver and verify readback
                          after shutdown (requires --sdk-self-test).
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
    config: undefined,
    driver: "memory",
    driverSpecified: false,
    help: false,
    mountpoint: undefined,
    mountpointSpecified: false,
    root: undefined,
    rootSpecified: false,
    sdkSelfTest: false,
    selfTest: false,
    transport: "auto",
    transportSpecified: false,
    reopen: false,
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
      case "--sdk-self-test":
        parsed.sdkSelfTest = true;
        break;
      case "--reopen":
        parsed.reopen = true;
        break;
      case "--config":
        parsed.config = takeValue(argv, index, argument);
        index += 1;
        break;
      case "--driver":
        parsed.driver = takeValue(argv, index, argument).toLowerCase();
        parsed.driverSpecified = true;
        index += 1;
        break;
      case "--mountpoint":
        parsed.mountpoint = takeValue(argv, index, argument);
        parsed.mountpointSpecified = true;
        index += 1;
        break;
      case "--root":
        parsed.root = takeValue(argv, index, argument);
        parsed.rootSpecified = true;
        index += 1;
        break;
      case "--transport":
        parsed.transport = takeValue(argv, index, argument).toLowerCase();
        parsed.transportSpecified = true;
        index += 1;
        break;
      default:
        throw new CliConfigError(`unknown argument: ${argument}`);
    }
  }

  if (parsed.help) return parsed;
  if (!parsed.mountpoint && !parsed.sdkSelfTest && !parsed.config) {
    throw new CliConfigError("--mountpoint is required unless --sdk-self-test or --config is used");
  }
  if (!DRIVERS.has(parsed.driver)) {
    throw new CliConfigError("--driver must be memory or host");
  }
  if (!TRANSPORTS.has(parsed.transport)) {
    throw new CliConfigError("--transport must be auto, fuse, 9p, or nfs");
  }
  if (parsed.driver === "memory" && parsed.root !== undefined) {
    throw new CliConfigError("--root is only valid with --driver host");
  }
  if (parsed.check && (parsed.selfTest || parsed.sdkSelfTest)) {
    throw new CliConfigError("--check cannot be combined with a self-test");
  }
  if (parsed.selfTest && parsed.sdkSelfTest) {
    throw new CliConfigError("--self-test and --sdk-self-test cannot be used together");
  }
  if (parsed.reopen && !parsed.sdkSelfTest) {
    throw new CliConfigError("--reopen requires --sdk-self-test");
  }
  if (parsed.reopen && parsed.driver === "memory" && parsed.config === undefined) {
    throw new CliConfigError("--reopen requires a durable driver; memory is process-local");
  }

  return {
    ...parsed,
    config: parsed.config === undefined ? undefined : resolve(parsed.config),
    mountpoint: parsed.mountpoint === undefined ? undefined : resolve(parsed.mountpoint),
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

function configObject(value, label) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new CliConfigError(`${label} must be an object`);
  }
  return value;
}

function configString(value, label) {
  if (typeof value !== "string" || value.length === 0) {
    throw new CliConfigError(`${label} must be a non-empty string`);
  }
  return value;
}

function configBoolean(value, label, fallback) {
  if (value === undefined) return fallback;
  if (typeof value !== "boolean") {
    throw new CliConfigError(`${label} must be a boolean`);
  }
  return value;
}

function configPositiveInteger(value, label, fallback) {
  if (value === undefined) return fallback;
  if (!Number.isSafeInteger(value) || value <= 0) {
    throw new CliConfigError(`${label} must be a positive integer`);
  }
  return value;
}

function configPath(value, label, baseDirectory) {
  return resolve(baseDirectory, configString(value, label));
}

function configCredential(value, label, resolveValue) {
  const reference = configObject(value, label);
  const name = configString(reference.env, `${label}.env`);
  if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(name)) {
    throw new CliConfigError(`${label}.env must be a valid environment variable name`);
  }
  if (!resolveValue) return undefined;
  const resolvedValue = process.env[name];
  if (!resolvedValue) throw new CliConfigError(`missing environment variable ${name}`);
  return resolvedValue;
}

function configStore(value, role, baseDirectory, resolveValue) {
  const store = configObject(value, `driver.storage.${role}`);
  const kind = configString(store.kind, `driver.storage.${role}.kind`).toLowerCase();
  if (kind === "memory") return { kind: "memory" };
  if (kind === "sqlite") {
    return {
      kind: "sqlite",
      uri: configPath(store.path, `driver.storage.${role}.path`, baseDirectory),
    };
  }
  if (kind === "pglite") {
    return {
      kind: "pglite",
      uri: configCredential(
        store.connection,
        `driver.storage.${role}.connection`,
        resolveValue,
      ),
      key: store.volume_key === undefined
        ? "mount-rs"
        : configString(store.volume_key, `driver.storage.${role}.volume_key`),
      durable: configBoolean(store.durable, `driver.storage.${role}.durable`, false),
    };
  }
  if (kind === "tidb") {
    return {
      kind: "tidb",
      uri: configCredential(
        store.connection,
        `driver.storage.${role}.connection`,
        resolveValue,
      ),
      key: store.volume_key === undefined
        ? "mount-rs"
        : configString(store.volume_key, `driver.storage.${role}.volume_key`),
      durable: configBoolean(store.durable, `driver.storage.${role}.durable`, false),
    };
  }
  if (kind === "r2") {
    if (role !== "blocks") {
      throw new CliConfigError("R2 is a block-only provider; metadata must use memory, sqlite, pglite, or tidb");
    }
    return {
      kind: "r2",
      endpoint: configString(store.endpoint, "driver.storage.blocks.endpoint"),
      bucket: configString(store.bucket, "driver.storage.blocks.bucket"),
      key: configString(store.prefix, "driver.storage.blocks.prefix"),
      accessKeyId: configCredential(store.access_key_id, "driver.storage.blocks.access_key_id", resolveValue),
      secretAccessKey: configCredential(
        store.secret_access_key,
        "driver.storage.blocks.secret_access_key",
        resolveValue,
      ),
      durable: configBoolean(store.durable, "driver.storage.blocks.durable", true),
    };
  }
  throw new CliConfigError(`unknown provider '${kind}' for ${role}`);
}

function configDriver(driver, baseDirectory, resolveValue) {
  const value = configObject(driver, "driver");
  const kind = configString(value.kind, "driver.kind").toLowerCase();
  switch (kind) {
    case "memory":
      return { kind };
    case "host":
      return {
        kind,
        root: configPath(value.root, "driver.root", baseDirectory),
      };
    case "sqlite":
      return {
        kind,
        database: configPath(value.database, "driver.database", baseDirectory),
      };
    case "splitstore": {
      const storage = value.storage === undefined
        ? {
            metadata: { kind: "sqlite", path: value.database },
            blocks: { kind: "sqlite", path: value.blocks },
            chunk_size_bytes: 64 * 1024,
          }
        : configObject(value.storage, "driver.storage");
      if (storage.metadata === undefined || storage.blocks === undefined) {
        throw new CliConfigError("driver.storage requires metadata and blocks providers");
      }
      return {
        kind,
        chunked: {
          metadata: configStore(storage.metadata, "metadata", baseDirectory, resolveValue),
          blocks: configStore(storage.blocks, "blocks", baseDirectory, resolveValue),
          chunkSize: configPositiveInteger(
            storage.chunk_size_bytes,
            "driver.storage.chunk_size_bytes",
            64 * 1024,
          ),
          owner: storage.owner === undefined
            ? `mount-rs-node-cli-${process.pid}-${Date.now()}`
            : configString(storage.owner, "driver.storage.owner"),
        },
      };
    }
    default:
      throw new CliConfigError(`unknown driver '${kind}'`);
  }
}

async function loadConfiguration(configuration, resolveValue) {
  if (configuration.config === undefined) return configuration;
  if (configuration.driverSpecified) {
    throw new CliConfigError("--driver cannot be combined with --config");
  }
  if (configuration.rootSpecified) {
    throw new CliConfigError("--root cannot be combined with --config");
  }

  let source;
  try {
    source = JSON.parse(await fs.readFile(configuration.config, "utf8"));
  } catch (error) {
    throw new CliConfigError(`cannot read config ${configuration.config}: ${error.message}`);
  }
  source = configObject(source, "config");
  if (source.version !== 1) {
    throw new CliConfigError("config.version must be 1");
  }
  const baseDirectory = dirname(configuration.config);
  const driver = configDriver(source.driver ?? { kind: "memory" }, baseDirectory, resolveValue);
  const resolved = {
    ...configuration,
    driver: driver.kind,
    provider: driver,
    mountpoint: configuration.mountpointSpecified
      ? configuration.mountpoint
      : source.mountpoint === undefined
        ? undefined
        : configPath(source.mountpoint, "config.mountpoint", baseDirectory),
    transport: configuration.transportSpecified
      ? configuration.transport
      : source.transport === undefined
        ? "auto"
        : configString(source.transport, "config.transport").toLowerCase(),
    readOnly: configBoolean(source.read_only, "config.read_only", false),
  };
  if (resolved.reopen && resolved.driver === "memory") {
    throw new CliConfigError("--reopen requires a durable driver; memory is process-local");
  }
  if (!TRANSPORTS.has(resolved.transport)) {
    throw new CliConfigError(`config.transport must be auto, fuse, 9p, or nfs`);
  }
  if (!resolved.sdkSelfTest && !resolved.mountpoint) {
    throw new CliConfigError("config.mountpoint is required unless --sdk-self-test is used");
  }
  return resolved;
}

async function selectDriver(sdk, configuration) {
  const provider = configuration.provider;
  if (provider?.kind === "splitstore") {
    const createChunkedDriver = findExport(sdk, ["createChunkedDriver"]);
    if (typeof createChunkedDriver !== "function") {
      throw new Error("the Node SDK does not export public createChunkedDriver");
    }
    return createChunkedDriver(provider.chunked);
  }
  if (provider?.kind === "sqlite") {
    const Filesystem = findExport(sdk, ["Filesystem"]);
    if (typeof Filesystem?.sqlite === "function") return Filesystem.sqlite(provider.database);
  }
  if (provider?.kind === "memory" || configuration.driver === "memory") {
    const createMemoryDriver = findExport(sdk, ["createMemoryDriver"]);
    if (typeof createMemoryDriver === "function") return createMemoryDriver();
    const Filesystem = findExport(sdk, ["Filesystem"]);
    if (typeof Filesystem?.memory === "function") return Filesystem.memory();
  } else if (provider?.kind === "host" || configuration.driver === "host") {
    const createNodeFsDriver = findExport(sdk, ["createNodeFsDriver"]);
    if (typeof createNodeFsDriver === "function") {
      return createNodeFsDriver(provider?.root ?? configuration.root, {
        readOnly: configuration.readOnly,
      });
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

async function runSdkSelfTest(createFilesystem, driver, reopen) {
  const filename = `.mount-rs-node-sdk-direct-${process.pid}-${Date.now()}.txt`;
  const path = `/${filename}`;
  const expected = `mount-rs Node SDK direct driver wrote this file (${driver})\n`;
  const patch = Buffer.from("partial");
  const patchOffset = 3;
  const finalLength = expected.length - 2;
  const expectedAfterPatch = Buffer.from(expected);
  patch.copy(expectedAfterPatch, patchOffset);
  const expectedFinal = expectedAfterPatch.subarray(0, finalLength);
  let filesystem = await createFilesystem();
  try {
    await filesystem.writeFile(path, expected);
    const handle = await filesystem.open(path, "r+");
    try {
      const result = await handle.write(patch, 0, patch.length, patchOffset);
      if (result.bytesWritten !== patch.length) {
        throw new Error("Node SDK direct-driver partial write length mismatch");
      }
      await handle.sync();
    } finally {
      await handle.close();
    }
    await filesystem.truncate(path, finalLength);
    const actual = Buffer.from(await filesystem.readFile(path));
    if (!actual.equals(expectedFinal)) {
      throw new Error("Node SDK direct-driver partial/truncate readback mismatch");
    }
    if (!reopen) {
      await filesystem.unlink(path).catch(() => {});
    }
  } finally {
    await filesystem.shutdown();
  }
  if (reopen) {
    filesystem = await createFilesystem();
    try {
      const actual = Buffer.from(await filesystem.readFile(path));
      if (!actual.equals(expectedFinal)) {
        throw new Error("Node SDK reopen partial/truncate readback mismatch");
      }
      await filesystem.unlink(path);
      console.log(`sdk self-test passed: Node SDK wrote, shut down, reopened, and read ${filename}`);
    } finally {
      await filesystem.shutdown();
    }
  } else {
    console.log(`sdk self-test passed: Node SDK wrote and read ${filename}`);
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
  configuration = await loadConfiguration(configuration, !configuration.check);
  if (configuration.check) {
    console.log(
      `configuration valid: driver=${configuration.driver} transport=${configuration.transport} mountpoint=${configuration.mountpoint} root=${configuration.root} (no SDK loaded; no mount attempted)`,
    );
    return;
  }

  const { mount, sdk } = await loadSdk();
  if (configuration.sdkSelfTest) {
    await runSdkSelfTest(
      () => selectDriver(sdk, configuration),
      configuration.driver,
      configuration.reopen,
    );
    return;
  }
  if (!configuration.mountpoint) {
    throw new CliConfigError("--mountpoint or config.mountpoint is required for a native mount");
  }
  const driver = await selectDriver(sdk, configuration);
  const mountOptions = {
    ...(configuration.transport === "auto" ? {} : { transport: configuration.transport }),
    ...(configuration.readOnly === undefined ? {} : { readOnly: configuration.readOnly }),
  };
  const mounted = await mount(driver, configuration.mountpoint, mountOptions);
  const closeMount = cleanupFunction(mounted);
  let mountClosed = false;
  let driverClosed = false;
  const closeOnce = async () => {
    if (!mountClosed) {
      await closeMount();
      mountClosed = true;
    }
    if (!driverClosed) {
      await driver.shutdown();
      driverClosed = true;
    }
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
