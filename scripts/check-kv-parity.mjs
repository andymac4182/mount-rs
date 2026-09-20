#!/usr/bin/env node

/**
 * Differential check for the Rust key-value driver and the pinned mountx
 * unstorage driver.
 *
 * The TypeScript side imports the actual pinned `src/drivers/unstorage.ts`
 * and supplies only the Storage methods that adapter uses. This keeps the
 * oracle independent of an installed unstorage package while still checking
 * the real driver implementation, including its byte-buffer and metadata
 * rules.
 *
 * Usage:
 *
 *   MOUNTX_SOURCE=/tmp/mountx-source.uWiHfX node scripts/check-kv-parity.mjs
 */

import assert from "node:assert/strict";
import { constants } from "node:fs";
import { execFileSync } from "node:child_process";
import { fileURLToPath, pathToFileURL } from "node:url";

const repo = fileURLToPath(new URL("..", import.meta.url));
const source = process.env.MOUNTX_SOURCE ?? "/tmp/mountx-source.uWiHfX";
const sourceRoot = pathToFileURL(source.endsWith("/") ? source : `${source}/`);

const [{ createUnstorageDriver }, { createLoopback }] = await Promise.all([
  import(new URL("src/drivers/unstorage.ts", sourceRoot).href),
  import(new URL("src/harness.ts", sourceRoot).href),
]);

const encoder = new TextEncoder();

function cloneBytes(value) {
  if (value instanceof Uint8Array) return new Uint8Array(value);
  if (value instanceof ArrayBuffer) return new Uint8Array(value.slice(0));
  return value;
}

function bytes(value) {
  return value == null ? null : value instanceof Uint8Array ? [...value] : [...value];
}

/** A minimal byte-oriented Storage compatible with the upstream adapter. */
class RawStore {
  constructor() {
    this.values = new Map();
    this.metadata = new Map();
    this.failNext = false;
    this.delayedSet = null;
  }

  put(key, value) {
    this.values.set(key, encoder.encode(value));
  }

  putBytes(key, value) {
    this.values.set(key, new Uint8Array(value));
  }

  putMeta(key, metadata) {
    this.metadata.set(key, metadata);
  }

  failNextSet() {
    this.failNext = true;
  }

  delayNextSet() {
    let arrive;
    let release;
    const reached = new Promise((resolve) => {
      arrive = resolve;
    });
    const resume = new Promise((resolve) => {
      release = resolve;
    });
    this.delayedSet = { arrive, resume };
    return { reached, release };
  }

  value(key) {
    return bytes(this.values.get(key));
  }

  async hasItem(key) {
    return this.values.has(key);
  }

  async getItemRaw(key) {
    return cloneBytes(this.values.get(key)) ?? null;
  }

  async setItemRaw(key, value) {
    const delayed = this.delayedSet;
    if (delayed) {
      this.delayedSet = null;
      delayed.arrive();
      await delayed.resume;
    }
    if (this.failNext) {
      this.failNext = false;
      throw new Error("transient write failure");
    }
    this.values.set(key, new Uint8Array(value));
  }

  async removeItem(key, options = {}) {
    this.values.delete(key);
    if (options.removeMeta) this.metadata.delete(key);
  }

  async getKeys(prefix = "") {
    const childPrefix = prefix === "" ? "" : `${prefix}:`;
    return [...this.values.keys()].filter(
      (key) =>
        !key.endsWith("$") &&
        (prefix === "" || key === prefix || key.startsWith(childPrefix)),
    );
  }

  async getMeta(key) {
    return this.metadata.get(key) ?? {};
  }
}

function options() {
  return {
    uid: 1000,
    gid: 1001,
    fileMode: 0o644,
    dirMode: 0o755,
    readOnly: false,
  };
}

function fs(store, driverOptions = options()) {
  return createLoopback(createUnstorageDriver(store, driverOptions));
}

function errorShape(error, fallback = {}) {
  // The upstream adapter forwards an arbitrary Storage rejection unchanged;
  // the Rust driver maps that backend failure to the filesystem contract's
  // EIO with the operation context. Normalize only that transport-level
  // difference so the data and retry semantics remain directly comparable.
  return {
    code: typeof error?.code === "string" ? error.code : "EIO",
    syscall: error?.syscall ?? fallback.syscall ?? null,
    path: error?.path ?? fallback.path ?? null,
    dest: error?.dest ?? fallback.dest ?? null,
  };
}

async function capture(operation, fallback = {}) {
  try {
    await operation;
    return null;
  } catch (error) {
    return errorShape(error, fallback);
  }
}

