import assert from "node:assert/strict";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { createNodeFsDriver, Filesystem } from "@mount-rs/core";
import { MOUNT_RS_BACKEND_METHODS } from "@mount-rs/virtual-fs";
import {
  createBash,
  createJustBashFilesystem,
} from "@mount-rs/virtual-fs/just-bash";

assert.deepEqual(MOUNT_RS_BACKEND_METHODS, [
  "stat",
  "lstat",
  "readdir",
  "open",
  "readFile",
  "writeFile",
  "mkdir",
  "rmdir",
  "unlink",
  "rename",
  "link",
  "symlink",
  "readlink",
  "chmod",
  "utimes",
]);

const driver = Filesystem.memory();
const bash = await createBash(driver, { cwd: "/" });
const fs = bash.fs;

function bounded(promise, milliseconds = 1_000) {
  let timer;
  const timeout = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error("copy test exceeded timeout")), milliseconds);
  });
  return Promise.race([promise, timeout]).finally(() => clearTimeout(timer));
}

try {
  const shellResult = await bash.exec(
    "mkdir -p /drive/dir && printf 'hello\\n' > /drive/dir/text.txt && " +
      "mv /drive/dir/text.txt /drive/dir/renamed.txt && " +
      "cat /drive/dir/renamed.txt",
  );
  assert.equal(shellResult.exitCode, 0, shellResult.stderr);
  assert.equal(shellResult.stdout, "hello\n");

  const bytes = Uint8Array.of(0, 1, 2, 127, 128, 254, 255);
  await driver.writeFile("/drive/binary.bin", bytes);
  assert.deepEqual([...await fs.readFileBuffer("/drive/binary.bin")], [...bytes]);
  assert.deepEqual(
    [...Buffer.from(await fs.readFileBytes("/drive/binary.bin"), "latin1")],
    [...bytes],
  );
  const base64 = await bash.exec("base64 /drive/binary.bin");
  assert.equal(base64.exitCode, 0, base64.stderr);
  assert.equal(base64.stdout.trim(), Buffer.from(bytes).toString("base64"));

  await fs.symlink("/drive/dir/renamed.txt", "/drive/link.txt");
  const linkStats = await fs.lstat("/drive/link.txt");
  assert.equal(linkStats.isSymbolicLink, true);
  assert.equal(await fs.readlink("/drive/link.txt"), "/drive/dir/renamed.txt");
  assert.equal(await fs.realpath("/drive/link.txt"), "/drive/dir/renamed.txt");
  assert.equal(await fs.readFile("/drive/link.txt"), "hello\n");

  await fs.link("/drive/dir/renamed.txt", "/drive/hard-link.txt");
  assert.equal(await fs.readFile("/drive/hard-link.txt"), "hello\n");
  await fs.chmod("/drive/dir/renamed.txt", 0o640);
  assert.equal((await fs.stat("/drive/dir/renamed.txt")).mode & 0o777, 0o640);
  const mtime = new Date(1_700_000_000_000);
  await fs.utimes("/drive/dir/renamed.txt", mtime, mtime);
  assert.equal((await fs.stat("/drive/dir/renamed.txt")).mtime.getTime(), mtime.getTime());

  await fs.cp("/drive/dir", "/drive/copied", { recursive: true });
  assert.equal(await fs.readFile("/drive/copied/renamed.txt"), "hello\n");
  await fs.mv("/drive/copied/renamed.txt", "/drive/copied/moved.txt");
  await fs.rm("/drive/copied", { recursive: true });
  assert.equal(await fs.exists("/drive/copied"), false);

  await assert.rejects(
    bounded(fs.cp("/drive/dir", "/drive/dir/descendant", { recursive: true })),
    (error) => error?.code === "EINVAL",
  );
  assert.equal(await fs.exists("/drive/dir/descendant"), false);
  assert.equal(await fs.readFile("/drive/dir/renamed.txt"), "hello\n");
  await fs.symlink("/drive/dir", "/drive/dir-alias");
  await assert.rejects(
    bounded(fs.cp("/drive/dir", "/drive/dir-alias/descendant", { recursive: true })),
    (error) => error?.code === "EINVAL",
  );
  assert.equal(await fs.exists("/drive/dir/descendant"), false);
  assert.equal(await fs.readFile("/drive/dir/renamed.txt"), "hello\n");
  await fs.rm("/drive/dir-alias");
  await assert.rejects(
    bounded(fs.cp("/drive/link.txt", "/drive/link.txt")),
    (error) => error?.code === "EINVAL",
  );
  assert.equal(await fs.readlink("/drive/link.txt"), "/drive/dir/renamed.txt");
  await assert.rejects(
    bounded(fs.cp("/drive/dir/renamed.txt", "/drive/hard-link.txt")),
    (error) => error?.code === "EINVAL",
  );

  const snapshot = fs.getAllPaths();
  await fs.writeFile("/drive/snapshot.txt", "snapshot");
  assert.equal(snapshot.includes("/drive/snapshot.txt"), false);
  assert.equal(fs.getAllPaths().includes("/drive/snapshot.txt"), true);
  await driver.unlink("/drive/snapshot.txt");
  assert.equal(fs.getAllPaths().includes("/drive/snapshot.txt"), true);
  await fs.refreshPaths();
  assert.equal(fs.getAllPaths().includes("/drive/snapshot.txt"), false);

  await driver.writeFile("/drive/outside.txt", Uint8Array.from([111, 107]));
  assert.equal(fs.getAllPaths().includes("/drive/outside.txt"), false);
  await fs.refreshPaths();
  assert.equal(fs.getAllPaths().includes("/drive/outside.txt"), true);
  const glob = await bash.exec("printf '%s\\n' /drive/*.txt");
  assert.equal(glob.exitCode, 0, glob.stderr);
  assert.match(glob.stdout, /\/drive\/outside\.txt/);

  await assert.rejects(
    fs.readFileBuffer("/drive/missing.txt"),
    (error) => error?.code === "ENOENT" && error.path === "/drive/missing.txt",
  );
  assert.equal(fs.resolvePath("/drive/dir", "../outside.txt"), "/drive/outside.txt");

  const readOnly = await createJustBashFilesystem(driver, {
    readOnly: true,
    refreshPaths: false,
  });
  try {
    await assert.rejects(
      readOnly.writeFile("/drive/blocked.txt", "no"),
      (error) => error?.code === "EROFS",
    );
    assert.equal(await readOnly.readFile("/drive/outside.txt"), "ok");
  } finally {
    await readOnly.close();
  }
} finally {
  await fs.close();
  await driver.shutdown();
}

