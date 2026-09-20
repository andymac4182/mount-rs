import assert from "node:assert/strict";
import { constants } from "node:fs";
import { pathToFileURL } from "node:url";
import { Filesystem } from "../index.js";

const text = (value) => Buffer.from(value).toString();

export async function exercise(fs) {
  const capabilities = fs.capabilities;
  assert.equal(typeof capabilities, "object");
  // `mknod` is the binding's legacy convenience boolean; the oracle models
  // the same extension only through its typed extensions list.
  if ("mknod" in capabilities) assert.equal(capabilities.mknod, true);
  assert.deepEqual([...capabilities.extensions], ["mknod"]);
  if (typeof fs.getCapabilities === "function") {
    assert.deepEqual(fs.getCapabilities(), capabilities);
  }

  const recursiveMkdirResult = await fs.mkdir("/tree/sub", {
    recursive: true,
    mode: 0o755,
  });
  const existingRecursiveMkdirResult = await fs.mkdir("/tree/sub", {
    recursive: true,
  });
  assert.equal(recursiveMkdirResult, "/tree");
  assert.equal(existingRecursiveMkdirResult, undefined);
  await fs.writeFile("/tree/data", Buffer.from("0123456789"));

  const handle = await fs.open("/tree/data", "r+");
  const positioned = await handle.read(Buffer.alloc(4), 0, 4, 3);
  assert.equal(positioned.bytesRead, 4);
  assert.equal(text(positioned.buffer), "3456");
  const written = await handle.write(Buffer.from("ab"), 0, 2, 1);
  assert.equal(written.bytesWritten, 2);
  await handle.close();
  assert.equal(text(await fs.readFile("/tree/data")), "0ab3456789");

  const cursor = await fs.open("/tree/data", "r");
  assert.equal(text((await cursor.read(Buffer.alloc(3))).buffer), "0ab");
  assert.equal(text((await cursor.read(Buffer.alloc(3))).buffer), "345");
  await cursor.close();

  const append = await fs.open("/tree/data", "a");
  await append.write(Buffer.from("-tail"), 0, 5, null);
  await append.close();
  assert.equal(text(await fs.readFile("/tree/data")), "0ab3456789-tail");

  const numeric = await fs.open(
    "/tree/numeric",
    constants.O_RDWR | constants.O_CREAT,
    0o640,
  );
  const numericWrite = await numeric.write(Buffer.from("x"), 0, 1, 0);
  assert.equal(numericWrite.bytesWritten, 1);
  await numeric.close();
  assert.equal(text(await fs.readFile("/tree/numeric")), "x");
  assert.equal((await fs.stat("/tree/numeric")).mode & 0o777, 0o640);

  const readonly = await fs.open("/tree/numeric", constants.O_RDONLY);
  await assert.rejects(readonly.write(Buffer.from("!")), (error) => {
    assert.equal(error.code, "EBADF");
    assert.equal(error.errno, -9);
    assert.equal(error.syscall, "write");
    return true;
  });
  await readonly.close();

  await assert.rejects(fs.open("/tree/missing", "r"), (error) => {
    assert.equal(error.code, "ENOENT");
    assert.equal(error.errno, -2);
    assert.equal(error.syscall, "open");
    assert.equal(error.path, "/tree/missing");
    return true;
  });

  const range = await fs.open("/tree/data", "r");
  await assert.rejects(range.read(Buffer.alloc(2), 0, 3, 0), (error) => {
    assert.equal(error.name, "RangeError");
    assert.equal(error.code, "ERR_OUT_OF_RANGE");
    return true;
  });
  await range.close();

  const stats = await fs.stat("/tree/data");
  assert.equal(stats.isFile(), true);
  assert.equal(stats.isDirectory(), false);
  const entries = await fs.readdir("/tree", { withFileTypes: true });
  const names = entries.map((entry) => entry.name).sort();
  assert.deepEqual(names, ["data", "numeric", "sub"]);
  assert.equal(entries.find((entry) => entry.name === "sub").isDirectory(), true);

  await fs.chown("/tree/data", 123, 456);
  await fs.chown("/tree/data", -1, 789);
  const owner = await fs.stat("/tree/data");
  assert.equal(owner.uid, 123);
  assert.equal(owner.gid, 789);

  await fs.utimes("/tree/data", new Date(1_700_000_000_000), 1_700_000_001);
  assert.equal((await fs.stat("/tree/data")).mtimeMs, 1_700_000_001_000);

  await fs.truncate("/tree/data", 3);
  assert.equal(text(await fs.readFile("/tree/data")), "0ab");
  await fs.truncate("/tree/data", 5);
  assert.deepEqual([...await fs.readFile("/tree/data")], [48, 97, 98, 0, 0]);

  const mknod = typeof fs.mknod === "function"
    ? fs.mknod.bind(fs)
    : fs.mountx?.mknod?.bind(fs.mountx);
  if (mknod) {
    await mknod("/tree/fifo", 0o010000 | 0o644, 0);
    const fifo = await fs.lstat("/tree/fifo");
    assert.equal(fifo.isFIFO(), true);
    assert.equal(fifo.mode & 0o170000, 0o010000);
  }
  if (fs.mountx?.mknod) {
    await fs.mountx.mknod("/tree/socket", 0o140000 | 0o600, 0);
    assert.equal((await fs.lstat("/tree/socket")).isSocket(), true);
  }

  return {
    data: text(await fs.readFile("/tree/data")),
    recursiveMkdirResult,
    existingRecursiveMkdirResult,
    entries: (await fs.readdir("/tree", { withFileTypes: true })).map((entry) => [
      entry.name,
      entry.isDirectory(),
      entry.isFIFO(),
    ]).sort(),
    numericMode: (await fs.stat("/tree/numeric")).mode & 0o777,
    owner: [owner.uid, owner.gid],
    statfs: await fs.statfs("/tree"),
  };
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  await exercise(Filesystem.memory());
  console.log("mount-rs N-API handle contract: PASS");
}
