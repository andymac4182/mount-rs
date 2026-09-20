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

const fileMetadata = {
  size: 3,
  atime: new Date(1_699_999_999_000),
  mtime: new Date(1_700_000_000_000),
  ctime: new Date(1_700_000_000_500),
  birthtime: new Date(1_699_999_998_000),
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

function statView(stats, includeTimes = true, includeCreationTimes = false) {
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
  if (includeCreationTimes) {
    value.ctimeMs = stats.ctimeMs;
    value.birthtimeMs = stats.birthtimeMs;
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

/**
 * Compare one capability-limited operation without deciding its errno in the
 * native test first. The pinned oracle decides whether the row is supported;
 * an unsupported row must produce the oracle's own ENOSYS/ENOTSUP answer.
 */
async function compareCapabilityRow({
  label,
  supported,
  syscall,
  oracleOperation,
  nativeOperation,
}) {
  const [expected, actual] = await Promise.all([
    capture(oracleOperation),
    capture(nativeOperation),
  ]);
  assert.deepEqual(actual, expected, `${label}: native result differs from oracle`);
  if (supported) {
    assert.equal(expected, null, `${label}: oracle claims support but rejected the operation`);
    return "implemented";
  }
  assert.ok(
    expected && (expected.code === "ENOSYS" || expected.code === "ENOTSUP"),
    `${label}: oracle did not classify the unsupported operation as ENOSYS/ENOTSUP`,
  );
  assert.equal(expected.syscall, syscall, `${label}: oracle syscall classification changed`);
  return "unsupported";
}

function makeOracle() {
  const backing = memoryStorageDriver();
  const storage = createStorage({
    driver: {
      ...backing,
      getMeta(key) {
        return key === "file" ? fileMetadata : {};
      },
    },
  });
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
const capabilityCounts = { implemented: 0, unsupported: 0, skipped: 0 };
let capabilityRowCount = 0;
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

  const capabilityRows = [
    {
      label: "hardlink",
      capability: "hardlinks",
      syscall: "link",
      oracleOperation: () => oracle.fs.link("/file", "/hardlink"),
      nativeOperation: () => native.fs.link("/file", "/hardlink"),
    },
    {
      label: "symlink",
      capability: "symlinks",
      syscall: "symlink",
      oracleOperation: () => oracle.fs.symlink("file", "/symlink"),
      nativeOperation: () => native.fs.symlink("file", "/symlink"),
    },
    {
      label: "readlink",
      capability: "symlinks",
      syscall: "readlink",
      oracleOperation: () => oracle.fs.readlink("/file"),
      nativeOperation: () => native.fs.readlink("/file"),
    },
    {
      label: "statfs",
      capability: "statfs",
      syscall: "statfs",
      oracleOperation: () => oracle.fs.statfs("/"),
      nativeOperation: () => native.fs.statfs("/"),
    },
  ];
  const fileBytes = new Uint8Array([1, 2, 3]);
  await oracle.storage.setItemRaw("file", fileBytes);
  native.store.values.set("file", new Uint8Array(fileBytes));
  native.store.metadata.set("file", fileMetadata);

  // Native metadata is a supported read path. Seed the raw stores directly so
  // the driver's process-local write overlay cannot mask the oracle metadata.
  const [oracleMetadataStats, nativeMetadataStats] = await Promise.all([
    oracle.fs.stat("/file"),
    native.fs.stat("/file"),
  ]);
  assert.deepEqual(
    statView(nativeMetadataStats, true, true),
    statView(oracleMetadataStats, true, true),
    "native metadata fields differ from the pinned oracle",
  );
  assert.equal(nativeMetadataStats.ctimeMs, fileMetadata.ctime.getTime());
  assert.equal(nativeMetadataStats.birthtimeMs, fileMetadata.birthtime.getTime());
  native.store.metadata.delete("file");
  capabilityRowCount = capabilityRows.length + 1;
  for (const row of capabilityRows) {
    const supported = expectedCapabilities[row.capability] === true;
    // These rows are intentionally exercised even when the oracle says they
    // are absent. A missing optional method is part of the oracle contract;
    // it is not a reason to skip the comparison.
    const classification = await compareCapabilityRow({ ...row, supported });
    capabilityCounts[classification] += 1;
  }

  // The mknod surface is an extension in mountx, not a normal FsDriver method.
  // Compare the actual oracle extension classification before constructing the
  // expected refusal. There is no callable oracle method when the extension is
  // absent; the pinned oracle's transport contract uses its own fsError helper
  // to answer ENOSYS for a non-regular mknod request in that case.
  const oracleMknod = oracle.fs.mountx?.mknod;
  const oracleMknodSupported =
    expectedCapabilities.extensions.includes("mknod") && typeof oracleMknod === "function";
  assert.equal(oracle.fs.mountx, undefined);
  assert.equal(oracleMknodSupported, false);
  const oracleMknodOperation = (path, mode, dev) => {
    if (oracleMknodSupported) {
      return oracleMknod.call(oracle.fs.mountx, path, mode, dev);
    }
    return Promise.reject(errors.fsError("ENOSYS", { syscall: "mknod" }));
  };
  const nativeMknod = native.fs.mknod;
  assert.equal(typeof nativeMknod, "function");
  const nativeMountxMknod = native.fs.mountx?.mknod;
  assert.equal(typeof nativeMountxMknod, "function");

  // The generic key space has no node-kind or device-number representation.
  // Keep the capability false and classify every special-node shape from the
  // upstream skipped group through both public mknod surfaces. If a future
  // adapter grows the extension, these rows become real oracle-backed behavior
  // checks without changing the test's capability decision.
  const specialNodeRows = [
    { label: "mknod FIFO", name: "fifo", mode: 0o010644, dev: 0 },
    { label: "mknod socket", name: "socket", mode: 0o140600, dev: 0 },
    { label: "mknod character device", name: "character", mode: 0o020666, dev: (1 << 8) | 3 },
    { label: "mknod block device", name: "block", mode: 0o060660, dev: 7 << 8 },
  ];
  for (const row of specialNodeRows) {
    capabilityCounts[await compareCapabilityRow({
      label: `${row.label} direct`,
      supported: oracleMknodSupported,
      syscall: "mknod",
      oracleOperation: () => oracleMknodOperation(`/direct-${row.name}`, row.mode, row.dev),
      nativeOperation: () => nativeMknod.call(native.fs, `/direct-${row.name}`, row.mode, row.dev),
    })] += 1;
    capabilityCounts[await compareCapabilityRow({
      label: `${row.label} mountx`,
      supported: oracleMknodSupported,
      syscall: "mknod",
      oracleOperation: () => oracleMknodOperation(`/mountx-${row.name}`, row.mode, row.dev),
      nativeOperation: () =>
        nativeMountxMknod.call(native.fs.mountx, `/mountx-${row.name}`, row.mode, row.dev),
    })] += 1;
  }
  capabilityRowCount = capabilityRows.length + specialNodeRows.length * 2 + 1;

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

// Keep the next disjoint capability-boundary packet in the same N-API gate as
// this profile check. It remains a standalone fixture so it can also be run
// directly when reviewing the pinned-oracle evidence.
await import("./capability-boundaries-next.mjs");

console.log(
  `mount-rs Unstorage capability parity: PASS (${capabilityRowCount} rows: ` +
    `${capabilityCounts.implemented} implemented, ${capabilityCounts.unsupported} ` +
    `oracle-classified unsupported, ${capabilityCounts.skipped} skipped; ` +
    "lstat/stat parity covered)",
);