assert.equal(fs.closed, true);
await assert.rejects(
  fs.readFileBuffer("/drive/outside.txt"),
  (error) => error?.code === "EBADF",
);

async function testSqlitePersistence() {
  const root = await mkdtemp(join(tmpdir(), "mount-rs-virtual-bash-sqlite-"));
  const databasePath = join(root, "drive.sqlite");
  let firstDriver;
  let firstBash;
  let secondDriver;
  let secondBash;
  try {
    firstDriver = await Filesystem.sqlite(databasePath);
    firstBash = await createBash(firstDriver, { cwd: "/" });
    const writeResult = await firstBash.exec(
      "mkdir -p /persist && printf 'sqlite-persist\\n' > /persist/message.txt",
    );
    assert.equal(writeResult.exitCode, 0, writeResult.stderr);
    await firstBash.fs.close();
    await firstDriver.shutdown();
    firstBash = undefined;
    firstDriver = undefined;

    secondDriver = await Filesystem.sqlite(databasePath);
    secondBash = await createBash(secondDriver, { cwd: "/" });
    const readResult = await secondBash.exec("cat /persist/message.txt");
    assert.equal(readResult.exitCode, 0, readResult.stderr);
    assert.equal(readResult.stdout, "sqlite-persist\n");
    assert.equal(await secondBash.fs.readFile("/persist/message.txt"), "sqlite-persist\n");
  } finally {
    await secondBash?.fs.close();
    await secondDriver?.shutdown();
    await firstBash?.fs.close();
    await firstDriver?.shutdown();
    await rm(root, { recursive: true, force: true });
  }
}

