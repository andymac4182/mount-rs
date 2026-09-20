import { pathToFileURL } from "node:url";

const source = process.env.MOUNTX_SOURCE;
if (!source) throw new Error("MOUNTX_SOURCE must point to a checkout of pithings/mountx");
const [{ createMemoryDriver }, { createLoopback }, types] = await Promise.all([
  import(pathToFileURL(`${source}/src/drivers/memory.ts`).href),
  import(pathToFileURL(`${source}/src/harness.ts`).href),
  import(pathToFileURL(`${source}/src/types.ts`).href),
]);

const fs = createLoopback(createMemoryDriver());
const errorCode = async (promise) => {
  try {
    await promise;
    return null;
  } catch (error) {
    return error.code ?? "unknown";
  }
};

await fs.writeFile("/flags", "abcdef");
const append = await fs.open("/flags", "a");
await append.write(new TextEncoder().encode("-g"), 0, 2);
await append.close();
await fs.truncate("/flags", 3);
await fs.truncate("/flags", 5);
await fs.chmod("/flags", 0o640);
await fs.chown("/flags", 123, 456);
await fs.utimes("/flags", new Date(1_700_000_000_000), new Date(1_700_000_001_000));

await fs.symlink("two", "/one");
await fs.symlink("one", "/two");
await fs.mountx.mknod("/fifo", types.S_IFIFO | 0o644, 0);
const flags = await fs.stat("/flags");
const fifo = await fs.stat("/fifo");
const statfs = await fs.statfs("/");

const output = {
  append: [...await fs.readFile("/flags")],
  flags: {
    mode: flags.mode & 0o7777,
    uid: flags.uid,
    gid: flags.gid,
    atime_ms: flags.atimeMs,
    mtime_ms: flags.mtimeMs,
  },
  symlink_loop: await errorCode(fs.stat("/one")),
  fifo: { mode: fifo.mode, is_fifo: fifo.isFIFO(), open_error: await errorCode(fs.open("/fifo", "r")) },
  statfs: {
    filesystem_type: statfs.type,
    block_size: statfs.bsize,
    blocks: statfs.blocks,
    blocks_free: statfs.bfree,
    blocks_available: statfs.bavail,
    files: statfs.files,
    files_free: statfs.ffree,
  },
};
process.stdout.write(`${JSON.stringify(output)}\n`);
