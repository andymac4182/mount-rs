import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { pathToFileURL, fileURLToPath } from "node:url";

const repo = fileURLToPath(new URL("../..", import.meta.url));
const expectedOracleRevision = "85361a8212ff9bff8e69f62fa8993ef2c2ec51e8";
const source = process.env.MOUNTX_SOURCE;

if (!source) {
  console.log(
    "core in-memory parity: SKIP (MOUNTX_SOURCE is unset; the pinned TypeScript oracle is required)",
  );
  process.exit(0);
}

const oracleRevision = execFileSync(
  "git",
  ["-C", source, "rev-parse", "HEAD"],
  { encoding: "utf8" },
).trim();
if (oracleRevision !== expectedOracleRevision) {
  throw new Error(
    `MOUNTX_SOURCE revision ${oracleRevision} is not the pinned oracle ${expectedOracleRevision}`,
  );
}

const [{ createMemoryDriver }, { createLoopback }] = await Promise.all([
  import(pathToFileURL(`${source}/src/drivers/memory.ts`).href),
  import(pathToFileURL(`${source}/src/harness.ts`).href),
]);

const S_IFIFO = 0o010000;
const S_IFCHR = 0o020000;
const binary = [0, 1, 2, 127, 128, 255];

// One serial trace deliberately stays on the memory driver and exercises the
// public loopback contract. Dynamic ctime/birthtime values are represented by
// presence booleans; explicit utimes/lutimes values remain exact.
const trace = [
  { op: "mkdir_recursive", path: "/work/./nested/../nested/deep", mode: 0o755 },
  { op: "write", path: "/work/nested/deep/blob", data: binary },
  { op: "stat", path: "/work/nested/deep/./blob", exact_times: false },
  {
    op: "utimes",
    path: "/work/nested/deep/blob",
    atime_ms: 1_111_000,
    mtime_ms: 2_222_000,
  },
  { op: "chmod", path: "/work/nested/deep/blob", mode: 0o640 },
  { op: "chown", path: "/work/nested/deep/blob", uid: 501, gid: 20 },
  { op: "stat", path: "/work/nested/deep/blob" },
  { op: "link", existing_path: "/work/nested/deep/blob", new_path: "/work/hard" },
  { op: "symlink", target: "nested/deep/blob", path: "/work/current" },
  { op: "lstat", path: "/work/current", exact_times: false },
  {
    op: "lutimes",
    path: "/work/current",
    atime_ms: 3_333_000,
    mtime_ms: 4_444_000,
  },
  { op: "lstat", path: "/work/current" },
  { op: "readlink", path: "/work/current" },
  { op: "stat", path: "/work/current" },
  { op: "list", path: "/work//nested/.." },
  { op: "statfs", path: "/work/./nested/../deep/.." },
  { op: "open", id: "h", path: "/work/hard", flags: "r+", mode: 0 },
  { op: "handle_sync", id: "h" },
  { op: "handle_datasync", id: "h" },
  { op: "handle_read", id: "h", count: 4, position: null },
  { op: "handle_write", id: "h", data: [250, 251, 0], position: null },
  { op: "handle_read", id: "h", count: 32, position: 0 },
  { op: "handle_stat", id: "h", exact_times: false },
  { op: "unlink", path: "/work/hard" },
  { op: "handle_stat", id: "h", exact_times: false },
  { op: "stat", path: "/work/hard" },
  { op: "handle_read", id: "h", count: 32, position: 0 },
  { op: "handle_close", id: "h" },
  { op: "handle_read", id: "h", count: 1, position: 0 },
  { op: "handle_stat", id: "h" },
  { op: "handle_close", id: "h" },
  { op: "rename", old_path: "/work/nested", new_path: "/work/renamed" },
  { op: "read", path: "/work/current" },
  { op: "readlink", path: "/work/current" },
  { op: "read", path: "/work/renamed/./deep/../deep/blob" },
  { op: "mkdir", path: "/work", mode: 0o755 },
  { op: "link", existing_path: "/work/renamed", new_path: "/work/directory-link" },
  { op: "readlink", path: "/work/renamed/deep/blob" },
  { op: "rmdir", path: "/work/renamed/deep" },
  { op: "unlink", path: "/work/renamed" },
  { op: "truncate", path: "/work/renamed/deep", length: 0 },
  { op: "rmdir", path: "/" },
  { op: "stat", path: "/missing" },
  { op: "symlink", target: "", path: "/empty" },
  { op: "rename", old_path: "/missing", new_path: "/work/no" },
  { op: "open", id: "exclusive", path: "/work/renamed/deep/blob", flags: "wx", mode: 0o644 },
  { op: "read", path: "/work/renamed" },
  { op: "write", path: "/work/renamed", data: [7] },
  { op: "rmdir", path: "/work/renamed/deep/blob" },
  {
    op: "mkdir_recursive",
    path: "/work/renamed/deep/blob/child",
    mode: 0o755,
  },
  { op: "mknod", path: "/fifo", mode: S_IFIFO | 0o644, dev: 0 },
  { op: "stat", path: "/fifo", exact_times: false },
  { op: "open", id: "fifo", path: "/fifo", flags: "r", mode: 0 },
  { op: "truncate", path: "/fifo", length: 0 },
  { op: "mknod", path: "/char", mode: S_IFCHR | 0o600, dev: 259 },
  { op: "stat", path: "/char", exact_times: false },
];

