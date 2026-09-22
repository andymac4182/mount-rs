import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createRequire } from "node:module";
import { pathToFileURL } from "node:url";

const PINNED_ORACLE = "85361a8212ff9bff8e69f62fa8993ef2c2ec51e8";
const require = createRequire(import.meta.url);
const binding = require("../../bindings/mount-rs-napi/index.js");
const source = process.env.MOUNTX_SOURCE;
assert.ok(source, "MOUNTX_SOURCE must identify the pinned mountx oracle");
assert.equal(
  execFileSync("git", ["-C", source, "rev-parse", "HEAD"], { encoding: "utf8" }).trim(),
  PINNED_ORACLE,
  "MOUNTX_SOURCE is not the pinned mountx oracle",
);

const upstream = (path) => import(pathToFileURL(`${source}/${path}`).href);
const upstreamRequire = createRequire(pathToFileURL(`${source}/package.json`));
const [{ createUnstorageDriver: createOracleDriver }, { createLoopback }, errors, storageModule] =
  await Promise.all([
    upstream("src/drivers/unstorage.ts"),
    upstream("src/harness.ts"),
    upstream("src/errors.ts"),
    import(pathToFileURL(upstreamRequire.resolve("unstorage")).href),
  ]);

const { createStorage } = storageModule;

/** The smallest raw callback store shared by the oracle and Rust adapters. */
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

  getKeys(prefix = "") {
    prefix ??= "";
    const childPrefix = prefix === "" ? "" : `${prefix}:`;
    return [...this.values.keys()].filter(
      (key) => prefix === "" || key === prefix || key.startsWith(childPrefix),
    );
  }

  getMeta(key) {
    return this.metadata.get(key) ?? {};
  }

  asOracleDriver() {
    return {
      hasItem: this.hasItem.bind(this),
      getItemRaw: this.getItemRaw.bind(this),
      setItemRaw: this.setItemRaw.bind(this),
      removeItem: this.removeItem.bind(this),
      getKeys: this.getKeys.bind(this),
      getMeta: this.getMeta.bind(this),
    };
  }
}

const options = {
  // These are process-local metadata overlays in both adapters. They make the
  // permission rows deterministic without pretending the test has kernel root.
  uid: 0,
  gid: 0,
  fileMode: 0o640,
  dirMode: 0o750,
};

function makePair() {
  const oracleStore = new RawStore();
  const nativeStore = new RawStore();
  const oracleStorage = createStorage({ driver: oracleStore.asOracleDriver() });
  const oracle = createLoopback(createOracleDriver(oracleStorage, options));
  const native = binding.createUnstorageDriver(nativeStore, options);
  return { oracle, native, oracleStore, nativeStore };
}

function errorView(error) {
  return {
    code: error?.code ?? null,
    errno: error?.errno ?? null,
    syscall: error?.syscall ?? null,
    path: error?.path ?? null,
    dest: error?.dest ?? null,
    message: error?.message ?? null,
  };
}

async function capture(operation, project = (value) => value) {
  try {
    return { ok: true, value: project(await operation()) };
  } catch (error) {
    return { ok: false, error: errorView(error) };
  }
}

function resultView(result) {
  if (result === undefined || result === null) return result;
  if (typeof result === "object" && "bytesWritten" in result) {
    return { bytesWritten: result.bytesWritten };
  }
  return result;
}

async function prepareFile(fs, path, contents = new Uint8Array([1])) {
  await writeFile(fs, path, contents);
  assert.deepEqual([...await fs.readFile(path)], [...contents], `${path}: prepare did not persist`);
}

async function prepareDirectory(fs, path) {
  await mkdir(fs, path);
  assert.equal((await fs.stat(path)).isDirectory(), true, `${path}: prepare did not create a directory`);
}

async function filesystemSnapshot(fs) {
  const entries = [];

  async function visit(path) {
    const children = await fs.readdir(path);
    children.sort((left, right) => left.name.localeCompare(right.name));
    for (const child of children) {
      const childPath = path === "/" ? `/${child.name}` : `${path}/${child.name}`;
      const stats = await fs.lstat(childPath);
      const entry = {
        path: childPath,
        mode: stats.mode,
        nlink: stats.nlink,
        uid: stats.uid,
        gid: stats.gid,
        size: stats.size,
        atimeMs: stats.atimeMs,
        mtimeMs: stats.mtimeMs,
        isFile: stats.isFile(),
        isDirectory: stats.isDirectory(),
        isSymbolicLink: stats.isSymbolicLink(),
      };
      if (stats.isFile()) entry.bytes = [...await fs.readFile(childPath)];
      entries.push(entry);
      if (stats.isDirectory()) await visit(childPath);
    }
  }

  await visit("/");
  return entries;
}

