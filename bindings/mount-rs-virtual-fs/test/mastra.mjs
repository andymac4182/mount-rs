import assert from "node:assert/strict";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { createNodeFsDriver, Filesystem, fsError } from "@mount-rs/core";
import {
  DirectoryNotFoundError,
  DirectoryNotEmptyError,
  FileExistsError,
  FileNotFoundError,
  StaleFileError,
  Workspace,
  WorkspaceReadOnlyError,
} from "@mastra/core/workspace";
import { createMastraFilesystem } from "@mount-rs/virtual-fs/mastra";

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
    await firstFilesystem.symlink("message.txt", "/persist/link");
    await firstWorkspace.filesystem.moveFile("/persist/link", "/persist/moved-link", {
      overwrite: false,
    });
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
    assert.equal(await secondFilesystem.readlink("/persist/moved-link"), "message.txt");
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
    await filesystem.symlink("from-adapter.bin", "/from-adapter-link");
    await workspace.filesystem.moveFile("/from-adapter-link", "/from-adapter-moved-link", {
      overwrite: false,
    });
    assert.equal(await filesystem.readlink("/from-adapter-moved-link"), "from-adapter.bin");
    await driver.writeFile("/mode-source", Buffer.from("mode"));
    await driver.chmod("/mode-source", 0o666);
    await workspace.filesystem.moveFile("/mode-source", "/mode-destination", {
      overwrite: false,
    });
    assert.equal((await driver.stat("/mode-destination")).mode & 0o777, 0o666);
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

async function testDestroyWaitsForAdmittedConditionalWrite() {
  const driver = Filesystem.memory();
  let enterStat;
  let releaseStat;
  const statEntered = new Promise((resolve) => { enterStat = resolve; });
  const statReleased = new Promise((resolve) => { releaseStat = resolve; });
  let gateStat = false;
  const backend = new Proxy(driver, {
    get(target, property) {
      if (property === "stat") {
        return async (...args) => {
          if (gateStat && args[0] === "/lease.txt") {
            enterStat();
            await statReleased;
          }
          return target.stat(...args);
        };
      }
      const value = Reflect.get(target, property, target);
      return typeof value === "function" ? value.bind(target) : value;
    },
  });
  const filesystem = createMastraFilesystem(backend, { id: "conditional-write-lease" });
  const workspace = new Workspace({ filesystem });
  let write;
  let destroy;
  try {
    await workspace.init();
    await workspace.filesystem.writeFile("/lease.txt", "before");
    const expectedMtime = new Date((await driver.stat("/lease.txt")).mtimeMs);
    gateStat = true;
    write = workspace.filesystem.writeFile("/lease.txt", "after", { expectedMtime });
    await statEntered;
    destroy = workspace.destroy();
    assert.equal(
      await Promise.race([
        destroy.then(() => "destroyed"),
        new Promise((resolve) => setTimeout(() => resolve("pending"), 25)),
      ]),
      "pending",
      "destroy must wait for the write admitted before it began",
    );
    releaseStat();
    await write;
    await destroy;
    assert.equal(Buffer.from(await driver.readFile("/lease.txt")).toString(), "after");
  } finally {
    releaseStat();
    await Promise.allSettled([write, destroy]);
    await workspace.destroy();
    await driver.shutdown();
  }
}

async function testNonrecursiveWriteDoesNotRecreateRemovedParent() {
  const driver = Filesystem.memory();
  let removedParent = false;
  const backend = new Proxy(driver, {
    get(target, property) {
      if (property === "lstat") {
        return async (...args) => {
          const stats = await target.lstat(...args);
          if (args[0] === "/parent" && !removedParent) {
            removedParent = true;
            await target.rmdir("/parent");
          }
          return stats;
        };
      }
      const value = Reflect.get(target, property, target);
      return typeof value === "function" ? value.bind(target) : value;
    },
  });
  const filesystem = createMastraFilesystem(backend, { id: "nonrecursive-write-parent" });
  const workspace = new Workspace({ filesystem });
  try {
    await workspace.init();
    await driver.mkdir("/parent");
    await assert.rejects(
      workspace.filesystem.writeFile("/parent/file", "content", { recursive: false }),
      (error) => error.code === "ENOENT",
    );
    assert.equal(removedParent, true);
    await assert.rejects(driver.lstat("/parent"), (error) => error.code === "ENOENT");
  } finally {
    await workspace.destroy();
    await driver.shutdown();
  }
}