function stableStats(stats, exactTimes = true) {
  let kind = "File";
  if (stats.isDirectory()) kind = "Directory";
  else if (stats.isSymbolicLink()) kind = "Symlink";
  else if (stats.isBlockDevice()) kind = "BlockDevice";
  else if (stats.isCharacterDevice()) kind = "CharacterDevice";
  else if (stats.isFIFO()) kind = "Fifo";
  else if (stats.isSocket()) kind = "Socket";
  return {
    dev: stats.dev,
    ino: stats.ino,
    mode: stats.mode,
    nlink: stats.nlink,
    uid: stats.uid,
    gid: stats.gid,
    rdev: stats.rdev,
    size: stats.size,
    blksize: stats.blksize,
    blocks: stats.blocks,
    atime_ms: exactTimes ? stats.atimeMs : null,
    mtime_ms: exactTimes ? stats.mtimeMs : null,
    atime_present: Number.isFinite(stats.atimeMs),
    mtime_present: Number.isFinite(stats.mtimeMs),
    ctime_present: Number.isFinite(stats.ctimeMs),
    birthtime_present: Number.isFinite(stats.birthtimeMs),
    kind,
  };
}

function stableStatfs(stats) {
  return {
    filesystem_type: stats.type,
    block_size: stats.bsize,
    blocks: stats.blocks,
    blocks_free: stats.bfree,
    blocks_available: stats.bavail,
    files: stats.files,
    files_free: stats.ffree,
  };
}

function position(command) {
  return command.position == null ? undefined : command.position;
}