async function captureMessage(operation, fallback = {}) {
  try {
    await operation;
    return null;
  } catch (error) {
    return {
      ...errorShape(error, fallback),
      message: error?.message ?? String(error),
    };
  }
}

function stableStats(stats, includeTimes) {
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
    isBlockDevice: stats.isBlockDevice(),
    isCharacterDevice: stats.isCharacterDevice(),
    isFIFO: stats.isFIFO(),
    isSocket: stats.isSocket(),
  };
  if (includeTimes) {
    value.atimeMs = stats.atimeMs;
    value.mtimeMs = stats.mtimeMs;
    value.birthtimeMs = stats.birthtimeMs;
  }
  return value;
}

function stableCapabilities(capabilities) {
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
  };
}

function sortedEntries(entries) {
  return entries
    .map((entry) => ({
      name: entry.name,
      kind: entry.isDirectory() ? "directory" : "file",
    }))
    .sort((left, right) => left.name.localeCompare(right.name));
}

async function readAt(handle, length, position) {
  const buffer = new Uint8Array(length);
  const result = await handle.read(buffer, 0, length, position);
  return [...buffer.subarray(0, result.bytesRead)];
}

async function basicScenario() {
  const store = new RawStore();
  store.put("note", "text");
  store.put("note$", "hidden metadata");
  store.put("shadow", "file wins");
  store.put("shadow:child", "unreachable");
  store.put("meta", "four");
  store.putMeta("meta", { size: 1234, mtime: new Date(1_700_000_000_000) });

  const loopback = fs(store);
  await loopback.mkdir("/a/b", { recursive: true });
  await loopback.writeFile("/a/b/c.bin", new Uint8Array([0, 1, 254, 255]));

  const binary = bytes(await loopback.readFile("/a/b/c.bin"));
  const nested = sortedEntries(await loopback.readdir("/a", { withFileTypes: true }));
  const rootEntries = sortedEntries(await loopback.readdir("/", { withFileTypes: true }));
  const binaryStats = stableStats(await loopback.stat("/a/b/c.bin"), false);

  const metadataStats = stableStats(await loopback.stat("/meta"), true);
  await loopback.chmod("/meta", 0o600);
  // The Rust driver contract carries milliseconds; the upstream Node API
  // carries seconds, so use 1s/2s on this side.
  await loopback.utimes("/meta", 1, 2);
  const metadataOverlay = stableStats(await loopback.stat("/meta"), true);

  const invalid = {
    colon: {
      write: await capture(loopback.writeFile("/a:b", encoder.encode("x"))),
      stat: await capture(loopback.stat("/a:b")),
      mkdir: await capture(loopback.mkdir("/a:b")),
    },
    question: {
      write: await capture(loopback.writeFile("/a?b", encoder.encode("x"))),
      stat: await capture(loopback.stat("/a?b")),
      mkdir: await capture(loopback.mkdir("/a?b")),
    },
    metadataSuffix: {
      write: await capture(loopback.writeFile("/meta$", encoder.encode("x"))),
      stat: await capture(loopback.stat("/meta$")),
      mkdir: await capture(loopback.mkdir("/meta$")),
    },
    missing: await capture(loopback.stat("/missing")),
    missingChild: await capture(loopback.stat("/missing/child")),
  };

  const shadow = {
    kind: (await loopback.stat("/shadow")).isFile() ? "file" : "directory",
    child: await capture(loopback.stat("/shadow/child")),
  };

  // Rust's decoded numeric flag namespace is Linux's O_* namespace. The
  // upstream driver receives the host constants but the resulting semantics
  // are the same on macOS and Linux.
  const createBits = 0o100 | 2;
  const createFlags = constants.O_CREAT | constants.O_RDWR;
  const numericCreate = await loopback.open("/numeric", createFlags, 0o640);
  await numericCreate.write(encoder.encode("n"), 0, 1, 0);
  await numericCreate.close();
  const appendBits = 0o100 | 1 | 0o2000;
  const appendFlags = constants.O_CREAT | constants.O_WRONLY | constants.O_APPEND;
  const numericAppend = await loopback.open("/numeric", appendFlags, 0);
  await numericAppend.write(encoder.encode("+a"), 0, 2, 0);
  await numericAppend.close();

  return {
    binary,
    nestedEntries: nested,
    rootEntries,
    binaryStats,
    metadataStats,
    metadataOverlay,
    invalid,
    shadow,
    numeric: {
      createBits,
      appendBits,
      data: store.value("numeric"),
      stats: stableStats(await loopback.stat("/numeric"), false),
    },
    capabilities: stableCapabilities(loopback.capabilities),
    stored: {
      "a:b:c.bin": store.value("a:b:c.bin"),
      note: store.value("note"),
      "note$": store.value("note$"),
      meta: store.value("meta"),
    },
  };
}

