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
] = await Promise.all([
  upstream("src/drivers/unstorage.ts"),
  upstream("src/harness.ts"),
  upstream("src/errors.ts"),
  import(pathToFileURL(upstreamRequire.resolve("unstorage")).href),
]);

const { createStorage } = storageModule;

/** A byte-oriented callback store used by both Unstorage adapters. */
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

  getKeys(prefix = "") {
    const childPrefix = prefix === "" ? "" : `${prefix}:`;
    return [...this.values.keys()].filter(
      (key) => prefix === "" || key === prefix || key.startsWith(childPrefix),
    );
  }

  getMeta() {
    return {};
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

function makePair() {
  const oracleStore = new RawStore();
  const nativeStore = new RawStore();
  const oracleStorage = createStorage({ driver: oracleStore.asOracleDriver() });
  const oracle = createLoopback(createOracleDriver(oracleStorage));
  const native = binding.createUnstorageDriver(nativeStore);
  return { oracle, native };
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

async function capture(operation) {
  try {
    return { ok: true, value: await operation() };
  } catch (error) {
    return { ok: false, error: errorView(error) };
  }
}

const counts = { PASS: 0, ENOSYS: 0, ENOTSUP: 0 };
const rows = [];

function formatError(error) {
  return `errno=${error.errno},syscall=${error.syscall},path=${JSON.stringify(error.path)},dest=${JSON.stringify(error.dest)}`;
}

/**
 * Compare one atomic hardlink operation against the pinned Unstorage oracle.
 * The oracle's missing capability is an explicit ENOSYS result, never a skip.
 */
async function runRow({ label, expectedError, scenario }) {
  const pair = makePair();
  try {
    assert.equal(pair.oracle.capabilities.hardlinks, false, `${label}: oracle capability changed`);
    assert.equal(pair.native.capabilities.hardlinks, false, `${label}: native capability changed`);
    assert.equal(typeof pair.oracle.link, "function", `${label}: oracle link facade missing`);
    assert.equal(typeof pair.native.link, "function", `${label}: native link facade missing`);

    const [expected, actual] = await Promise.all([
      capture(() => scenario(pair.oracle)),
      capture(() => scenario(pair.native)),
    ]);
    assert.deepEqual(actual, expected, `${label}: native result differs from oracle`);

    if (expected.ok) {
      counts.PASS++;
      rows.push(`${label}=PASS`);
      return;
    }

    if (expected.error.code === "ENOSYS" || expected.error.code === "ENOTSUP") {
      assert.equal(expected.error.code, "ENOSYS", `${label}: unsupported classification changed`);
      assert.equal(expected.error.syscall, "link", `${label}: syscall classification changed`);
      assert.equal(
        expected.error.errno,
        -errors.ERRNO_CODES.ENOSYS,
        `${label}: errno classification changed`,
      );
      counts[expected.error.code]++;
      rows.push(`${label}=${expected.error.code}(${formatError(expected.error)})`);
      return;
    }

    assert.equal(expected.error.code, expectedError, `${label}: supported error changed`);
    assert.equal(
      expected.error.errno,
      -errors.ERRNO_CODES[expectedError],
      `${label}: supported errno changed`,
    );
    assert.equal(expected.error.syscall, "link", `${label}: syscall classification changed`);
    counts.PASS++;
    rows.push(`${label}=PASS(${expected.error.code})`);
  } finally {
    await pair.native.shutdown();
    await pair.oracle.shutdown?.();
  }
}

/**
 * The pinned upstream conformance inventory has three hardlink test blocks.
 * Its final block contains two independent link calls, so this packet keeps
 * the directory and missing-source refusals as separate atomic rows. It uses
 * only the Unstorage oracle/native adapters; MemoryFs and ChunkedFs are not
 * evidence for this capability boundary.
 */
const hardlinkRows = [
  {
    label: "hardlink inode sharing and unlink lifetime",
    scenario: async (fs) => {
      await fs.writeFile("/original", new TextEncoder().encode("shared"));
      await fs.link("/original", "/alias");

      const original = await fs.stat("/original");
      const alias = await fs.stat("/alias");
      assert.equal(alias.ino, original.ino);
      assert.equal(alias.nlink, 2);
      assert.equal(new TextDecoder().decode(await fs.readFile("/alias")), "shared");

      await fs.writeFile("/original", new TextEncoder().encode("updated"));
      assert.equal(new TextDecoder().decode(await fs.readFile("/alias")), "updated");

      await fs.unlink("/original");
      assert.equal((await fs.stat("/alias")).nlink, 1);
      assert.equal(new TextDecoder().decode(await fs.readFile("/alias")), "updated");
    },
  },
  {
    label: "hardlink existing destination refusal",
    expectedError: "EEXIST",
    scenario: async (fs) => {
      await fs.writeFile("/source", new Uint8Array([1]));
      await fs.writeFile("/taken", new Uint8Array([2]));
      await fs.link("/source", "/taken");
    },
  },
  {
    label: "hardlink directory source refusal",
    expectedError: "EPERM",
    scenario: async (fs) => {
      await fs.mkdir("/directory");
      await fs.link("/directory", "/directory-link");
    },
  },
  {
    label: "hardlink missing source refusal",
    expectedError: "ENOENT",
    scenario: async (fs) => fs.link("/missing", "/alias"),
  },
];

const oracleCapabilities = resolveCapabilities(
  createOracleDriver(createStorage({ driver: new RawStore().asOracleDriver() })),
);
assert.equal(oracleCapabilities.hardlinks, false, "pinned oracle hardlinks capability changed");
assert.equal(hardlinkRows.length, 4, "hardlink inventory changed without updating this test");
for (const row of hardlinkRows) await runRow(row);

assert.equal(counts.PASS, 0, "hardlink packet unexpectedly used supported rows");
assert.equal(counts.ENOSYS, hardlinkRows.length, "hardlink packet lost explicit ENOSYS rows");
assert.equal(counts.ENOTSUP, 0, "hardlink packet changed from ENOSYS to ENOTSUP");
console.log(
  `mount-rs Unstorage hardlink capability boundary: PASS (${hardlinkRows.length} rows: ` +
    `${counts.PASS} supported, ${counts.ENOSYS} ENOSYS, ${counts.ENOTSUP} ENOTSUP, 0 skipped; ` +
    `oracle=${PINNED_ORACLE})`,
);
console.log(`mount-rs Unstorage hardlink rows: ${rows.join(", ")}`);
