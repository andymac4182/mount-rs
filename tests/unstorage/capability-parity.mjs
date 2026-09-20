import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { pathToFileURL } from "node:url";

const require = createRequire(import.meta.url);
const binding = require("../../integrations/mount-rs-napi/index.js");
const source = process.env.MOUNTX_SOURCE;
assert.ok(source, "MOUNTX_SOURCE must identify the pinned upstream checkout");

const upstream = (path) => import(pathToFileURL(`${source}/${path}`).href);
const upstreamRequire = createRequire(pathToFileURL(`${source}/package.json`));
const [{ createUnstorageDriver: createOracleDriver }, { createLoopback, resolveCapabilities }, errors, storageModule, memoryModule] =
  await Promise.all([
    upstream("src/drivers/unstorage.ts"),
    upstream("src/harness.ts"),
    upstream("src/errors.ts"),
    import(pathToFileURL(upstreamRequire.resolve("unstorage")).href),
    import(pathToFileURL(upstreamRequire.resolve("unstorage/drivers/memory")).href),
  ]);

const { createStorage } = storageModule;
const { default: memoryStorageDriver } = memoryModule;

/** A minimal raw store with the same key semantics as unstorage's memory driver. */
class RawStore {
  constructor() {
    this.values = new Map();
    this.metadata = new Map();
  }

  hasItem(key) {
    return this.values.has(key);
  }

  getItemRaw(key) {
    const value = this.values.get(key);
    if (value === undefined) return null;
    return value instanceof Uint8Array ? new Uint8Array(value) : value;
  }

  setItemRaw(key, value) {
    assert.equal(value instanceof Uint8Array, true, "native store receives bytes");
    this.values.set(key, new Uint8Array(value));
  }

  removeItem(key) {
    this.values.delete(key);
    this.metadata.delete(key);
  }

  getKeys(prefix) {
    const childPrefix = prefix === "" ? "" : `${prefix}:`;
    return [...this.values.keys()].filter(
      (key) => prefix === "" || key === prefix || key.startsWith(childPrefix),
    );
  }

  getMeta(key) {
    return this.metadata.get(key) ?? {};
  }
}

const options = {
  // Explicit root-shaped ownership makes this exercise independent of the
  // account running the test. Unstorage stores this in its process-local
  // metadata overlay; it does not claim kernel root-only enforcement.
  uid: 0,
  gid: 0,
  fileMode: 0o640,
  dirMode: 0o750,
};

function capabilityView(capabilities) {
  return {
    handles: capabilities.handles,
    hardlinks: capabilities.hardlinks,
    symlinks: capabilities.symlinks,
    permissions: capabilities.permissions,
    times: capabilities.times,
    truncate: capabilities.truncate,
    atomicRename: capabilities.atomicRename,
    caseSensitive: capabilities.caseSensitive,
    statfs: capabilities.statfs,
    readOnly: capabilities.readOnly,
    durableWrites: capabilities.durableWrites,
    mknod: capabilities.mknod ?? false,
    extensions: [...(capabilities.extensions ?? [])],
  };
}

function statView(stats, includeTimes = true) {
  const value = {
    mode: stats.mode,
    nlink: stats.nlink,
    uid: stats.uid,
    gid: stats.gid,
    size: stats.size,
    blksize: stats.blksize,
    blocks: stats.blocks,
    isFile: stats.isFile(),
    isDirectory: stats.isDirectory(),
    isSymbolicLink: stats.isSymbolicLink(),
  };
  if (includeTimes) {
    value.atimeMs = stats.atimeMs;
    value.mtimeMs = stats.mtimeMs;
  }
  return value;
}

function errorView(error) {
  return {
    code: error?.code ?? null,
    syscall: error?.syscall ?? null,
    path: error?.path ?? null,
    dest: error?.dest ?? null,
  };
}

async function capture(operation) {
  try {
    await operation();
    return null;
  } catch (error) {
    return errorView(error);
  }
}

function makeOracle() {
  const storage = createStorage({ driver: memoryStorageDriver() });
  const driver = createOracleDriver(storage, options);
  return { storage, driver, fs: createLoopback(driver) };
}

function makeNative() {
  const store = new RawStore();
  const fs = binding.createUnstorageDriver(store, options);
  return { store, fs };
}

