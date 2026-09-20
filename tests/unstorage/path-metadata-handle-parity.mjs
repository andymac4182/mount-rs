import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createRequire } from "node:module";
import { pathToFileURL } from "node:url";

const PINNED_ORACLE = "85361a8212ff9bff8e69f62fa8993ef2c2ec51e8";
const require = createRequire(import.meta.url);
const binding = require("../../integrations/mount-rs-napi/index.js");
const source = process.env.MOUNTX_SOURCE;
assert.ok(source, "MOUNTX_SOURCE must identify the pinned upstream checkout");
const revision = execFileSync("git", ["-C", source, "rev-parse", "HEAD"], {
  encoding: "utf8",
}).trim();
assert.equal(revision, PINNED_ORACLE, "MOUNTX_SOURCE is not the pinned mountx oracle");

const upstream = (path) => import(pathToFileURL(`${source}/${path}`).href);
const upstreamRequire = createRequire(pathToFileURL(`${source}/package.json`));
const [
  { createUnstorageDriver: createOracleDriver },
  { createLoopback },
  storageModule,
] = await Promise.all([
  upstream("src/drivers/unstorage.ts"),
  upstream("src/harness.ts"),
  import(pathToFileURL(upstreamRequire.resolve("unstorage")).href),
]);

const { createStorage } = storageModule;

const options = {
  uid: 0,
  gid: 0,
  fileMode: 0o640,
  dirMode: 0o750,
};

/**
 * The callback shape consumed by the N-API bridge. The same store is wrapped
 * by mountx's TypeScript adapter so this packet compares the bridge itself,
 * rather than comparing two different storage implementations.
 */
class RawStore {
  constructor(values, metadata = new Map()) {
    this.values = new Map(values);
    this.metadata = new Map(metadata);
  }

  hasItem(key) {
    return this.values.has(key);
  }

  getItemRaw(key) {
    const value = this.values.get(key);
    return value === undefined
      ? null
      : value instanceof Uint8Array
        ? new Uint8Array(value)
        : value;
  }