async function execute(fs, handles, command) {
  switch (command.op) {
    case "mkdir_recursive":
      return (await fs.mkdir(command.path, { recursive: true, mode: command.mode })) ?? null;
    case "mkdir":
      await fs.mkdir(command.path, { recursive: false, mode: command.mode });
      return null;
    case "write":
      await fs.writeFile(command.path, Uint8Array.from(command.data));
      return null;
    case "read":
      return Array.from(await fs.readFile(command.path));
    case "stat":
    case "lstat":
      return stableStats(await fs[command.op](command.path), command.exact_times !== false);
    case "statfs":
      return stableStatfs(await fs.statfs(command.path));
    case "list":
      return (await fs.readdir(command.path)).map((entry) => ({
        name: entry.name,
        parent_path: entry.parentPath,
        type: entry.isDirectory()
          ? "Directory"
          : entry.isSymbolicLink()
            ? "Symlink"
            : entry.isBlockDevice()
              ? "BlockDevice"
              : entry.isCharacterDevice()
                ? "CharacterDevice"
                : entry.isFIFO()
                  ? "Fifo"
                  : entry.isSocket()
                    ? "Socket"
                    : "File",
      }));
    case "utimes":
      await fs.utimes(command.path, command.atime_ms / 1000, command.mtime_ms / 1000);
      return null;
    case "lutimes":
      await fs.lutimes(command.path, command.atime_ms / 1000, command.mtime_ms / 1000);
      return null;
    case "chmod":
      await fs.chmod(command.path, command.mode);
      return null;
    case "chown":
      await fs.chown(command.path, command.uid, command.gid);
      return null;
    case "link":
      await fs.link(command.existing_path, command.new_path);
      return null;
    case "symlink":
      await fs.symlink(command.target, command.path);
      return null;
    case "readlink":
      return await fs.readlink(command.path);
    case "rename":
      await fs.rename(command.old_path, command.new_path);
      return null;
    case "unlink":
      await fs.unlink(command.path);
      return null;
    case "rmdir":
      await fs.rmdir(command.path);
      return null;
    case "truncate":
      await fs.truncate(command.path, command.length);
      return null;
    case "mknod":
      await fs.mountx.mknod(command.path, command.mode, command.dev);
      return null;
    case "open": {
      const handle = await fs.open(command.path, command.flags, command.mode);
      handles.set(command.id, handle);
      return { fd: handle.fd };
    }
    case "handle_read": {
      const buffer = new Uint8Array(command.count);
      const result = await handles.get(command.id).read(buffer, 0, buffer.byteLength, position(command));
      return Array.from(buffer.subarray(0, result.bytesRead));
    }
    case "handle_write": {
      const buffer = Uint8Array.from(command.data);
      const result = await handles.get(command.id).write(buffer, 0, buffer.byteLength, position(command));
      return result.bytesWritten;
    }
    case "handle_stat":
      return stableStats(await handles.get(command.id).stat(), command.exact_times !== false);
    case "handle_truncate":
      await handles.get(command.id).truncate(command.length);
      return null;
    case "handle_sync":
      await handles.get(command.id).sync();
      return null;
    case "handle_datasync":
      await handles.get(command.id).datasync();
      return null;
    case "handle_close":
      await handles.get(command.id).close();
      return null;
    default:
      throw new Error(`unknown core parity operation: ${command.op}`);
  }
}

async function runOracle() {
  const fs = createLoopback(createMemoryDriver({ uid: 501, gid: 20, umask: 0, rootMode: 0o755 }));
  const handles = new Map();
  const results = [];
  for (const command of trace) {
    try {
      results.push({ ok: await execute(fs, handles, command) });
    } catch (error) {
      results.push({ error: error?.code ?? String(error) });
    }
  }
  return results;
}

const rustLines = execFileSync(
  "cargo",
  ["run", "--quiet", "--locked", "--manifest-path", "tests/core_parity/Cargo.toml"],
  {
    cwd: repo,
    env: { ...process.env, MOUNTX_SOURCE: source },
    input: `${trace.map((command) => JSON.stringify(command)).join("\n")}\n`,
    encoding: "utf8",
  },
)
  .trim()
  .split("\n")
  .map((line) => JSON.parse(line));
const oracleResults = await runOracle();

assert.equal(rustLines.length, trace.length, "Rust trace result count diverged");
assert.equal(oracleResults.length, trace.length, "oracle trace result count diverged");
for (let index = 0; index < trace.length; index++) {
  assert.deepEqual(rustLines[index], oracleResults[index], `trace step ${index}: ${JSON.stringify(trace[index])}`);
}

const errorCount = oracleResults.filter((result) => "error" in result).length;
console.log(
  `core in-memory parity: PASS (steps=${trace.length}, ok=${trace.length - errorCount}, stable-errors=${errorCount}, fail=0, skipped=0, oracle=${oracleRevision})`,
);