async function stateSnapshot(fs, store) {
  return {
    filesystem: await filesystemSnapshot(fs),
    raw: {
      values: structuredClone(store.values),
      metadata: structuredClone(store.metadata),
    },
  };
}

const counts = { PASS: 0, ENOSYS: 0, ENOTSUP: 0, SKIP: 0 };
const rows = [];

/**
 * Compare one complete upstream scenario. An unsupported result is a result,
 * not a skip: both adapters must return the oracle's exact stable refusal.
 */
async function runRow({
  label,
  syscall,
  expectedError,
  prepare,
  scenario,
  project = resultView,
}) {
  const pair = makePair();
  try {
    if (prepare) {
      await Promise.all([prepare(pair.oracle), prepare(pair.native)]);
    }
    const [oracleBefore, nativeBefore] = await Promise.all([
      stateSnapshot(pair.oracle, pair.oracleStore),
      stateSnapshot(pair.native, pair.nativeStore),
    ]);
    const [expected, actual] = await Promise.all([
      capture(() => scenario(pair.oracle), project),
      capture(() => scenario(pair.native), project),
    ]);
    assert.deepEqual(actual, expected, `${label}: native result differs from oracle`);

    if (!expected.ok) {
      const [oracleAfter, nativeAfter] = await Promise.all([
        stateSnapshot(pair.oracle, pair.oracleStore),
        stateSnapshot(pair.native, pair.nativeStore),
      ]);
      assert.deepEqual(oracleAfter, oracleBefore, `${label}: oracle refusal changed state`);
      assert.deepEqual(nativeAfter, nativeBefore, `${label}: native refusal changed state`);
    }

    if (expected.ok) {
      counts.PASS++;
      rows.push(`${label}=PASS`);
      return;
    }

    if (expected.error.code === "ENOSYS" || expected.error.code === "ENOTSUP") {
      assert.equal(expected.error.syscall, syscall, `${label}: syscall classification changed`);
      assert.equal(
        expected.error.errno,
        -errors.ERRNO_CODES[expected.error.code],
        `${label}: unsupported errno classification changed`,
      );
      counts[expected.error.code]++;
      rows.push(`${label}=${expected.error.code}`);
      return;
    }
    assert.equal(expected.error.code, expectedError, `${label}: unexpected oracle error`);
    assert.equal(
      expected.error.errno,
      -errors.ERRNO_CODES[expectedError],
      `${label}: errno classification changed`,
    );
    counts.PASS++;
    rows.push(`${label}=PASS(${expected.error.code})`);
  } finally {
    await pair.native.shutdown();
    await pair.oracle.shutdown?.();
  }
}

async function writeFile(fs, path, contents = new Uint8Array([1])) {
  await fs.writeFile(path, contents);
}

async function mkdir(fs, path) {
  await fs.mkdir(path);
}

function mknodOperation(fs, path, mode, dev) {
  const operation = fs.mountx?.mknod ?? fs.mknod;
  if (typeof operation === "function") return operation.call(fs.mountx ?? fs, path, mode, dev);
  return Promise.reject(errors.fsError("ENOSYS", { syscall: "mknod" }));
}

// The upstream conformance case is gated on symlinks. Exercise the complete
// case, so ENOSYS at creation is recorded explicitly rather than becoming a
// pending row that hides the later O_EXCL behavior.
const symlinkOpenRows = [
  {
    label: "symlink-gated exclusive open of dangling link",
    syscall: "symlink",
    scenario: async (fs) => {
      await fs.symlink("nowhere", "/dangling");
      const handle = await fs.open("/dangling", "wx");
      await handle.close();
    },
  },
  {
    label: "symlink-gated exclusive open of valid link",
    syscall: "symlink",
    prepare: (fs) => prepareFile(fs, "/target"),
    scenario: async (fs) => {
      await fs.symlink("target", "/link");
      const handle = await fs.open("/link", "wx");
      await handle.close();
    },
  },
];
for (const row of symlinkOpenRows) await runRow(row);