async function testNormalizedPathsAndConcurrentNoClobberWrites() {
  const driver = Filesystem.memory();
  const filesystem = createMastraFilesystem(driver, { id: "path-and-no-clobber" });
  const workspace = new Workspace({ filesystem });
  try {
    await workspace.init();
    await workspace.filesystem.mkdir("/folder");
    await workspace.filesystem.writeFile("folder/../target.txt", "logical");
    assert.equal(
      await workspace.filesystem.readFile("/target.txt", { encoding: "utf8" }),
      "logical",
    );
    await assert.rejects(
      workspace.filesystem.writeFile("/missing/child.txt", "blocked", { recursive: false }),
      (error) => error instanceof DirectoryNotFoundError && error.path === "/missing",
    );
    assert.equal(await workspace.filesystem.exists("/missing"), false);

    const results = await Promise.allSettled([
      workspace.filesystem.writeFile("/winner.txt", "first", { overwrite: false }),
      workspace.filesystem.writeFile("winner.txt", "second", { overwrite: false }),
    ]);
    assert.equal(results.filter((result) => result.status === "fulfilled").length, 1);
    const loser = results.find((result) => result.status === "rejected");
    assert.equal(loser.reason instanceof FileExistsError, true);
    assert.equal(loser.reason.path, "/winner.txt");
    const winner = results[0].status === "fulfilled" ? "first" : "second";
    assert.equal(
      await workspace.filesystem.readFile("/winner.txt", { encoding: "utf8" }),
      winner,
    );
    await driver.chmod("/target.txt", 0o440);
    await workspace.filesystem.moveFile("/target.txt", "/moved.txt", { overwrite: false });
    assert.equal((await driver.stat("/moved.txt")).mode & 0o777, 0o440);
    assert.equal(await workspace.filesystem.exists("/target.txt"), false);
    assert.equal(await workspace.filesystem.readFile("/moved.txt", { encoding: "utf8" }), "logical");
  } finally {
    await workspace.destroy();
    await driver.shutdown();
  }
}

async function testMoveNoClobberWhenDestinationAppearsDuringCheck() {
  const driver = Filesystem.memory();
  let enterCheck;
  let releaseCheck;
  const checkEntered = new Promise((resolve) => { enterCheck = resolve; });
  const checkReleased = new Promise((resolve) => { releaseCheck = resolve; });
  const backend = new Proxy(driver, {
    get(target, property) {
      if (property === "link") {
        return async (...args) => { throw fsError("ENOTSUP", { syscall: "link", path: args[1] }); };
      }
      if (property === "lstat") {
        return async (...args) => {
          try {
            return await target.lstat(...args);
          } catch (error) {
            if (args[0] === "/destination.txt" && error.code === "ENOENT") {
              enterCheck();
              await checkReleased;
            }
            throw error;
          }
        };
      }
      const value = Reflect.get(target, property, target);
      return typeof value === "function" ? value.bind(target) : value;
    },
  });
  const filesystem = createMastraFilesystem(backend, { id: "move-no-clobber" });
  const workspace = new Workspace({ filesystem });
  let move;
  try {
    await workspace.init();
    await driver.writeFile("/source.txt", Buffer.from("source"));
    move = workspace.filesystem.moveFile("/source.txt", "/destination.txt", {
      overwrite: false,
    });
    await checkEntered;
    await driver.writeFile("/destination.txt", Buffer.from("outside"));
    releaseCheck();
    await assert.rejects(
      move,
      (error) => error instanceof FileExistsError && error.path === "/destination.txt",
    );
    assert.equal(Buffer.from(await driver.readFile("/source.txt")).toString(), "source");
    assert.equal(Buffer.from(await driver.readFile("/destination.txt")).toString(), "outside");
  } finally {
    releaseCheck();
    await Promise.allSettled([move]);
    await workspace.destroy();
    await driver.shutdown();
  }
}

