import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createRequire } from "node:module";
import { pathToFileURL } from "node:url";

const PINNED_ORACLE = "85361a8212ff9bff8e69f62fa8993ef2c2ec51e8";
const require = createRequire(import.meta.url);
const binding = require("../../integrations/mount-rs-napi/index.js");
const source = process.env.MOUNTX_SOURCE;
assert.ok(source, "MOUNTX_SOURCE must identify the pinned mountx oracle");
assert.equal(
  execFileSync("git", ["-C", source, "rev-parse", "HEAD"], { encoding: "utf8" }).trim(),
  PINNED_ORACLE,
  "MOUNTX_SOURCE is not the pinned mountx oracle",
);

const upstream = (path) => import(pathToFileURL(`${source}/${path}`).href);
const upstreamRequire = createRequire(pathToFileURL(`${source}/package.json`));
const [
  { createUnstorageDriver: createOracleDriver },
  { createLoopback, resolveCapabilities },
  errors,
  storageModule,
  memoryModule,
] = await Promise.all([
  upstream("src/drivers/unstorage.ts"),
  upstream("src/harness.ts"),
  upstream("src/errors.ts"),
  import(pathToFileURL(upstreamRequire.resolve("unstorage")).href),
  import(pathToFileURL(upstreamRequire.resolve("unstorage/drivers/memory")).href),
]);

const { createStorage } = storageModule;
const { default: memoryStorageDriver } = memoryModule;

/** The byte-oriented callback store shared by the oracle and native adapter. */
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
    return value === undefined ? null : new Uint8Array(value);
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
  // The overlay makes permission/timestamp rows deterministic. It is not a
  // claim that the process has host-level root privileges.
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
  return { oracle, native };
}

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

function errorView(error) {
  return {
    code: error?.code ?? null,
    errno: error?.errno ?? null,
    syscall: error?.syscall ?? null,
    path: error?.path ?? null,
    dest: error?.dest ?? null,
  };
}

async function capture(operation, project = (value) => value) {
  try {
    return { ok: true, value: project(await operation()) };
  } catch (error) {
    return { ok: false, error: errorView(error) };
  }
}

function statsView(stats) {
  return {
    mode: stats.mode,
    uid: stats.uid,
    gid: stats.gid,
    nlink: stats.nlink,
    size: stats.size,
    isFile: stats.isFile(),
    isSymbolicLink: stats.isSymbolicLink(),
  };
}

const counts = { PASS: 0, ENOSYS: 0, ENOTSUP: 0 };
const rows = [];

/**
 * Compare one complete row against the pinned TypeScript adapter. An
 * unsupported operation is asserted as an exact result, never converted into
 * a skipped test. The syscall and errno are part of the preserved evidence.
 */