async function handlesScenario() {
  const sharedStore = new RawStore();
  const sharedFs = fs(sharedStore);
  await sharedFs.writeFile("/shared", encoder.encode("aaaa"));
  const writer = await sharedFs.open("/shared", "r+");
  const reader = await sharedFs.open("/shared", "r");
  await writer.write(encoder.encode("bb"), 0, 2, 0);
  const visible = await readAt(reader, 4, 0);
  const beforeSync = sharedStore.value("shared");
  const beforeSyncStats = stableStats(await sharedFs.stat("/shared"), false);
  await writer.sync();
  const afterSync = sharedStore.value("shared");
  await writer.close();
  await reader.close();

  const failureStore = new RawStore();
  const failureFs = fs(failureStore);
  await failureFs.writeFile("/failure", encoder.encode("aaaa"));
  const failureHandle = await failureFs.open("/failure", "r+");
  await failureHandle.write(encoder.encode("bbbb"), 0, 4, 0);
  failureStore.failNextSet();
  const failure = await captureMessage(failureHandle.close(), {
    syscall: "fsync",
    path: "/failure",
  });
  const retry = await failureFs.open("/failure", "r+");
  await retry.sync();
  await retry.close();
  await failureHandle.close();

  const truncateStore = new RawStore();
  const truncateFs = fs(truncateStore);
  await truncateFs.writeFile("/truncate", encoder.encode("aaaa"));
  const truncateHandle = await truncateFs.open("/truncate", "r+");
  await truncateHandle.write(encoder.encode("bbbb"), 0, 4, 0);
  truncateStore.failNextSet();
  await captureMessage(truncateHandle.close(), {
    syscall: "fsync",
    path: "/truncate",
  });
  await truncateFs.truncate("/truncate", 2);

  const unlinkStore = new RawStore();
  const unlinkFs = fs(unlinkStore);
  await unlinkFs.writeFile("/doomed", encoder.encode("still here"));
  const doomed = await unlinkFs.open("/doomed", "r+");
  await unlinkFs.unlink("/doomed");
  const orphanRead = await readAt(doomed, 10, 0);
  const orphanStat = stableStats(await doomed.stat(), false);
  await doomed.write(encoder.encode("x"), 0, 1, 0);
  await doomed.close();

  const replacedStore = new RawStore();
  const replacedFs = fs(replacedStore);
  await replacedFs.writeFile("/source", encoder.encode("source"));
  await replacedFs.writeFile("/target", encoder.encode("target"));
  const replaced = await replacedFs.open("/target", "r+");
  await replacedFs.rename("/source", "/target");
  await replaced.write(encoder.encode("zzzzzz"), 0, 6, 0);
  await replaced.close();

  const renamedStore = new RawStore();
  const renamedFs = fs(renamedStore);
  await renamedFs.writeFile("/from", encoder.encode("aaaa"));
  const renamed = await renamedFs.open("/from", "r+");
  await renamed.write(encoder.encode("bb"), 0, 2, 0);
  await renamedFs.rename("/from", "/to");
  await renamed.write(encoder.encode("cc"), 0, 2, 2);
  await renamed.close();

  const zeroStore = new RawStore();
  const zeroFs = fs(zeroStore);
  await zeroFs.writeFile("/zero", encoder.encode("x"));
  const zero = await zeroFs.open("/zero", "r+");
  await zero.write(new Uint8Array(), 0, 0, 3);
  const zeroStats = stableStats(await zeroFs.stat("/zero"), false);
  await zero.close();

  return {
    shared: {
      visibleBeforeSync: visible,
      storedBeforeSync: beforeSync,
      statsBeforeSync: beforeSyncStats,
      storedAfterSync: afterSync,
    },
    flushFailure: {
      error: failure,
      storedAfterRetry: failureStore.value("failure"),
    },
    truncateAfterFailure: truncateStore.value("truncate"),
    unlink: {
      read: orphanRead,
      stat: orphanStat,
      stored: unlinkStore.value("doomed"),
    },
    replacedDestination: {
      target: bytes(await replacedFs.readFile("/target")),
      source: replacedStore.value("source"),
    },
    renamedOpen: {
      from: renamedStore.value("from"),
      to: renamedStore.value("to"),
    },
    zeroPositionWrite: {
      data: zeroStore.value("zero"),
      stats: zeroStats,
    },
  };
}