const oracle = makeOracle();
const native = makeNative();
try {
  // The capability profile is the oracle for everything the flat key space
  // cannot represent. In particular, root-shaped ownership does not promote
  // hardlinks, symlinks, statfs, or mknod into supported operations.
  const expectedCapabilities = capabilityView(resolveCapabilities(oracle.driver));
  assert.deepEqual(capabilityView(native.fs.capabilities), expectedCapabilities);
  assert.deepEqual(expectedCapabilities, {
    handles: true,
    hardlinks: false,
    symlinks: false,
    permissions: true,
    times: true,
    truncate: true,
    atomicRename: false,
    caseSensitive: true,
    statfs: false,
    readOnly: false,
    durableWrites: false,
    mknod: false,
    extensions: [],
  });

  // N-API returns a fresh capability value. A caller cannot mutate the
  // resolved profile and make a later transport believe an unsupported
  // operation is available.
  const mutableSnapshot = native.fs.capabilities;
  mutableSnapshot.hardlinks = true;
  mutableSnapshot.symlinks = true;
  mutableSnapshot.statfs = true;
  mutableSnapshot.mknod = true;
  mutableSnapshot.extensions.push("mknod");
  assert.deepEqual(capabilityView(native.fs.capabilities), expectedCapabilities);

  const unsupported = [
    ["hardlink", (fs) => fs.link("/file", "/hardlink")],
    ["symlink", (fs) => fs.symlink("file", "/symlink")],
    ["readlink", (fs) => fs.readlink("/file")],
    ["statfs", (fs) => fs.statfs("/")],
  ];
  await native.fs.writeFile("/file", new Uint8Array([1, 2, 3]));
  await oracle.fs.writeFile("/file", new Uint8Array([1, 2, 3]));
  for (const [label, operation] of unsupported) {
    const [expected, actual] = await Promise.all([
      capture(() => operation(oracle.fs)),
      capture(() => operation(native.fs)),
    ]);
    assert.deepEqual(actual, expected, `${label}: native result differs from oracle`);
    assert.deepEqual(actual, {
      code: "ENOSYS",
      syscall: label === "hardlink" ? "link" : label,
      path: null,
      dest: null,
    });
  }

  // The mknod surface is an extension in mountx, not a normal FsDriver method.
  // The oracle therefore has no mountx extension; the Rust facade still
  // exposes its compatibility method, which must remain an explicit ENOSYS.
  assert.equal(oracle.fs.mountx, undefined);
  const mknodExpected = errorView(errors.fsError("ENOSYS", { syscall: "mknod" }));
  assert.deepEqual(errorView(await (async () => {
    try {
      await native.fs.mknod("/fifo", 0o010644, 0);
      return null;
    } catch (error) {
      return error;
    }
  })()), mknodExpected);
  assert.deepEqual(errorView(await (async () => {
    try {
      await native.fs.mountx.mknod("/fifo-mountx", 0o010644, 0);
      return null;
    } catch (error) {
      return error;
    }
  })()), mknodExpected);

  // Permissions and link-aware timestamp calls are supported as overlays. In
  // a no-symlink profile lstat/lutimes are intentionally indistinguishable
  // from stat/utimes, including when ownership is root-shaped.
  const [oracleDir, nativeDir] = await Promise.all([
    oracle.fs.mkdir("/dir", { mode: 0o751 }),
    native.fs.mkdir("/dir", { mode: 0o751 }),
  ]);
  assert.equal(nativeDir, oracleDir);
  await Promise.all([
    oracle.fs.chmod("/file", 0o604),
    native.fs.chmod("/file", 0o604),
  ]);
  await Promise.all([
    oracle.fs.chown("/file", 65_534, 65_534),
    native.fs.chown("/file", 65_534, 65_534),
  ]);
  await Promise.all([
    oracle.fs.lchown("/file", 65_534, 65_534),
    native.fs.lchown("/file", 65_534, 65_534),
  ]);
  await Promise.all([
    oracle.fs.utimes("/file", 1, 2),
    native.fs.utimes("/file", 1, 2),
  ]);
  await Promise.all([
    oracle.fs.lutimes("/file", 3, 4),
    native.fs.lutimes("/file", 3, 4),
  ]);

  const [oracleFileStats, nativeFileStats, oracleLstat, nativeLstat] = await Promise.all([
    oracle.fs.stat("/file"),
    native.fs.stat("/file"),
    oracle.fs.lstat("/file"),
    native.fs.lstat("/file"),
  ]);
  assert.deepEqual(statView(nativeFileStats), statView(oracleFileStats));
  assert.deepEqual(statView(nativeLstat), statView(oracleLstat));
  assert.deepEqual(statView(nativeLstat), statView(nativeFileStats));

  // The overlay is deliberately not serialized into the raw store. A fresh
  // driver sees the backend's defaults, matching the TypeScript oracle.
  const freshNative = binding.createUnstorageDriver(native.store, options);
  const freshOracleStorage = createStorage({ driver: memoryStorageDriver() });
  await freshOracleStorage.setItemRaw("file", new Uint8Array([1, 2, 3]));
  const freshOracle = createLoopback(createOracleDriver(freshOracleStorage, options));
  try {
    const [expectedFresh, actualFresh] = await Promise.all([
      freshOracle.stat("/file"),
      freshNative.stat("/file"),
    ]);
    assert.deepEqual(statView(actualFresh, false), statView(expectedFresh, false));
    assert.equal(native.store.metadata.size, 0);
    assert.deepEqual([...native.store.values.get("file")], [1, 2, 3]);
  } finally {
    await freshNative.shutdown();
  }
} finally {
  await native.fs.shutdown();
  await oracle.fs.shutdown?.();
}

console.log("mount-rs Unstorage capability parity: PASS");