async function runRow({ label, syscall, expectedError, scenario, project = (value) => value }) {
  const pair = makePair();
  try {
    const [expected, actual] = await Promise.all([
      capture(() => scenario(pair.oracle), project),
      capture(() => scenario(pair.native), project),
    ]);
    assert.deepEqual(actual, expected, `${label}: native result differs from oracle`);

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

function mknodOperation(fs, surface, path, mode, dev) {
  const operation = surface === "direct" ? fs.mknod : fs.mountx?.mknod;
  if (typeof operation === "function") {
    return operation.call(surface === "direct" ? fs : fs.mountx, path, mode, dev);
  }
  return Promise.reject(errors.fsError("ENOSYS", { syscall: "mknod" }));
}

// This is a nested cross-directory alias request, distinct from the existing
// root-level hardlink rows. The false capability must be observable before a
// key-value store invents inode aliases or path validation changes the answer.
await runRow({
  label: "nested cross-directory hardlink boundary",
  syscall: "link",
  scenario: async (fs) => {
    await fs.mkdir("/left");
    await fs.mkdir("/right");
    await fs.writeFile("/left/source", new Uint8Array([1, 2, 3]));
    await fs.link("/left/source", "/right/alias");
  },
});

// The optional type argument and a missing parent are separate symlink
// boundary inputs. Neither may turn the absent node-kind representation into
// a path-dependent success or a broad skipped row.
await runRow({
  label: "symlink explicit type boundary",
  syscall: "symlink",
  scenario: async (fs) => fs.symlink("target", "/link", "junction"),
});

await runRow({
  label: "symlink missing-parent boundary",
  syscall: "symlink",
  scenario: async (fs) => fs.symlink("target", "/missing/link"),
});

// Generic Unstorage has no capacity contract. Check both a real nested path
// and a missing path so the refusal remains capability-driven rather than
// accidentally becoming a fabricated capacity result or ENOENT.
await runRow({
  label: "nested statfs capability boundary",
  syscall: "statfs",
  scenario: async (fs) => {
    await fs.mkdir("/dir");
    await fs.writeFile("/dir/file", new Uint8Array([1]));
    return fs.statfs("/dir");
  },
});

await runRow({
  label: "missing-path statfs capability boundary",
  syscall: "statfs",
  scenario: async (fs) => fs.statfs("/missing"),
});

// Exercise both public mknod surfaces and path contexts. The prior packet used
// one selected facade; each public surface must still retain the exact absent
// extension boundary.
for (const surface of ["direct", "mountx"]) {
  await runRow({
    label: `mknod ${surface} regular/no-type boundary`,
    syscall: "mknod",
    scenario: async (fs) => mknodOperation(fs, surface, "/regular", 0o100600, 0),
  });
  await runRow({
    label: `mknod ${surface} missing-parent boundary`,
    syscall: "mknod",
    scenario: async (fs) => mknodOperation(fs, surface, "/missing/node", 0o010600, 0),
  });
}

// `lutimes` is available as an ordinary-file overlay even when the symlink
// capability is false. Its missing-path error is a supported ENOENT boundary,
// not the symlink-gated ENOSYS classification.
await runRow({
  label: "lutimes missing-path supported boundary",
  expectedError: "ENOENT",
  scenario: async (fs) => fs.lutimes("/missing", 1, 2),
});

// Preserve root-shaped ownership semantics without claiming a real uid-0
// process. `-1` means leave that side unchanged in the upstream contract.
await runRow({
  label: "lchown partial gid overlay",
  scenario: async (fs) => {
    await fs.writeFile("/file", new Uint8Array([1]));
    await fs.lchown("/file", -1, 42);
    const stats = await fs.stat("/file");
    return { uid: stats.uid, gid: stats.gid };
  },
});

await runRow({
  label: "lchown partial uid overlay",
  scenario: async (fs) => {
    await fs.writeFile("/file", new Uint8Array([1]));
    await fs.lchown("/file", 42, -1);
    const stats = await fs.stat("/file");
    return { uid: stats.uid, gid: stats.gid };
  },
});

await runRow({
  label: "lchown unchanged-owner overlay",
  scenario: async (fs) => {
    await fs.writeFile("/file", new Uint8Array([1]));
    const before = await fs.stat("/file");
    await fs.lchown("/file", -1, -1);
    const after = await fs.stat("/file");
    return {
      before: { uid: before.uid, gid: before.gid },
      after: { uid: after.uid, gid: after.gid },
    };
  },
});

await runRow({
  label: "setuid/setgid/sticky permission overlay",
  scenario: async (fs) => {
    await fs.writeFile("/file", new Uint8Array([1]));
    await fs.chmod("/file", 0o7751);
    return fs.stat("/file");
  },
  project: (stats) => ({ ...statsView(stats), mode: stats.mode & 0o7777 }),
});

assert.deepEqual(
  capabilityView(
    resolveCapabilities(createOracleDriver(createStorage({ driver: memoryStorageDriver() }), options)),
  ),
  {
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
  },
  "pinned oracle capability profile changed",
);

const evidencePair = makePair();
try {
  assert.deepEqual(
    capabilityView(evidencePair.native.capabilities),
    capabilityView(evidencePair.oracle.capabilities),
    "native capability profile differs from the pinned oracle",
  );
} finally {
  await evidencePair.native.shutdown();
  await evidencePair.oracle.shutdown?.();
}

assert.equal(rows.length, 14, "next capability inventory changed without updating this test");
assert.equal(counts.PASS + counts.ENOSYS + counts.ENOTSUP, rows.length);
console.log(
  `mount-rs Unstorage next capability parity: PASS (${rows.length} rows: ` +
    `${counts.PASS} PASS, ${counts.ENOSYS} ENOSYS, ${counts.ENOTSUP} ENOTSUP, 0 skipped; ` +
    `oracle=${PINNED_ORACLE})`,
);
console.log(`mount-rs Unstorage next capability rows: ${rows.join(", ")}`);