async function testMoveNoClobberCleansFailedCopy() {
  const driver = Filesystem.memory();
  const backend = new Proxy(driver, {
    get(target, property) {
      if (property === "link") {
        return async (...args) => { throw fsError("ENOTSUP", { syscall: "link", path: args[1] }); };
      }
      if (property === "open") {
        return async (...args) => {
          const handle = await target.open(...args);
          if (args[0] !== "/partial.txt") return handle;
          return {
            write: async () => ({ bytesWritten: 0 }),
            stat: () => handle.stat(),
            close: () => handle.close(),
          };
        };
      }
      const value = Reflect.get(target, property, target);
      return typeof value === "function" ? value.bind(target) : value;
    },
  });
  const filesystem = createMastraFilesystem(backend, { id: "move-failed-copy" });
  const workspace = new Workspace({ filesystem });
  try {
    await workspace.init();
    await driver.writeFile("/source.txt", Buffer.from("source"));
    await assert.rejects(
      workspace.filesystem.moveFile("/source.txt", "/partial.txt", { overwrite: false }),
      (error) => error.code === "EIO",
    );
    assert.equal(Buffer.from(await driver.readFile("/source.txt")).toString(), "source");
    assert.equal(Buffer.from(await driver.readFile("/partial.txt")).toString(), "",
      "an incomplete owned copy may remain when safe conditional cleanup is unavailable");
  } finally {
    await workspace.destroy();
    await driver.shutdown();
  }
}

async function testMoveNoClobberDirectoryAndSymlink() {
  const driver = Filesystem.memory();
  const filesystem = createMastraFilesystem(driver, { id: "move-directory-and-link" });
  const workspace = new Workspace({ filesystem });
  try {
    await workspace.init();
    await workspace.filesystem.mkdir("/tree");
    await workspace.filesystem.writeFile("/tree/child.txt", "child");
    await assert.rejects(
      workspace.filesystem.moveFile("/tree", "/moved-tree", { overwrite: false }),
      (error) => error.code === "ENOTSUP",
    );
    assert.equal(await workspace.filesystem.exists("/moved-tree"), false);
    assert.equal(
      await workspace.filesystem.readFile("/tree/child.txt", { encoding: "utf8" }),
      "child",
    );
    await filesystem.symlink("/tree/child.txt", "/link");
    await workspace.filesystem.moveFile("/link", "/moved-link", { overwrite: false });
    assert.equal(await filesystem.readlink("/moved-link"), "/tree/child.txt");
    await assert.rejects(driver.lstat("/link"), (error) => error.code === "ENOENT");
    await workspace.filesystem.mkdir("/occupied");
    await assert.rejects(
      workspace.filesystem.moveFile("/tree", "/occupied", { overwrite: false }),
      (error) => error.code === "ENOTSUP",
    );
    assert.equal(await workspace.filesystem.exists("/tree/child.txt"), true);
  } finally {
    await workspace.destroy();
    await driver.shutdown();
  }
}