// Keep every symlink-gated conformance scenario visible even though this
// adapter intentionally has no symlink representation. Each scenario is
// reduced to the first operation that the upstream test needs; ENOSYS is a
// result asserted against the pinned oracle, never a skipped test.
const symlinkRows = [
  {
    label: "symlink readlink/stat prerequisite",
    syscall: "symlink",
    prepare: (fs) => prepareFile(fs, "/target"),
    scenario: async (fs) => fs.symlink("target", "/link"),
  },
  {
    label: "symlinked directory traversal prerequisite",
    syscall: "symlink",
    prepare: (fs) => prepareDirectory(fs, "/real"),
    scenario: async (fs) => fs.symlink("real", "/alias"),
  },
  {
    label: "dangling symlink lstat/readlink prerequisite",
    syscall: "symlink",
    scenario: async (fs) => fs.symlink("nowhere", "/dangling"),
  },
  {
    label: "symlink loop prerequisite",
    syscall: "symlink",
    scenario: async (fs) => fs.symlink("loop-b", "/loop-a"),
  },
  {
    label: "readlink regular-file error boundary",
    syscall: "readlink",
    prepare: (fs) => prepareFile(fs, "/file"),
    scenario: async (fs) => fs.readlink("/file"),
  },
  {
    label: "unicode symlink size prerequisite",
    syscall: "symlink",
    scenario: async (fs) => fs.symlink("héllo→ø", "/unicode"),
  },
  {
    label: "symlink existing-name refusal prerequisite",
    syscall: "symlink",
    prepare: (fs) => prepareFile(fs, "/taken"),
    scenario: async (fs) => fs.symlink("whatever", "/taken"),
  },
];
for (const row of symlinkRows) await runRow(row);

// The three hard-link conformance behaviors are kept together as full
// scenarios. The first is the positive inode/nlink contract; the other two
// preserve the required error distinctions if hardlinks are implemented later.
const hardlinkRows = [
  {
    label: "hardlink inode sharing and unlink lifetime",
    syscall: "link",
    prepare: (fs) => prepareFile(fs, "/original", new TextEncoder().encode("shared")),
    scenario: async (fs) => {
      await fs.link("/original", "/alias");
      const original = await fs.stat("/original");
      const alias = await fs.stat("/alias");
      assert.equal(alias.ino, original.ino);
      assert.equal(alias.nlink, 2);
      await fs.unlink("/original");
      assert.equal((await fs.stat("/alias")).nlink, 1);
    },
  },
  {
    label: "hardlink existing destination refusal",
    syscall: "link",
    prepare: async (fs) => {
      await prepareFile(fs, "/source");
      await prepareFile(fs, "/taken");
    },
    scenario: async (fs) => {
      await fs.link("/source", "/taken");
    },
  },
  {
    label: "hardlink directory refusal",
    syscall: "link",
    prepare: (fs) => prepareDirectory(fs, "/directory"),
    scenario: async (fs) => {
      await fs.link("/directory", "/directory-link");
    },
  },
  {
    label: "hardlink missing source refusal",
    syscall: "link",
    scenario: async (fs) => {
      await fs.link("/missing", "/alias");
    },
  },
];
for (const row of hardlinkRows) await runRow(row);

// statfs is a single upstream skip, but its full answer is important because
// transports use it for capacity reporting. No synthetic values are accepted.
await runRow({
  label: "statfs root capacity",
  syscall: "statfs",
  scenario: async (fs) => {
    const stats = await fs.statfs("/");
    assert.ok(stats.bsize > 0);
    assert.ok(stats.blocks > 0);
    assert.ok(stats.bfree >= 0);
  },
});

// mknod is an optional mountx extension. Keep each call in its own row: a
// capability refusal on the first call must not hide an independent mode or
// path boundary later in the upstream scenario. The oracle's absent
// extension is classified with its own ENOSYS helper; the Rust extension must
// match it, not invent EPERM.
const mknodRows = [
  {
    label: "mknod FIFO creation boundary",
    syscall: "mknod",
    scenario: async (fs) => mknodOperation(fs, "/fifo", 0o010644, 0),
  },
  {
    label: "mknod socket creation boundary",
    syscall: "mknod",
    scenario: async (fs) => mknodOperation(fs, "/sock", 0o140600, 0),
  },
  {
    label: "mknod character-device boundary",
    syscall: "mknod",
    scenario: async (fs) => mknodOperation(fs, "/char", 0o020666, (1 << 8) | 3),
  },
  {
    label: "mknod block-device boundary",
    syscall: "mknod",
    scenario: async (fs) => mknodOperation(fs, "/block", 0o060660, 7 << 8),
  },
  {
    label: "mknod regular/no-type plain-file boundary",
    syscall: "mknod",
    scenario: async (fs) => mknodOperation(fs, "/plain", 0o644, 0),
  },
  {
    label: "mknod regular-file mode boundary",
    syscall: "mknod",
    scenario: async (fs) => mknodOperation(fs, "/regular", 0o100600, 0),
  },
  {
    label: "mknod directory type refusal",
    syscall: "mknod",
    scenario: async (fs) => mknodOperation(fs, "/directory", 0o040755, 0),
  },
  {
    label: "mknod ordinary-name creation boundary",
    syscall: "mknod",
    scenario: async (fs) => mknodOperation(fs, "/fifo", 0o010644, 0),
  },
  {
    label: "mknod ordinary-name rename/unlink boundary",
    prepare: (fs) => prepareFile(fs, "/fifo"),
    scenario: async (fs) => {
      await fs.rename("/fifo", "/moved");
      await fs.unlink("/moved");
      assert.deepEqual(await fs.readdir("/"), [], "ordinary-name lifecycle left an entry behind");
    },
  },
  {
    label: "mknod existing-name refusal",
    syscall: "mknod",
    prepare: (fs) => prepareFile(fs, "/taken"),
    scenario: async (fs) => {
      await mknodOperation(fs, "/taken", 0o010644, 0);
    },
  },
  {
    label: "mknod missing-parent refusal",
    syscall: "mknod",
    scenario: async (fs) => {
      await mknodOperation(fs, "/nowhere/fifo", 0o010644, 0);
    },
  },
];
for (const row of mknodRows) await runRow(row);