async function testNativeNodeVisibilityAndReadonlyPolicy() {
  const root = await mkdtemp(join(tmpdir(), "mount-rs-virtual-bash-node-"));
  let driver;
  let bash;
  let readOnlyDriver;
  let readOnly;
  try {
    await writeFile(join(root, "host-seed.txt"), "host-seed\n");
    driver = createNodeFsDriver(root);
    bash = await createBash(driver, { cwd: "/" });
    const hostRead = await bash.exec("cat /host-seed.txt");
    assert.equal(hostRead.exitCode, 0, hostRead.stderr);
    assert.equal(hostRead.stdout, "host-seed\n");

    await bash.fs.writeFile("/from-adapter.bin", Uint8Array.of(0, 1, 255));
    assert.deepEqual([...await readFile(join(root, "from-adapter.bin"))], [0, 1, 255]);

    await writeFile(join(root, "external.txt"), "external\n");
    assert.equal(bash.fs.getAllPaths().includes("/external.txt"), false);
    assert.equal(await bash.fs.readFile("/external.txt"), "external\n");
    await bash.fs.refreshPaths();
    assert.equal(bash.fs.getAllPaths().includes("/external.txt"), true);

    await bash.fs.close();
    await driver.shutdown();
    bash = undefined;
    driver = undefined;

    readOnlyDriver = createNodeFsDriver(root, { readOnly: true });
    readOnly = await createJustBashFilesystem(readOnlyDriver, {
      readOnly: false,
      refreshPaths: false,
    });
    assert.equal(readOnlyDriver.capabilities.readOnly, true);
    assert.equal(readOnly.readOnly, true);
    await assert.rejects(
      readOnly.writeFile("/must-not-write", "blocked"),
      (error) => error?.code === "EROFS",
    );
  } finally {
    await readOnly?.close();
    await readOnlyDriver?.shutdown();
    await bash?.fs.close();
    await driver?.shutdown();
    await rm(root, { recursive: true, force: true });
  }
}

function shortWriteBackend(driver, { zeroProgress = false } = {}) {
  return {
    capabilities: driver.capabilities,
    shutdown: () => driver.shutdown(),
    stat: (...args) => driver.stat(...args),
    lstat: (...args) => driver.lstat(...args),
    readdir: (...args) => driver.readdir(...args),
    readFile: (...args) => driver.readFile(...args),
    writeFile: (...args) => driver.writeFile(...args),
    mkdir: (...args) => driver.mkdir(...args),
    rmdir: (...args) => driver.rmdir(...args),
    unlink: (...args) => driver.unlink(...args),
    rename: (...args) => driver.rename(...args),
    link: (...args) => driver.link(...args),
    symlink: (...args) => driver.symlink(...args),
    readlink: (...args) => driver.readlink(...args),
    chmod: (...args) => driver.chmod(...args),
    utimes: (...args) => driver.utimes(...args),
    open: async (...args) => {
      const handle = await driver.open(...args);
      return {
        write: async (buffer, offset, length, position) => {
          if (zeroProgress) return { bytesWritten: 0 };
          const shortLength = Math.max(1, Math.ceil(length / 2));
          return handle.write(buffer, offset, shortLength, position);
        },
        close: () => handle.close(),
      };
    },
  };
}

async function testShortWrites() {
  const actualDriver = Filesystem.memory();
  const adapter = await createJustBashFilesystem(shortWriteBackend(actualDriver), {
    refreshPaths: false,
  });
  try {
    await adapter.writeFile("/short.bin", Uint8Array.of(1, 2, 3, 4, 5), { overwrite: false });
    await adapter.appendFile("/short.bin", Uint8Array.of(6, 7, 8));
    assert.deepEqual(
      [...await actualDriver.readFile("/short.bin")],
      [1, 2, 3, 4, 5, 6, 7, 8],
    );
  } finally {
    await adapter.close();
    await actualDriver.shutdown();
  }

  const zeroDriver = Filesystem.memory();
  const zeroAdapter = await createJustBashFilesystem(shortWriteBackend(zeroDriver, { zeroProgress: true }), {
    refreshPaths: false,
  });
  try {
    await assert.rejects(
      zeroAdapter.writeFile("/zero-progress.bin", Uint8Array.of(1), { overwrite: false }),
      (error) => error?.code === "EIO",
    );
    assert.deepEqual([...await zeroDriver.readFile("/zero-progress.bin")], []);
  } finally {
    await zeroAdapter.close();
    await zeroDriver.shutdown();
  }
}

await testSqlitePersistence();
await testNativeNodeVisibilityAndReadonlyPolicy();
await testShortWrites();

console.log("virtual-fs just-bash integration: PASS");