async function testDirectoryMoveFailurePreservesOtherBackendData() {
  const driver = Filesystem.memory();
  let injected = false;
  const backend = new Proxy(driver, {
    get(target, property) {
      if (property === "readFile") {
        return async (...args) => {
          if (args[0] === "/source/child.txt") {
            injected = true;
            await target.writeFile("/destination/outsider.txt", Buffer.from("outside"));
            throw fsError("EIO", { syscall: "read", path: args[0] });
          }
          return target.readFile(...args);
        };
      }
      const value = Reflect.get(target, property, target);
      return typeof value === "function" ? value.bind(target) : value;
    },
  });
  const filesystem = createMastraFilesystem(backend, { id: "foreign-child-on-copy-failure" });
  const workspace = new Workspace({ filesystem });
  try {
    await workspace.init();
    await driver.mkdir("/source");
    await driver.writeFile("/source/child.txt", Buffer.from("source"));
    await assert.rejects(
      workspace.filesystem.moveFile("/source", "/destination", { overwrite: false }),
      (error) => error.code === "EIO" || error.code === "ENOTSUP",
    );
    assert.equal(Buffer.from(await driver.readFile("/source/child.txt")).toString(), "source");
    if (injected) {
      assert.equal(Buffer.from(await driver.readFile("/destination/outsider.txt")).toString(), "outside");
    } else {
      await assert.rejects(driver.lstat("/destination"), (error) => error.code === "ENOENT");
    }
  } finally {
    await workspace.destroy();
    await driver.shutdown();
  }
}

async function testSymlinkMoveDoesNotAcceptForeignDestination() {
  const driver = Filesystem.memory();
  let injected = false;
  const backend = new Proxy(driver, {
    get(target, property) {
      if (property === "symlink") {
        return async (...args) => {
          await target.symlink(...args);
          if (args[1] === "/destination") {
            injected = true;
            await target.writeFile("/outsider", Buffer.from("outside"));
            await target.rename("/outsider", "/destination");
          }
        };
      }
      if (property === "link") {
        return async (...args) => {
          await target.link(...args);
          if (args[1] === "/destination") {
            injected = true;
            await target.writeFile("/outsider", Buffer.from("outside"));
            await target.rename("/outsider", "/destination");
          }
        };
      }
      const value = Reflect.get(target, property, target);
      return typeof value === "function" ? value.bind(target) : value;
    },
  });
  const filesystem = createMastraFilesystem(backend, { id: "symlink-foreign-destination" });
  const workspace = new Workspace({ filesystem });
  try {
    await workspace.init();
    await driver.writeFile("/target", Buffer.from("target"));
    await driver.symlink("/target", "/source-link");
    const result = await Promise.allSettled([
      workspace.filesystem.moveFile("/source-link", "/destination", { overwrite: false }),
    ]);
    assert.equal(injected, true);
    assert.equal(result[0].status, "rejected", "a foreign destination must not complete the move");
    assert.equal(await driver.readlink("/source-link"), "/target");
    assert.equal(Buffer.from(await driver.readFile("/destination")).toString(), "outside");
  } finally {
    await workspace.destroy();
    await driver.shutdown();
  }
}

async function testSymlinkMoveFailsClosedWithoutHardlinks() {
  const driver = Filesystem.memory();
  const backend = new Proxy(driver, {
    get(target, property) {
      if (property === "link") {
        return async (...args) => {
          throw fsError("ENOTSUP", { syscall: "link", path: args[1] });
        };
      }
      const value = Reflect.get(target, property, target);
      return typeof value === "function" ? value.bind(target) : value;
    },
  });
  const filesystem = createMastraFilesystem(backend, { id: "symlink-no-hardlinks" });
  const workspace = new Workspace({ filesystem });
  try {
    await workspace.init();
    await driver.symlink("/target", "/source-link");
    await assert.rejects(
      workspace.filesystem.moveFile("/source-link", "/destination", { overwrite: false }),
      (error) => error.code === "ENOTSUP",
    );
    assert.equal(await driver.readlink("/source-link"), "/target");
    await assert.rejects(driver.lstat("/destination"), (error) => error.code === "ENOENT");
  } finally {
    await workspace.destroy();
    await driver.shutdown();
  }
}

