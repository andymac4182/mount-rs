import assert from "node:assert/strict";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { createNodeFsDriver, Filesystem } from "@andymac4182/mount-rs";
import {
  DirectoryNotEmptyError,
  FileExistsError,
  FileNotFoundError,
  StaleFileError,
  Workspace,
  WorkspaceReadOnlyError,
} from "@mastra/core/workspace";
import { createMastraFilesystem } from "@andymac4182/mount-rs-virtual-fs/mastra";

const driver = Filesystem.memory();
const filesystem = createMastraFilesystem(driver, {
  id: "logical-drive",
  name: "Logical drive",
  closeOnDestroy: true,
});
const workspace = new Workspace({ filesystem });

await workspace.init();
assert.equal(filesystem.isReady(), true);
assert.equal(filesystem.getInfo().metadata.hostMount, false);

await workspace.filesystem.mkdir("/docs");
const bytes = Uint8Array.of(0, 3, 127, 128, 255);
await workspace.filesystem.writeFile("/docs/data.bin", bytes);
const data = await workspace.filesystem.readFile("/docs/data.bin");
assert.equal(Buffer.isBuffer(data), true);
assert.deepEqual([...data], [...bytes]);
await workspace.filesystem.writeFile("/docs/readme.txt", "hello\n");
assert.equal(await workspace.filesystem.readFile("/docs/readme.txt", { encoding: "utf8" }), "hello\n");

const listed = await workspace.filesystem.readdir("/docs");
assert.deepEqual(listed.map((entry) => entry.name).sort(), ["data.bin", "readme.txt"]);
const dataStat = await workspace.filesystem.stat("/docs/data.bin");
assert.equal(dataStat.type, "file");
assert.equal(dataStat.size, bytes.byteLength);

await filesystem.symlink("/docs/data.bin", "/docs/data-link");
const symlinkEntry = (await workspace.filesystem.readdir("/docs"))
  .find((entry) => entry.name === "data-link");
assert.equal(symlinkEntry.isSymlink, true);
assert.equal(symlinkEntry.symlinkTarget, "/docs/data.bin");
assert.equal(await filesystem.readlink("/docs/data-link"), "/docs/data.bin");
assert.deepEqual(
  [...(await workspace.filesystem.readFile("/docs/data-link"))],
  [...bytes],
);
assert.equal(await filesystem.realpath("/docs/data-link"), "/docs/data.bin");

const oldMtime = (await workspace.filesystem.stat("/docs/readme.txt")).modifiedAt;
await workspace.filesystem.writeFile("/docs/readme.txt", "changed\n");
await assert.rejects(
  workspace.filesystem.writeFile("/docs/readme.txt", "conflict\n", { overwrite: false }),
  (error) => error instanceof FileExistsError && error.path === "/docs/readme.txt",
);
await assert.rejects(
  workspace.filesystem.writeFile("/docs/readme.txt", "stale\n", {
    expectedMtime: new Date(0),
  }),
  (error) => error instanceof StaleFileError && error.expectedMtime.getTime() === 0,
);
assert.notEqual(
  (await workspace.filesystem.stat("/docs/readme.txt")).modifiedAt.getTime(),
  oldMtime.getTime(),
);

await workspace.filesystem.copyFile("/docs/data.bin", "/docs/copied.bin");
await assert.rejects(
  workspace.filesystem.moveFile("/docs/data.bin", "/docs/copied.bin", { overwrite: false }),
  (error) => error instanceof FileExistsError && error.path === "/docs/copied.bin",
);
await workspace.filesystem.moveFile("/docs/copied.bin", "/docs/moved.bin");
await workspace.filesystem.deleteFile("/docs/moved.bin");
assert.equal(await workspace.filesystem.exists("/docs/moved.bin"), false);

await assert.rejects(
  workspace.filesystem.readFile("/docs/missing.txt"),
  (error) => error instanceof FileNotFoundError && error.path === "/docs/missing.txt",
);
await assert.rejects(
  workspace.filesystem.rmdir("/docs"),
  (error) => error instanceof DirectoryNotEmptyError && error.path === "/docs",
);

await workspace.filesystem.deleteFile("/docs/data-link");
await workspace.filesystem.deleteFile("/docs/data.bin");
await workspace.filesystem.deleteFile("/docs/readme.txt");
await workspace.filesystem.rmdir("/docs");
assert.equal(await workspace.filesystem.exists("/docs"), false);