async function edgeScenario() {
  const sharedStore = new RawStore();
  const sharedFs = fs(sharedStore);
  await sharedFs.writeFile("/shared", encoder.encode("abcdef"));
  const reader = await sharedFs.open("/shared", "r");
  const truncating = await sharedFs.open("/shared", "w");
  const shared = {
    storedBeforeClose: sharedStore.value("shared"),
    readerAfterTruncate: await readAt(reader, 6, 0),
    statsAfterTruncate: stableStats(await sharedFs.stat("/shared"), false),
  };
  await truncating.close();
  await reader.close();
  shared.storedAfterClose = sharedStore.value("shared");

  const pendingStore = new RawStore();
  const pendingFs = fs(pendingStore);
  await pendingFs.writeFile("/pending", encoder.encode("aaaa"));
  const first = await pendingFs.open("/pending", "r+");
  const second = await pendingFs.open("/pending", "r+");
  await first.write(encoder.encode("bbbb"), 0, 4, 0);
  const gate = pendingStore.delayNextSet();
  const syncing = first.sync();
  await gate.reached;
  await second.write(encoder.encode("cccc"), 0, 4, 0);
  gate.release();
  await syncing;
  await first.close();
  await second.close();
  const pendingFlush = pendingStore.value("pending");

  const renamedStore = new RawStore();
  const renamedFs = fs(renamedStore);
  await renamedFs.writeFile("/from", encoder.encode("old"));
  const clean = await renamedFs.open("/from", "r");
  await renamedFs.rename("/from", "/to");
  await clean.close();
  renamedStore.put("to", "fresh");
  const cleanRename = {
    from: renamedStore.value("from"),
    to: bytes(await renamedFs.readFile("/to")),
  };

  const metadataStore = new RawStore();
  const metadataFs = fs(metadataStore);
  metadataStore.put("meta", "old");
  metadataStore.putMeta("meta", { size: 1234 });
  await metadataFs.unlink("/meta");
  const recreated = await metadataFs.open("/meta", "w");
  await recreated.close();
  const metadataAfterRecreate = stableStats(await metadataFs.stat("/meta"), false);

  const operationsStore = new RawStore();
  const operationsFs = fs(operationsStore);
  await operationsFs.writeFile("/file", encoder.encode("x"));
  await operationsFs.mkdir("/empty");
  await operationsFs.mkdir("/nonempty");
  await operationsFs.writeFile("/nonempty/child", encoder.encode("x"));
  await operationsFs.mkdir("/source");
  await operationsFs.writeFile("/source/child", encoder.encode("x"));
  await operationsFs.mkdir("/destination");
  await operationsFs.writeFile("/destination/child", encoder.encode("x"));
  const recursiveFirst = await operationsFs.mkdir("/created/leaf", { recursive: true });
  const recursiveAgain = await operationsFs.mkdir("/created/leaf", { recursive: true });
  const operationErrors = {
    mkdirExisting: await capture(operationsFs.mkdir("/file")),
    mkdirThroughFile: await capture(
      operationsFs.mkdir("/file/child", { recursive: true }),
    ),
    rmdirFile: await capture(operationsFs.rmdir("/file")),
    rmdirMissing: await capture(operationsFs.rmdir("/missing")),
    rmdirNonempty: await capture(operationsFs.rmdir("/nonempty")),
    rmdirRoot: await capture(operationsFs.rmdir("/")),
    unlinkDirectory: await capture(operationsFs.unlink("/empty")),
    renameMissing: await capture(operationsFs.rename("/missing", "/new")),
    renameFileToDirectory: await capture(operationsFs.rename("/file", "/empty")),
    renameDirectoryToFile: await capture(operationsFs.rename("/source", "/file")),
    renameDirectoryIntoSelf: await capture(
      operationsFs.rename("/source", "/source/child/deeper"),
    ),
    renameDirectoryNonempty: await capture(
      operationsFs.rename("/source", "/destination"),
    ),
  };

  const handleStore = new RawStore();
  const handleFs = fs(handleStore);
  await handleFs.writeFile("/f", encoder.encode("x"));
  const readOnlyHandle = await handleFs.open("/f", "r");
  const directoryHandle = await operationsFs.open("/empty", "r");
  const oneByte = new Uint8Array(1);
  const handleErrors = {
    writeOnReadOnly: await capture(
      readOnlyHandle.write(encoder.encode("y"), 0, 1, 0),
    ),
    truncateOnReadOnly: await capture(readOnlyHandle.truncate(0)),
    directoryRead: await capture(directoryHandle.read(oneByte, 0, 1, 0)),
    directoryWriteOpen: await capture(operationsFs.open("/empty", "w")),
  };
  await directoryHandle.close();
  await readOnlyHandle.close();
  handleErrors.readAfterClose = await capture(
    readOnlyHandle.read(oneByte, 0, 1, 0),
  );

  return {
    sharedTruncate: shared,
    pendingFlush,
    cleanRename,
    metadataAfterRecreate,
    operations: {
      recursiveFirst: recursiveFirst ?? null,
      recursiveAgain: recursiveAgain ?? null,
      errors: operationErrors,
      source: bytes(await operationsFs.readFile("/source/child")),
      destination: bytes(await operationsFs.readFile("/destination/child")),
    },
    handleErrors,
  };
}