// Positive controls for the root-shaped Unstorage ownership overlay. These
// rows keep the permission behavior from being confused with the symlink/root
// prerequisite of the upstream lchown case.
await runRow({
  label: "regular-file permission and ownership overlay",
  scenario: async (fs) => {
    await writeFile(fs, "/owned");
    await fs.chmod("/owned", 0o600);
    await fs.chown("/owned", 65_534, 65_534);
    await fs.lchown("/owned", 0, 0);
    const stats = await fs.stat("/owned");
    assert.equal(stats.mode & 0o777, 0o600);
    assert.equal(stats.uid, 0);
    assert.equal(stats.gid, 0);
  },
});

await runRow({
  label: "regular-file timestamp overlay",
  scenario: async (fs) => {
    await writeFile(fs, "/timed");
    await fs.utimes("/timed", new Date(1_000), new Date(2_000));
    await fs.utimes("/timed", 5, 6);
    const stats = await fs.stat("/timed");
    assert.equal(stats.atimeMs, 5_000);
    assert.equal(stats.mtimeMs, 6_000);
  },
});

// Missing-path controls ensure a supported metadata method does not turn an
// upstream ordinary ENOENT into a silent skip or an invented capability error.
await runRow({
  label: "chmod missing path",
  expectedError: "ENOENT",
  scenario: async (fs) => fs.chmod("/missing", 0o644),
});

await runRow({
  label: "utimes missing path",
  expectedError: "ENOENT",
  scenario: async (fs) => fs.utimes("/missing", 1, 1),
});

// The symlink-gated timestamp and root-ownership rows are represented by the
// first operation that can be asked on this adapter. If symlinks are ever
// enabled, the complete downstream distinction is checked as well.
await runRow({
  label: "symlink-gated lutimes target-vs-link",
  syscall: "symlink",
  prepare: (fs) => prepareFile(fs, "/target"),
  scenario: async (fs) => {
    await fs.symlink("target", "/link");
    await fs.utimes("/target", new Date(1_000), new Date(1_000));
    await fs.lutimes("/link", new Date(9_000), new Date(9_000));
    assert.equal((await fs.lstat("/link")).mtimeMs, 9_000);
    assert.equal((await fs.stat("/target")).mtimeMs, 1_000);
  },
});

await runRow({
  label: "symlink-gated root-only lchown target-vs-link",
  syscall: "symlink",
  prepare: (fs) => prepareFile(fs, "/target"),
  scenario: async (fs) => {
    await fs.symlink("target", "/link");
    const before = await fs.stat("/target");
    await fs.lchown("/link", 65_534, 65_534);
    assert.equal((await fs.lstat("/link")).uid, 65_534);
    assert.equal((await fs.stat("/target")).uid, before.uid);
    await fs.chown("/link", 65_534, 65_534);
    assert.equal((await fs.stat("/target")).uid, 65_534);
  },
});

assert.equal(rows.length, 31, "remaining-skip inventory changed without updating this test");
assert.equal(counts.PASS + counts.ENOSYS + counts.ENOTSUP, rows.length);
assert.equal(counts.SKIP, 0, "capability-gated skips must remain in the upstream suite");
console.log(
  `mount-rs Unstorage remaining-skip parity: PASS (${rows.length} rows: ` +
    `${counts.PASS} PASS, ${counts.ENOSYS} ENOSYS, ${counts.ENOTSUP} ENOTSUP, ` +
    `${counts.SKIP} skipped; ` +
    `oracle=${PINNED_ORACLE})`,
);
console.log(`mount-rs Unstorage remaining-skip rows: ${rows.join(", ")}`);