const readOnlyDriver = Filesystem.memory();
const readOnly = createMastraFilesystem(readOnlyDriver, { readOnly: true, closeOnDestroy: true });
await readOnly.init();
await assert.rejects(
  readOnly.writeFile("/blocked.txt", "no"),
  (error) => error instanceof WorkspaceReadOnlyError,
);
await readOnly.destroy();

await workspace.destroy();
assert.equal(filesystem.status, "destroyed");
await assert.rejects(
  filesystem.readFile("/docs/missing.txt"),
  (error) => error?.code === "EBADF",
);

async function testSqlitePersistence() {
  const root = await mkdtemp(join(tmpdir(), "mount-rs-virtual-mastra-sqlite-"));
  const databasePath = join(root, "drive.sqlite");
  let firstDriver;
  let firstWorkspace;
  let secondDriver;
  let secondWorkspace;
  try {
    firstDriver = await Filesystem.sqlite(databasePath);
    const firstFilesystem = createMastraFilesystem(firstDriver, {
      id: "sqlite-first",
      closeOnDestroy: true,
    });
    firstWorkspace = new Workspace({ filesystem: firstFilesystem });
    await firstWorkspace.init();
    await firstWorkspace.filesystem.mkdir("/persist");
    await firstWorkspace.filesystem.writeFile("/persist/message.txt", "sqlite-mas");
    await firstWorkspace.destroy();
    firstWorkspace = undefined;
    firstDriver = undefined;

    secondDriver = await Filesystem.sqlite(databasePath);
    const secondFilesystem = createMastraFilesystem(secondDriver, {
      id: "sqlite-second",
      closeOnDestroy: true,
    });
    secondWorkspace = new Workspace({ filesystem: secondFilesystem });
    await secondWorkspace.init();
    assert.equal(
      await secondWorkspace.filesystem.readFile("/persist/message.txt", { encoding: "utf8" }),
      "sqlite-mas",
    );
  } finally {
    await secondWorkspace?.destroy();
    await secondDriver?.shutdown();
    await firstWorkspace?.destroy();
    await firstDriver?.shutdown();
    await rm(root, { recursive: true, force: true });
  }
}

async function testNativeNodeVisibilityAndReadonlyPolicy() {
  const root = await mkdtemp(join(tmpdir(), "mount-rs-virtual-mastra-node-"));
  let driver;
  let workspace;
  let readOnlyDriver;
  let readOnlyWorkspace;
  try {
    await writeFile(join(root, "host-seed.txt"), "host-seed\n");
    driver = createNodeFsDriver(root);
    const filesystem = createMastraFilesystem(driver, {
      id: "node-fs",
      closeOnDestroy: true,
    });
    workspace = new Workspace({ filesystem });
    await workspace.init();
    assert.equal(
      await workspace.filesystem.readFile("/host-seed.txt", { encoding: "utf8" }),
      "host-seed\n",
    );
    await workspace.filesystem.writeFile("/from-adapter.bin", Uint8Array.of(0, 1, 255));
    assert.deepEqual([...await readFile(join(root, "from-adapter.bin"))], [0, 1, 255]);
    await writeFile(join(root, "external.txt"), "external\n");
    assert.equal(await workspace.filesystem.readFile("/external.txt", { encoding: "utf8" }), "external\n");
    await workspace.destroy();
    workspace = undefined;
    driver = undefined;

    readOnlyDriver = createNodeFsDriver(root, { readOnly: true });
    const readOnlyFilesystem = createMastraFilesystem(readOnlyDriver, {
      id: "node-fs-read-only",
      readOnly: false,
      closeOnDestroy: true,
    });
    readOnlyWorkspace = new Workspace({ filesystem: readOnlyFilesystem });
    await readOnlyWorkspace.init();
    assert.equal(readOnlyDriver.capabilities.readOnly, true);
    assert.equal(readOnlyFilesystem.readOnly, true);
    await assert.rejects(
      readOnlyWorkspace.filesystem.writeFile("/must-not-write", "blocked"),
      (error) => error instanceof WorkspaceReadOnlyError,
    );
  } finally {
    await readOnlyWorkspace?.destroy();
    await readOnlyDriver?.shutdown();
    await workspace?.destroy();
    await driver?.shutdown();
    await rm(root, { recursive: true, force: true });
  }
}

await testSqlitePersistence();
await testNativeNodeVisibilityAndReadonlyPolicy();

console.log("virtual-fs Mastra integration: PASS");
