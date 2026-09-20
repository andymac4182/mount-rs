import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { pathToFileURL } from "node:url";

const require = createRequire(import.meta.url);
const binding = require("../../integrations/mount-rs-napi/index.js");
const source = process.env.MOUNTX_SOURCE;
assert.ok(source, "MOUNTX_SOURCE must identify the pinned upstream checkout");

const upstream = (path) => import(pathToFileURL(`${source}/${path}`).href);
const upstreamRequire = createRequire(pathToFileURL(`${source}/package.json`));
const [{ createUnstorageDriver: createOracleDriver }, { createLoopback }, storageModule, memoryModule] =
  await Promise.all([
    upstream("src/drivers/unstorage.ts"),
    upstream("src/harness.ts"),
    import(pathToFileURL(upstreamRequire.resolve("unstorage")).href),
    import(pathToFileURL(upstreamRequire.resolve("unstorage/drivers/memory")).href),
  ]);

const { createStorage } = storageModule;
const { default: memoryStorageDriver } = memoryModule;

/** The minimum callback surface needed to make an in-memory Unstorage store. */
class RawStore {
  constructor() {
    this.values = new Map();
  }

  hasItem(key) {
    return this.values.has(key);
  }

  getItemRaw(key) {
    const value = this.values.get(key);
    return value === undefined ? null : new Uint8Array(value);
  }

  setItemRaw(key, value) {
    assert.equal(value instanceof Uint8Array, true, "native store receives bytes");
    this.values.set(key, new Uint8Array(value));
  }

  removeItem(key) {
    this.values.delete(key);
  }

  getKeys(prefix) {
    const childPrefix = prefix === "" ? "" : `${prefix}:`;
    return [...this.values.keys()].filter(
      (key) => prefix === "" || key === prefix || key.startsWith(childPrefix),
    );
  }

  getMeta() {
    return {};
  }
}

const options = {
  // Use the same root-shaped metadata overlay as the existing capability test.
  // These values do not grant host privileges; the focused rows below must not
  // turn a non-root runner into evidence for the upstream root prerequisite.
  uid: 0,
  gid: 0,
  fileMode: 0o640,
  dirMode: 0o750,
};