async function testExclusiveCopyDoesNotChangeForeignFileMode() {
  const driver = Filesystem.memory();
  const backend = new Proxy(driver, {
    get(target, property) {
      if (property === "link") {
        return async (...args) => { throw fsError("ENOTSUP", { syscall: "link", path: args[1] }); };
      }
      if (property === "open") {
        return async (...args) => {
          const handle = await target.open(...args);
          if (args[0] !== "/destination") return handle;
          return {
            write: (...writeArgs) => handle.write(...writeArgs),
            stat: () => handle.stat(),
            close: async () => {
              await handle.close();
              await target.writeFile("/outsider", Buffer.from("outside"));
              await target.chmod("/outsider", 0o600);
              await target.rename("/outsider", "/destination");
            },
          };
        };
      }
      const value = Reflect.get(target, property, target);
      return typeof value === "function" ? value.bind(target) : value;
    },
  });
  const filesystem = createMastraFilesystem(backend, { id: "foreign-file-mode" });
  const workspace = new Workspace({ filesystem });
  try {
    await workspace.init();
    await driver.writeFile("/source", Buffer.from("source"));
    await driver.chmod("/source", 0o444);
    await assert.rejects(
      workspace.filesystem.moveFile("/source", "/destination", { overwrite: false }),
      (error) => error instanceof FileExistsError && error.path === "/destination",
    );
    assert.equal(Buffer.from(await driver.readFile("/source")).toString(), "source");
    assert.equal(Buffer.from(await driver.readFile("/destination")).toString(), "outside");
    assert.equal((await driver.stat("/destination")).mode & 0o777, 0o600);
  } finally {
    await workspace.destroy();
    await driver.shutdown();
  }
}

async function testMoveNoClobberRetainsCopyWhenSourceRemovalFails() {
  const driver = Filesystem.memory();
  const backend = new Proxy(driver, {
    get(target, property) {
      if (property === "unlink") {
        return async (...args) => {
          if (args[0] === "/source.txt") {
            throw fsError("EIO", { syscall: "unlink", path: args[0] });
          }
          return target.unlink(...args);
        };
      }
      const value = Reflect.get(target, property, target);
      return typeof value === "function" ? value.bind(target) : value;
    },
  });
  const filesystem = createMastraFilesystem(backend, { id: "move-removal-failure" });
  const workspace = new Workspace({ filesystem });
  try {
    await workspace.init();
    await driver.writeFile("/source.txt", Buffer.from("source"));
    await assert.rejects(
      workspace.filesystem.moveFile("/source.txt", "/copied.txt", { overwrite: false }),
      (error) => error.code === "EIO",
    );
    assert.equal(Buffer.from(await driver.readFile("/source.txt")).toString(), "source");
    assert.equal(Buffer.from(await driver.readFile("/copied.txt")).toString(), "source");
  } finally {
    await workspace.destroy();
    await driver.shutdown();
  }
}

async function testMovePreservesReplacedSource() {
  for (const sourceKind of ["file", "symlink"]) {
    const driver = Filesystem.memory();
    let replaced = false;
    const backend = new Proxy(driver, {
      get(target, property) {
        if (property === "link" && sourceKind === "file") {
          return async (...args) => { throw fsError("ENOTSUP", { syscall: "link", path: args[1] }); };
        }
        if (property === "lstat") {
          return async (...args) => {
            if (args[0] === "/source" && !replaced) {
              try {
                await target.lstat("/destination");
                replaced = true;
                await target.rename("/source", "/original-source");
                if (sourceKind === "file") {
                  await target.mkdir("/source");
                  await target.writeFile("/source/outsider.txt", Buffer.from("outside"));
                } else {
                  await target.writeFile("/source", Buffer.from("outside"));
                }
              } catch (error) {
                if (error.code !== "ENOENT") throw error;
              }
            }
            return target.lstat(...args);
          };
        }
        const value = Reflect.get(target, property, target);
        return typeof value === "function" ? value.bind(target) : value;
      },
    });
    const filesystem = createMastraFilesystem(backend, { id: `replaced-source-${sourceKind}` });
    const workspace = new Workspace({ filesystem });
    try {
      await workspace.init();
      if (sourceKind === "file") {
        await driver.writeFile("/source", Buffer.from("original"));
      } else {
        await driver.symlink("/target", "/source");
      }
      await assert.rejects(
        workspace.filesystem.moveFile("/source", "/destination", { overwrite: false }),
        (error) => error.code === "EAGAIN",
        sourceKind,
      );
      assert.equal(replaced, true);
      if (sourceKind === "file") {
        assert.equal(Buffer.from(await driver.readFile("/source/outsider.txt")).toString(), "outside");
        assert.equal(Buffer.from(await driver.readFile("/original-source")).toString(), "original");
      } else {
        assert.equal(Buffer.from(await driver.readFile("/source")).toString(), "outside");
        assert.equal(await driver.readlink("/original-source"), "/target");
      }
    } finally {
      await workspace.destroy();
      await driver.shutdown();
    }
  }
}