async function resolutionScenario() {
  const shadowStore = new RawStore();
  shadowStore.put("a:b", "shadowing");
  shadowStore.put("a:b:c:d", "deep");
  const shadowFs = fs(shadowStore);
  const below = {
    "/a/b/c": await capture(shadowFs.stat("/a/b/c")),
    "/a/b/c/d/e": await capture(shadowFs.stat("/a/b/c/d/e")),
    "/a/b/zz": await capture(shadowFs.stat("/a/b/zz")),
  };
  const missing = {
    "/a/zz/c": await capture(shadowFs.stat("/a/zz/c")),
    "/zz/b/c": await capture(shadowFs.stat("/zz/b/c")),
  };

  const truncateStore = new RawStore();
  const truncateFs = fs(truncateStore);
  truncateStore.put("f", "abcdef");
  await truncateFs.truncate("/f", 3);

  return {
    shadow: {
      kind: (await shadowFs.stat("/a/b")).isFile() ? "file" : "directory",
      below,
      leaf: (await shadowFs.stat("/a/b/c/d")).isFile() ? "file" : "directory",
      missing,
    },
    nonzeroTruncate: bytes(await truncateFs.readFile("/f")),
  };
}

async function readonlyScenario() {
  const store = new RawStore();
  store.put("f", "content");
  const readOnly = fs(store, { ...options(), readOnly: true });
  const read = bytes(await readOnly.readFile("/f"));
  const createAppend = constants.O_CREAT | constants.O_WRONLY | constants.O_APPEND;
  const errors = {
    openExisting: await capture(readOnly.open("/f", "w")),
    openNew: await capture(readOnly.open("/new", createAppend)),
    mkdir: await capture(readOnly.mkdir("/dir")),
    rmdir: await capture(readOnly.rmdir("/dir")),
    unlink: await capture(readOnly.unlink("/f")),
    rename: await capture(readOnly.rename("/f", "/g")),
    truncate: await capture(readOnly.truncate("/f", 0)),
    chmod: await capture(readOnly.chmod("/f", 0o600)),
    chown: await capture(readOnly.chown("/f", 0, 0)),
    utimes: await capture(readOnly.utimes("/f", 1000, 2000)),
  };

  const unsupportedStore = new RawStore();
  const unsupportedFs = fs(unsupportedStore);
  await unsupportedFs.writeFile("/f", encoder.encode("x"));
  const unsupported = {
    link: await capture(unsupportedFs.link("/f", "/g")),
    symlink: await capture(unsupportedFs.symlink("f", "/g")),
    readlink: await capture(unsupportedFs.readlink("/f")),
    statfs: await capture(unsupportedFs.statfs("/")),
  };

  return {
    read,
    capabilities: stableCapabilities(readOnly.capabilities),
    errors,
    unsupported,
    stored: store.value("f"),
  };
}

const upstream = {
  basic: await basicScenario(),
  handles: await handlesScenario(),
  edges: await edgeScenario(),
  resolution: await resolutionScenario(),
  readonly: await readonlyScenario(),
};
const environment = { ...process.env, MOUNTX_SOURCE: source };
const rust = JSON.parse(
  execFileSync(
    "cargo",
    ["run", "-p", "mount-rs-kv", "--locked", "--quiet", "--example", "kv_oracle"],
    {
      cwd: repo,
      env: environment,
      encoding: "utf8",
    },
  ),
);

assert.deepStrictEqual(rust, upstream);
console.log("mountx unstorage KV parity: PASS");
console.log(JSON.stringify(rust, null, 2));