function makePair() {
  const oracleStorage = createStorage({ driver: memoryStorageDriver() });
  const oracle = createLoopback(createOracleDriver(oracleStorage, options));
  const store = new RawStore();
  const native = binding.createUnstorageDriver(store, options);
  return { oracle, native, store };
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

async function writeFile(pair, path, contents = new Uint8Array([1])) {
  await Promise.all([pair.oracle.writeFile(path, contents), pair.native.writeFile(path, contents)]);
}

async function mkdir(pair, path) {
  await Promise.all([pair.oracle.mkdir(path), pair.native.mkdir(path)]);
}

/**
 * Exercise the first unsupported operation needed by one upstream skipped row.
 * The operation is intentionally not replaced with a synthetic errno: the
 * pinned oracle decides whether the row is ENOSYS or ENOTSUP, and the Rust
 * bridge must return that exact classification and syscall.
 */
async function assertUnsupported({ label, syscall, prepare, oracleOperation, nativeOperation }) {
  const pair = makePair();
  try {
    await prepare?.(pair);
    const [expected, actual] = await Promise.all([
      capture(() => oracleOperation(pair.oracle)),
      capture(() => nativeOperation(pair.native)),
    ]);
    assert.deepEqual(actual, expected, `${label}: native result differs from oracle`);
    assert.ok(
      expected && (expected.code === "ENOSYS" || expected.code === "ENOTSUP"),
      `${label}: oracle did not classify the boundary as ENOSYS/ENOTSUP`,
    );
    assert.equal(expected.syscall, syscall, `${label}: oracle syscall classification changed`);
    return expected.code;
  } finally {
    await pair.native.shutdown();
    await pair.oracle.shutdown?.();
  }
}

const rows = [
  {
    label: "hardlink to an existing destination",
    syscall: "link",
    prepare: (pair) => Promise.all([writeFile(pair, "/source"), writeFile(pair, "/taken")]),
    oracleOperation: (fs) => fs.link("/source", "/taken"),
    nativeOperation: (fs) => fs.link("/source", "/taken"),
  },
  {
    label: "hardlink of a directory",
    syscall: "link",
    prepare: (pair) => mkdir(pair, "/directory"),
    oracleOperation: (fs) => fs.link("/directory", "/directory-link"),
    nativeOperation: (fs) => fs.link("/directory", "/directory-link"),
  },
  {
    label: "hardlink with a missing source",
    syscall: "link",
    oracleOperation: (fs) => fs.link("/missing", "/alias"),
    nativeOperation: (fs) => fs.link("/missing", "/alias"),
  },
  {
    label: "exclusive open of a dangling symlink prerequisite",
    syscall: "symlink",
    oracleOperation: (fs) => fs.symlink("nowhere", "/dangling"),
    nativeOperation: (fs) => fs.symlink("nowhere", "/dangling"),
  },
  {
    label: "symlinked directory traversal prerequisite",
    syscall: "symlink",
    prepare: (pair) => mkdir(pair, "/real"),
    oracleOperation: (fs) => fs.symlink("real", "/alias"),
    nativeOperation: (fs) => fs.symlink("real", "/alias"),
  },
  {
    label: "dangling symlink visibility prerequisite",
    syscall: "symlink",
    oracleOperation: (fs) => fs.symlink("nowhere", "/dangling"),
    nativeOperation: (fs) => fs.symlink("nowhere", "/dangling"),
  },
  {
    label: "symlink loop prerequisite",
    syscall: "symlink",
    oracleOperation: (fs) => fs.symlink("loop-b", "/loop-a"),
    nativeOperation: (fs) => fs.symlink("loop-b", "/loop-a"),
  },
  {
    label: "unicode symlink size prerequisite",
    syscall: "symlink",
    oracleOperation: (fs) => fs.symlink("héllo→ø", "/unicode"),
    nativeOperation: (fs) => fs.symlink("héllo→ø", "/unicode"),
  },
  {
    label: "symlink over an existing name prerequisite",
    syscall: "symlink",
    prepare: (pair) => writeFile(pair, "/taken"),
    oracleOperation: (fs) => fs.symlink("whatever", "/taken"),
    nativeOperation: (fs) => fs.symlink("whatever", "/taken"),
  },
  {
    // `lutimes` itself is supported by Unstorage. Calling it on a missing path
    // would only prove ENOENT, not the skipped `times + symlinks` row. The
    // oracle's first required operation is symlink creation, whose ENOSYS is
    // the honest capability boundary for the link-specific timestamp case.
    label: "lutimes on a symlink prerequisite",
    syscall: "symlink",
    prepare: (pair) => writeFile(pair, "/target"),
    oracleOperation: (fs) => fs.symlink("target", "/link"),
    nativeOperation: (fs) => fs.symlink("target", "/link"),
  },
  {
    // The upstream lchown row is additionally root-gated because it changes
    // ownership away from the current user. Unstorage's root-shaped overlay
    // already proves ordinary chown/lchown in capability-parity.mjs; this row
    // remains link-gated, so classify the missing symlink without pretending a
    // non-root host executed the link-versus-target ownership assertion.
    label: "root-only lchown of a symlink prerequisite",
    syscall: "symlink",
    prepare: (pair) => writeFile(pair, "/target"),
    oracleOperation: (fs) => fs.symlink("target", "/link"),
    nativeOperation: (fs) => fs.symlink("target", "/link"),
  },
];

const counts = { ENOSYS: 0, ENOTSUP: 0 };
for (const row of rows) {
  const code = await assertUnsupported(row);
  counts[code]++;
}

console.log(
  `mount-rs Unstorage edge parity: PASS (${rows.length} rows: ` +
    `${counts.ENOSYS} ENOSYS, ${counts.ENOTSUP} ENOTSUP, 0 skipped; ` +
    "hardlink edges and symlink-dependent timestamp/root rows classified)",
);