async function testDeleteAndRmdirPreserveReplacedEntries() {
  for (const originalKind of ["file", "directory"]) {
    const driver = Filesystem.memory();
    let checks = 0;
    const backend = new Proxy(driver, {
      get(target, property) {
        if (property === "lstat") {
          return async (...args) => {
            if (args[0] === "/target" && ++checks === 2) {
              await target.rename("/target", "/original-target");
              if (originalKind === "file") {
                await target.mkdir("/target");
              } else {
                await target.writeFile("/target", Buffer.from("outside"));
              }
            }
            return target.lstat(...args);
          };
        }
        const value = Reflect.get(target, property, target);
        return typeof value === "function" ? value.bind(target) : value;
      },
    });
    const filesystem = createMastraFilesystem(backend, { id: `replaced-delete-${originalKind}` });
    const workspace = new Workspace({ filesystem });
    try {
      await workspace.init();
      if (originalKind === "file") {
        await driver.writeFile("/target", Buffer.from("original"));
        await assert.rejects(workspace.filesystem.deleteFile("/target"),
          (error) => error.code === "EAGAIN");
        assert.equal((await driver.lstat("/target")).isDirectory(), true);
        assert.equal(Buffer.from(await driver.readFile("/original-target")).toString(), "original");
      } else {
        await driver.mkdir("/target");
        await assert.rejects(workspace.filesystem.rmdir("/target"),
          (error) => error.code === "EAGAIN");
        assert.equal(Buffer.from(await driver.readFile("/target")).toString(), "outside");
        assert.equal((await driver.lstat("/original-target")).isDirectory(), true);
      }
      assert.equal(checks, 2);
    } finally {
      await workspace.destroy();
      await driver.shutdown();
    }
  }
}

async function testFailedCopyDoesNotUnlinkForeignReplacement() {
  const driver = Filesystem.memory();
  let reachedCleanupUnlink = false;
  let checkedDestination = false;
  const backend = new Proxy(driver, {
    get(target, property) {
      if (property === "link") {
        return async (...args) => { throw fsError("ENOTSUP", { syscall: "link", path: args[1] }); };
      }
      if (property === "open") {
        return async (...args) => {
          const handle = await target.open(...args);
          if (args[0] !== "/destination") return handle;
          return { write: async () => ({ bytesWritten: 0 }), stat: () => handle.stat(), close: () => handle.close() };
        };
      }
      if (property === "lstat") {
        return async (...args) => {
          const result = await target.lstat(...args);
          if (args[0] === "/destination") checkedDestination = true;
          return result;
        };
      }
      if (property === "unlink") {
        return async (...args) => {
          if (args[0] === "/destination") {
            reachedCleanupUnlink = true;
            await target.writeFile("/outsider", Buffer.from("outside"));
            await target.rename("/outsider", "/destination");
          }
          return target.unlink(...args);
        };
      }
      const value = Reflect.get(target, property, target);
      return typeof value === "function" ? value.bind(target) : value;
    },
  });
  const filesystem = createMastraFilesystem(backend, { id: "failed-copy-cleanup-race" });
  const workspace = new Workspace({ filesystem });
  try {
    await workspace.init();
    await driver.writeFile("/source", Buffer.from("original"));
    await assert.rejects(
      workspace.filesystem.moveFile("/source", "/destination", { overwrite: false }),
      (error) => error.code === "EIO",
    );
    assert.equal(checkedDestination, false,
      "copy failure leaves the partial entry without a path-based cleanup check");
    assert.equal(reachedCleanupUnlink, false,
      "path-based cleanup cannot safely unlink a destination after copy failure");
    assert.equal(Buffer.from(await driver.readFile("/destination")).toString(), "");
    assert.equal(Buffer.from(await driver.readFile("/source")).toString(), "original");
  } finally {
    await workspace.destroy();
    await driver.shutdown();
  }
}

