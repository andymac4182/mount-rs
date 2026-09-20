import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { pathToFileURL, fileURLToPath } from "node:url";

const repo = fileURLToPath(new URL("../..", import.meta.url));
const expectedOracleRevision = "85361a8212ff9bff8e69f62fa8993ef2c2ec51e8";
const source = process.env.MOUNTX_SOURCE;

if (!source) {
  console.log(
    "core handle lifecycle parity: SKIP (MOUNTX_SOURCE is unset; the pinned TypeScript oracle is required)",
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

const trace = [
  // Cursor reads and writes, explicit positional operations, and truncation
  // must not accidentally share or advance the wrong piece of handle state.
  { op: "open", id: "cursor", path: "/cursor", flags: "w+", mode: 0o644 },
  { op: "handle_write", id: "cursor", data: [0, 255, 1], position: null },
  { op: "handle_read", id: "cursor", count: 2, position: null },
  { op: "handle_read", id: "cursor", count: 3, position: 0 },
  { op: "handle_read", id: "cursor", count: 1, position: null },
  { op: "handle_write", id: "cursor", data: [2, 3], position: null },
  { op: "handle_stat", id: "cursor" },
  { op: "handle_truncate", id: "cursor", length: 8 },
  { op: "handle_read", id: "cursor", count: 8, position: 0 },
  { op: "handle_read", id: "cursor", count: 4, position: null },
  { op: "handle_truncate", id: "cursor", length: 3 },
  { op: "handle_read", id: "cursor", count: 8, position: 0 },

  // Close is idempotent; closed-handle errors take precedence over access
  // checks and the successful sync methods remain harmless after close.
  { op: "handle_close", id: "cursor" },
  { op: "handle_sync", id: "cursor" },
  { op: "handle_datasync", id: "cursor" },
  { op: "handle_read", id: "cursor", count: 1, position: 0 },
  { op: "handle_write", id: "cursor", data: [9], position: 0 },
  { op: "handle_truncate", id: "cursor", length: 0 },
  { op: "handle_stat", id: "cursor" },
  { op: "handle_close", id: "cursor" },

  // O_APPEND chooses EOF even when the caller supplies an explicit position;
  // zero-length writes do not invent bytes or move the cursor unexpectedly.
  { op: "write_file", path: "/append", data: [10, 11, 12] },
  { op: "open", id: "append", path: "/append", flags: "a+", mode: 0 },
  { op: "handle_write", id: "append", data: [20], position: 0 },
  { op: "handle_write", id: "append", data: [], position: 0 },
  { op: "handle_read", id: "append", count: 8, position: 0 },
  { op: "handle_read", id: "append", count: 2, position: null },
  { op: "handle_write", id: "append", data: [21, 22], position: null },
  { op: "handle_read", id: "append", count: 8, position: 0 },
  { op: "handle_close", id: "append" },
  { op: "read_file", path: "/append" },

  // Binary bytes and a write beyond EOF exercise sparse zero filling. The
  // later growth must also remain zero-filled after a shrink and regrow.
  { op: "open", id: "sparse", path: "/sparse", flags: "w+", mode: 0 },
  { op: "handle_write", id: "sparse", data: [170], position: 5 },
  { op: "handle_read", id: "sparse", count: 8, position: 0 },
  { op: "handle_read", id: "sparse", count: 1, position: null },
  { op: "handle_write", id: "sparse", data: [], position: null },
  { op: "handle_write", id: "sparse", data: [187, 188], position: null },
  { op: "handle_read", id: "sparse", count: 8, position: 0 },
  { op: "handle_truncate", id: "sparse", length: 10 },
  { op: "handle_read", id: "sparse", count: 12, position: 0 },
  { op: "handle_stat", id: "sparse" },
  { op: "handle_close", id: "sparse" },
  { op: "read_file", path: "/sparse" },

  // Access-mode errors happen before I/O; a handle can still be used for the
  // operation its flags allow until it is closed.
  { op: "write_file", path: "/readonly", data: [4, 5] },
  { op: "open", id: "readonly", path: "/readonly", flags: "r", mode: 0 },
  { op: "handle_write", id: "readonly", data: [6], position: 0 },
  { op: "handle_truncate", id: "readonly", length: 1 },
  { op: "handle_read", id: "readonly", count: 8, position: 0 },
  { op: "handle_close", id: "readonly" },
  { op: "open", id: "writeonly", path: "/writeonly", flags: "w", mode: 0o600 },
  { op: "handle_read", id: "writeonly", count: 1, position: 0 },
  { op: "handle_close", id: "writeonly" },

  // A live handle retains the inode after unlink, including its cursor and
  // bytes; the pathname disappears independently.
  { op: "write_file", path: "/orphan", data: [9, 8, 7] },
  { op: "open", id: "orphan", path: "/orphan", flags: "r+", mode: 0 },
  { op: "unlink", path: "/orphan" },
  { op: "handle_stat", id: "orphan" },
  { op: "handle_read", id: "orphan", count: 8, position: 0 },
  { op: "handle_write", id: "orphan", data: [6], position: 1 },
  { op: "handle_read", id: "orphan", count: 8, position: 0 },
  { op: "handle_close", id: "orphan" },
  { op: "read_file", path: "/orphan" },

  // Directory handles may be stat'ed, but byte I/O is rejected as EISDIR.
  { op: "mkdir", path: "/directory", mode: 0o755 },
  { op: "open", id: "directory", path: "/directory", flags: "r", mode: 0 },
  { op: "handle_stat", id: "directory" },
  { op: "handle_read", id: "directory", count: 1, position: 0 },
  { op: "handle_close", id: "directory" },

  // Empty files and zero-length reads are valid even with a read position past
  // EOF; no data is copied and the handle remains usable.
  { op: "write_file", path: "/empty", data: [] },
  { op: "open", id: "empty", path: "/empty", flags: "r+", mode: 0 },
  { op: "handle_read", id: "empty", count: 0, position: 999 },
  { op: "handle_read", id: "empty", count: 1, position: 0 },
  { op: "handle_write", id: "empty", data: [], position: 0 },
  { op: "handle_close", id: "empty" },
  { op: "read_file", path: "/empty" },
];

function stableStats(stats) {
  let kind = "File";
  if (stats.isDirectory()) kind = "Directory";
  else if (stats.isSymbolicLink()) kind = "Symlink";
  else if (stats.isBlockDevice()) kind = "BlockDevice";
  else if (stats.isCharacterDevice()) kind = "CharacterDevice";
  else if (stats.isFIFO()) kind = "Fifo";
  else if (stats.isSocket()) kind = "Socket";
  return {
    mode: stats.mode,
    nlink: stats.nlink,
    uid: stats.uid,
    gid: stats.gid,
    rdev: stats.rdev,
    size: stats.size,
    blksize: stats.blksize,
    blocks: stats.blocks,
    kind,
  };
}

function position(command) {
  return command.position == null ? undefined : command.position;
}

async function execute(fs, handles, command) {
  switch (command.op) {
    case "mkdir":
      await fs.mkdir(command.path, { recursive: false, mode: command.mode });
      return null;
    case "write_file":
      await fs.writeFile(command.path, Uint8Array.from(command.data));
      return null;
    case "read_file":
      return Array.from(await fs.readFile(command.path));
    case "unlink":
      await fs.unlink(command.path);
      return null;
    case "open": {
      const handle = await fs.open(command.path, command.flags, command.mode);
      handles.set(command.id, handle);
      return { fd: handle.fd };
    }
    case "handle_read": {
      const buffer = new Uint8Array(command.count);
      const { bytesRead } = await handles.get(command.id).read(
        buffer,
        0,
        command.count,
        position(command),
      );
      return Array.from(buffer.subarray(0, bytesRead));
    }
    case "handle_write": {
      const { bytesWritten } = await handles.get(command.id).write(
        Uint8Array.from(command.data),
        0,
        command.data.length,
        position(command),
      );
      return bytesWritten;
    }
    case "handle_stat":
      return stableStats(await handles.get(command.id).stat());
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
      throw new Error(`unknown core lifecycle operation: ${command.op}`);
  }
}

async function runOracle() {
  const fs = createLoopback(
    createMemoryDriver({ uid: 501, gid: 20, umask: 0, rootMode: 0o755 }),
  );
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
  ["run", "--quiet", "--locked", "--manifest-path", "tests/core_lifecycle/Cargo.toml"],
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

assert.equal(rustLines.length, trace.length, "Rust lifecycle result count diverged");
assert.equal(oracleResults.length, trace.length, "oracle lifecycle result count diverged");
for (let index = 0; index < trace.length; index++) {
  assert.deepEqual(
    rustLines[index],
    oracleResults[index],
    `lifecycle step ${index}: ${JSON.stringify(trace[index])}`,
  );
}

const errorCount = oracleResults.filter((result) => "error" in result).length;
console.log(
  `core handle lifecycle parity: PASS (steps=${trace.length}, ok=${trace.length - errorCount}, errors=${errorCount}, fail=0, skipped=0, oracle=${oracleRevision})`,
);