  setItemRaw(key, value) {
    assert.equal(value instanceof Uint8Array, true, "setItemRaw receives bytes");
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

  getAllKeys() {
    return [...this.values.keys()];
  }

  getMeta(key) {
    return this.metadata.get(key) ?? {};
  }

  // unstorage invokes driver callbacks without a receiver; the N-API bridge
  // invokes the same callbacks as members of the supplied store object.
  // Bind only the oracle view so both adapters still share identical state.
  asOracleDriver() {
    return {
      hasItem: this.hasItem.bind(this),
      getItemRaw: this.getItemRaw.bind(this),
      setItemRaw: this.setItemRaw.bind(this),
      removeItem: this.removeItem.bind(this),
      // A raw unstorage driver exposes an unfiltered listing; the Storage
      // wrapper applies its own base filter. The N-API callback surface is
      // intentionally different and receives the already-scoped prefix.
      getKeys: this.getAllKeys.bind(this),
      getMeta: this.getMeta.bind(this),
    };
  }
}

function cloneValues(values) {
  return [...values].map(([key, value]) => [key, new Uint8Array(value)]);
}

function cloneMetadata(metadata) {
  return [...metadata].map(([key, value]) => [key, { ...value }]);
}

function makePair(values, metadata = new Map()) {
  const oracleStore = new RawStore(cloneValues(values), cloneMetadata(metadata));
  const nativeStore = new RawStore(cloneValues(values), cloneMetadata(metadata));
  const oracleStorage = createStorage({ driver: oracleStore.asOracleDriver() });
  const oracle = createLoopback(createOracleDriver(oracleStorage, options));
  const native = binding.createUnstorageDriver(nativeStore, options);
  return { oracle, native, oracleStore, nativeStore };
}

function errorView(error) {
  return {
    code: error?.code ?? null,
    syscall: error?.syscall ?? null,
    path: error?.path ?? null,
    dest: error?.dest ?? null,
  };
}

async function capture(operation, project = (value) => value) {
  try {
    return project(await operation());
  } catch (error) {
    return { error: errorView(error) };
  }
}

async function compare(pair, label, oracleOperation, nativeOperation, project) {
  const [expected, actual] = await Promise.all([
    capture(oracleOperation, project),
    capture(nativeOperation, project),
  ]);
  assert.deepEqual(actual, expected, `${label}: native result differs from oracle`);
  return expected;
}

function statView(stats) {
  return {
    mode: stats.mode,
    nlink: stats.nlink,
    uid: stats.uid,
    gid: stats.gid,
    size: stats.size,
    blksize: stats.blksize,
    blocks: stats.blocks,
    atimeMs: stats.atimeMs,
    mtimeMs: stats.mtimeMs,
    ctimeMs: stats.ctimeMs,
    birthtimeMs: stats.birthtimeMs,
    isFile: stats.isFile(),
    isDirectory: stats.isDirectory(),
    isSymbolicLink: stats.isSymbolicLink(),
  };
}

function entriesView(entries) {
  return entries
    .map((entry) => ({
      name: entry.name,
      parentPath: entry.parentPath,
      isFile: entry.isFile(),
      isDirectory: entry.isDirectory(),
      isSymbolicLink: entry.isSymbolicLink(),
    }))
    .sort((left, right) => left.name.localeCompare(right.name));
}

function bytesView(value) {
  return [...value];
}

const rows = [];
const record = (label) => rows.push(label);

// The raw store already contains a nested key. These calls exercise the
// driver's single path-to-key choke point with '.', '..', repeated slashes,
// and a trailing slash while comparing the resulting path and directory view.
{
  const values = new Map([["dir:file", new Uint8Array([1, 2, 3])]]);
  const pair = makePair(
    values,
    new Map([
      [
        "dir:file",
        {
          atime: new Date(1_699_999_999_000),
          mtime: new Date(1_700_000_000_000),
          ctime: new Date(1_700_000_000_500),
          birthtime: new Date(1_699_999_998_000),
        },
      ],
    ]),
  );
  try {
    await compare(
      pair,
      "normalized stat",
      () => pair.oracle.stat("/dir/./nested/../file"),
      () => pair.native.stat("/dir/./nested/../file"),
      statView,
    );
    record("normalized stat");

    await compare(
      pair,
      "normalized read",
      () => pair.oracle.readFile("/dir//file/"),
      () => pair.native.readFile("/dir//file/"),
      bytesView,
    );
    record("normalized read");

    await compare(
      pair,
      "normalized readdir",
      () => pair.oracle.readdir("/dir/../dir/"),
      () => pair.native.readdir("/dir/../dir/"),
      entriesView,
    );
    record("normalized readdir");

    const invalid = await compare(
      pair,
      "unrepresentable key",
      () => pair.oracle.writeFile("/dir/bad:name", new Uint8Array([9])),
      () => pair.native.writeFile("/dir/bad:name", new Uint8Array([9])),
      () => undefined,
    );
    assert.deepEqual(invalid, {
      error: { code: "EINVAL", syscall: "open", path: "/dir/bad:name", dest: null },
    });
    record("unrepresentable key");
  } finally {
    await pair.native.shutdown();
    await pair.oracle.shutdown?.();
  }
}

// Metadata supplied by a provider is visible to both adapters, while the
// chmod/chown/utimes overlay remains deterministic after the explicit calls.
{
  const metadata = new Map([
    [
      "metadata",
      {
        size: 1234,
        atime: new Date(1_699_999_999_000),
        mtime: new Date(1_700_000_000_000),
        ctime: new Date(1_700_000_000_500),
        birthtime: new Date(1_699_999_998_000),
      },
    ],
  ]);
  const pair = makePair(new Map([["metadata", new Uint8Array([1, 2, 3, 4])]]), metadata);
  try {
    await compare(
      pair,
      "provider metadata",
      () => pair.oracle.stat("/metadata"),
      () => pair.native.stat("/metadata"),
      statView,
    );
    record("provider metadata");

    await Promise.all([
      pair.oracle.chmod("/metadata", 0o604),
      pair.native.chmod("/metadata", 0o604),
    ]);
    await Promise.all([
      pair.oracle.chown("/metadata", 65_534, 65_534),
      pair.native.chown("/metadata", 65_534, 65_534),
    ]);
    await Promise.all([
      pair.oracle.utimes("/metadata", 1, 2),
      pair.native.utimes("/metadata", 1, 2),
    ]);
    const overlay = await compare(
      pair,
      "metadata overlay",
      () => pair.oracle.stat("/metadata"),
      () => pair.native.stat("/metadata"),
      (stats) => ({
        ...statView(stats),
        // ctime is intentionally process-local after a mutation; both sides
        // must update it, but their clocks need not tick at the same instant.
        ctimeMs: Number.isFinite(stats.ctimeMs),
      }),
    );
    assert.equal(overlay.mtimeMs, 2000);
    assert.equal(overlay.atimeMs, 1000);
    assert.equal(overlay.mode & 0o777, 0o604);
    assert.equal(overlay.uid, 65_534);
    assert.equal(overlay.gid, 65_534);
    assert.equal(overlay.ctimeMs, true);
    record("metadata overlay");
  } finally {
    await pair.native.shutdown();
    await pair.oracle.shutdown?.();
  }
}

// A write through one open handle is immediately visible through another
// handle, but does not reach the raw store until sync/close. An unlinked open
// handle stays readable and must never write its orphaned buffer back.
{
  const pair = makePair(
    new Map([
      ["shared", new Uint8Array([97, 97, 97, 97])],
      ["orphan", new Uint8Array([115, 116, 105, 108, 108])],
    ]),
  );
  try {
    const handles = await Promise.all([
      Promise.all([pair.oracle.open("/shared", "r+"), pair.native.open("/shared", "r+")]),
      Promise.all([pair.oracle.open("/shared", "r"), pair.native.open("/shared", "r")]),
    ]);
    const [oracleWriter, nativeWriter] = handles[0];
    const [oracleReader, nativeReader] = handles[1];
    try {
      const writeResult = await compare(
        pair,
        "shared handle write",
        () => oracleWriter.write(new Uint8Array([98, 98]), 0, 2, 0),
        () => nativeWriter.write(new Uint8Array([98, 98]), 0, 2, 0),
        (result) => ({ bytesWritten: result.bytesWritten }),
      );
      assert.equal(writeResult.bytesWritten, 2);
      record("shared handle write");

      const readResult = await compare(
        pair,
        "shared handle visibility",
        async () => {
          const buffer = new Uint8Array(4);
          const result = await oracleReader.read(buffer, 0, buffer.length, 0);
          return { bytesRead: result.bytesRead, bytes: bytesView(buffer) };
        },
        async () => {
          const buffer = new Uint8Array(4);
          const result = await nativeReader.read(buffer, 0, buffer.length, 0);
          return { bytesRead: result.bytesRead, bytes: bytesView(buffer) };
        },
      );
      assert.deepEqual(readResult, { bytesRead: 4, bytes: [98, 98, 97, 97] });
      assert.deepEqual([...pair.oracleStore.values.get("shared")], [97, 97, 97, 97]);
      assert.deepEqual([...pair.nativeStore.values.get("shared")], [97, 97, 97, 97]);
      record("shared handle visibility");

      await Promise.all([oracleWriter.sync(), nativeWriter.sync()]);
      assert.deepEqual([...pair.oracleStore.values.get("shared")], [98, 98, 97, 97]);
      assert.deepEqual([...pair.nativeStore.values.get("shared")], [98, 98, 97, 97]);
      record("shared handle sync");
    } finally {
      await Promise.all([
        oracleWriter.close(),
        nativeWriter.close(),
        oracleReader.close(),
        nativeReader.close(),
      ]);
    }

    const [oracleOrphan, nativeOrphan] = await Promise.all([
      pair.oracle.open("/orphan", "r+"),
      pair.native.open("/orphan", "r+"),
    ]);
    try {
      await Promise.all([pair.oracle.unlink("/orphan"), pair.native.unlink("/orphan")]);
      const orphanRead = await compare(
        pair,
        "orphan handle read",
        async () => {
          const buffer = new Uint8Array(5);
          const result = await oracleOrphan.read(buffer, 0, buffer.length, 0);
          return { bytesRead: result.bytesRead, bytes: bytesView(buffer) };
        },
        async () => {
          const buffer = new Uint8Array(5);
          const result = await nativeOrphan.read(buffer, 0, buffer.length, 0);
          return { bytesRead: result.bytesRead, bytes: bytesView(buffer) };
        },
      );
      assert.deepEqual(orphanRead, { bytesRead: 5, bytes: [115, 116, 105, 108, 108] });
      record("orphan handle read");

      await Promise.all([
        oracleOrphan.write(new Uint8Array([88]), 0, 1, 0),
        nativeOrphan.write(new Uint8Array([88]), 0, 1, 0),
      ]);
    } finally {
      await Promise.all([oracleOrphan.close(), nativeOrphan.close()]);
    }
    assert.equal(pair.oracleStore.values.has("orphan"), false);
    assert.equal(pair.nativeStore.values.has("orphan"), false);
    record("orphan handle no writeback");
  } finally {
    await pair.native.shutdown();
    await pair.oracle.shutdown?.();
  }
}

assert.equal(rows.length, 11, "the custom-store packet changed its row count unexpectedly");
console.log(
  `mount-rs Unstorage path/metadata/handle parity: PASS (${rows.length} rows, ` +
    `0 mismatches, 0 skipped; oracle=${revision})`,
);