async function testDestroyWaitsForAdmittedOverwriteMove() {
  const driver = Filesystem.memory();
  let enterMkdir;
  let releaseMkdir;
  const mkdirEntered = new Promise((resolve) => { enterMkdir = resolve; });
  const mkdirReleased = new Promise((resolve) => { releaseMkdir = resolve; });
  const backend = new Proxy(driver, {
    get(target, property) {
      if (property === "mkdir") {
        return async (...args) => {
          if (args[0] === "/nested") {
            enterMkdir();
            await mkdirReleased;
          }
          return target.mkdir(...args);
        };
      }
      const value = Reflect.get(target, property, target);
      return typeof value === "function" ? value.bind(target) : value;
    },
  });
  const filesystem = createMastraFilesystem(backend, { id: "overwrite-move-lease" });
  const workspace = new Workspace({ filesystem });
  let move;
  let destroy;
  try {
    await workspace.init();
    await driver.writeFile("/source", Buffer.from("source"));
    move = workspace.filesystem.moveFile("/source", "/nested/destination");
    await mkdirEntered;
    destroy = workspace.destroy();
    assert.equal(await Promise.race([
      destroy.then(() => "destroyed"),
      new Promise((resolve) => setTimeout(() => resolve("pending"), 25)),
    ]), "pending", "destroy must wait for the admitted move and its parent creation");
    releaseMkdir();
    await move;
    await destroy;
    assert.equal(Buffer.from(await driver.readFile("/nested/destination")).toString(), "source");
    await assert.rejects(driver.lstat("/source"), (error) => error.code === "ENOENT");
  } finally {
    releaseMkdir();
    await Promise.allSettled([move, destroy]);
    await workspace.destroy();
    await driver.shutdown();
  }
}

async function testOverwriteMovePathIndexTracksBackendResult() {
  const driver = Filesystem.memory();
  let failSourceReadback = false;
  const backend = new Proxy(driver, {
    get(target, property) {
      if (property === "rename") {
        return async (...args) => {
          await target.rename(...args);
          if (args[0] === "/source" && args[1] === "/destination") failSourceReadback = true;
        };
      }
      if (property === "lstat") {
        return async (...args) => {
          if (args[0] === "/source" && failSourceReadback) {
            throw fsError("EIO", { syscall: "lstat", path: args[0] });
          }
          return target.lstat(...args);
        };
      }
      const value = Reflect.get(target, property, target);
      return typeof value === "function" ? value.bind(target) : value;
    },
  });
  const filesystem = createMastraFilesystem(backend, { id: "overwrite-move-index" });
  const workspace = new Workspace({ filesystem });
  try {
    await workspace.init();
    await workspace.filesystem.writeFile("/source", "same inode");
    await driver.link("/source", "/destination");
    await filesystem.refreshPaths();
    await workspace.filesystem.moveFile("/source", "/destination");
    assert.equal(Buffer.from(await driver.readFile("/source")).toString(), "same inode");
    assert.equal(filesystem.getAllPaths().includes("/source"), true,
      "the index must retain a source still present after backend rename");

    await workspace.filesystem.mkdir("/old");
    await workspace.filesystem.writeFile("/old/stale", "removed out of band");
    await driver.unlink("/old/stale");
    await workspace.filesystem.mkdir("/source-dir");
    await workspace.filesystem.writeFile("/source-dir/current", "current");
    await workspace.filesystem.moveFile("/source-dir", "/old");
    assert.equal(filesystem.getAllPaths().includes("/old/stale"), false);
    assert.equal(filesystem.getAllPaths().includes("/old/current"), true);
  } finally {
    await workspace.destroy();
    await driver.shutdown();
  }
}

async function testMoveNoClobberPreservesSourceIfDestinationIsReplaced() {
  for (const replacementPoint of ["before-identity-check", "during-write"]) {
    const driver = Filesystem.memory();
    const replaceDestination = async () => {
      await driver.writeFile("/replacement.txt", Buffer.from("outside"));
      await driver.rename("/replacement.txt", "/destination.txt");
    };
    const backend = new Proxy(driver, {
      get(target, property) {
        if (property === "link") {
          return async (...args) => { throw fsError("ENOTSUP", { syscall: "link", path: args[1] }); };
        }
        if (property === "open") {
          return async (...args) => {
            const handle = await target.open(...args);
            if (args[0] !== "/destination.txt") return handle;
            if (replacementPoint === "before-identity-check") await replaceDestination();
            return {
              write: async (...writeArgs) => {
                const result = await handle.write(...writeArgs);
                if (replacementPoint === "during-write") await replaceDestination();
                return result;
              },
              stat: () => handle.stat(),
              close: () => handle.close(),
            };
          };
        }
        const value = Reflect.get(target, property, target);
        return typeof value === "function" ? value.bind(target) : value;
      },
    });
    const filesystem = createMastraFilesystem(backend, { id: `move-replaced-${replacementPoint}` });
    const workspace = new Workspace({ filesystem });
    try {
      await workspace.init();
      await driver.writeFile("/source.txt", Buffer.from("source"));
      await assert.rejects(
        workspace.filesystem.moveFile("/source.txt", "/destination.txt", { overwrite: false }),
        (error) => error instanceof FileExistsError && error.path === "/destination.txt",
        replacementPoint,
      );
      assert.equal(Buffer.from(await driver.readFile("/source.txt")).toString(), "source");
      assert.equal(Buffer.from(await driver.readFile("/destination.txt")).toString(), "outside");
    } finally {
      await workspace.destroy();
      await driver.shutdown();
    }
  }
}

await testSqlitePersistence();
await testNativeNodeVisibilityAndReadonlyPolicy();
await testDestroyWaitsForAdmittedConditionalWrite();
await testNonrecursiveWriteDoesNotRecreateRemovedParent();
await testNormalizedPathsAndConcurrentNoClobberWrites();
await testMoveNoClobberWhenDestinationAppearsDuringCheck();
await testMoveNoClobberCleansFailedCopy();
await testMoveNoClobberDirectoryAndSymlink();
await testDirectoryMoveFailurePreservesOtherBackendData();
await testSymlinkMoveDoesNotAcceptForeignDestination();
await testSymlinkMoveFailsClosedWithoutHardlinks();
await testExclusiveCopyDoesNotChangeForeignFileMode();
await testMoveNoClobberRetainsCopyWhenSourceRemovalFails();
await testMovePreservesReplacedSource();
await testDeleteAndRmdirPreserveReplacedEntries();
await testFailedCopyDoesNotUnlinkForeignReplacement();
await testDestroyWaitsForAdmittedOverwriteMove();
await testOverwriteMovePathIndexTracksBackendResult();
await testMoveNoClobberPreservesSourceIfDestinationIsReplaced();

console.log("virtual-fs Mastra integration: PASS");
