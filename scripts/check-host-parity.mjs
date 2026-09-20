import assert from "node:assert/strict";
import { constants } from "node:fs";
import { execFileSync } from "node:child_process";
import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { isDeepStrictEqual } from "node:util";
import { fileURLToPath, pathToFileURL } from "node:url";

const repo = fileURLToPath(new URL("..", import.meta.url));
const source = process.env.MOUNTX_SOURCE;
if (!source) {
  throw new Error("MOUNTX_SOURCE must point to the pinned mountx checkout");
}

const sourceRoot = pathToFileURL(source.endsWith("/") ? source : `${source}/`);
const [{ createNodeFsDriver }, { createLoopback }] = await Promise.all([
  import(new URL("src/drivers/node-fs.ts", sourceRoot).href),
  import(new URL("src/harness.ts", sourceRoot).href),
]);

const bytes = (text) => Array.from(Buffer.from(text, "utf8"));

function stableStats(stats) {
  // Compare the semantic fields that a rooted host driver promises. Deliberately
  // ignore only host-allocation observations: dev/ino, block geometry, and all
  // timestamps differ between isolated temporary roots and macOS/Linux filesystems.
  return {
    mode: stats.mode,
    nlink: stats.nlink,
    uid: stats.uid,
    gid: stats.gid,
    rdev: stats.rdev,
    size: stats.size,
    isFile: stats.isFile(),
    isDirectory: stats.isDirectory(),
    isSymbolicLink: stats.isSymbolicLink(),
    isBlockDevice: stats.isBlockDevice(),
    isCharacterDevice: stats.isCharacterDevice(),
    isFIFO: stats.isFIFO(),
    isSocket: stats.isSocket(),
  };
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
    // `mknod` is not part of the upstream capability shape; an absent optional
    // member has the same resolved meaning as false in the Rust contract.
    mknod: capabilities.mknod ?? false,
  };
}

function direntType(entry) {
  if (entry.isDirectory()) return "Directory";
  if (entry.isSymbolicLink()) return "Symlink";
  if (entry.isFile()) return "File";
  if (entry.isBlockDevice()) return "BlockDevice";
  if (entry.isCharacterDevice()) return "CharacterDevice";
  if (entry.isFIFO()) return "FIFO";
  if (entry.isSocket()) return "Socket";
  return "Unknown";
}

function normalizeHostPath(value, root) {
  if (typeof value !== "string") return null;
  const absoluteRoot = root.endsWith("/") ? root.slice(0, -1) : root;
  if (value === absoluteRoot) return "<root>";
  if (value.startsWith(`${absoluteRoot}/`)) {
    return `<root>${value.slice(absoluteRoot.length)}`;
  }
  return value;
}

function errorShape(error, root) {
  return {
    code: typeof error?.code === "string" ? error.code : "UNKNOWN",
    syscall: typeof error?.syscall === "string" ? error.syscall : null,
    path: normalizeHostPath(error?.path, root),
    dest: normalizeHostPath(error?.dest, root),
  };
}

function commandPosition(command) {
  return command.position === undefined ? null : command.position;
}

async function runTypeScript(root, commands) {
  const fs = createLoopback(createNodeFsDriver(root));
  const handles = new Map();
  const results = [];

  const execute = async (command) => {
    switch (command.op) {
      case "capabilities":
        return stableCapabilities(fs.capabilities);
      case "mkdir":
        return (
          (await fs.mkdir(command.path, {
            recursive: command.recursive ?? false,
            mode: command.mode,
          })) ?? null
        );
      case "rmdir":
        await fs.rmdir(command.path);
        return null;
      case "unlink":
        await fs.unlink(command.path);
        return null;
      case "rename":
        await fs.rename(command.path, command.dest);
        return null;
      case "link":
        await fs.link(command.path, command.dest);
        return null;
      case "symlink":
        await fs.symlink(command.target, command.path);
        return null;
      case "readlink":
        return await fs.readlink(command.path);
      case "chmod":
        await fs.chmod(command.path, command.mode);
        return null;
      case "truncate":
        await fs.truncate(command.path, command.length);
        return null;
      case "stat":
        return stableStats(await fs.stat(command.path));
      case "lstat":
        return stableStats(await fs.lstat(command.path));
      case "readdir": {
        const entries = await fs.readdir(command.path, { withFileTypes: true });
        return entries
          .sort((left, right) => left.name.localeCompare(right.name))
          .map((entry) => ({ name: entry.name, type: direntType(entry) }));
      }
      case "open": {
        const handle = await fs.open(command.path, command.flags, command.mode);
        handles.set(command.id, handle);
        return { opened: true };
      }
      case "open_numeric": {
        const handle = await fs.open(command.path, command.flags, command.mode);
        handles.set(command.id, handle);
        return { opened: true };
      }
      case "handle_write": {
        const input = Uint8Array.from(command.buffer);
        const result = await handles.get(command.id).write(
          input,
          command.offset,
          command.length,
          commandPosition(command),
        );
        return { bytesWritten: result.bytesWritten, buffer: Array.from(result.buffer) };
      }
      case "handle_read": {
        const buffer = Uint8Array.from(command.buffer);
        const result = await handles.get(command.id).read(
          buffer,
          command.offset,
          command.length,
          commandPosition(command),
        );
        return { bytesRead: result.bytesRead, buffer: Array.from(result.buffer) };
      }
      case "handle_stat":
        return stableStats(await handles.get(command.id).stat());
      case "handle_truncate":
        await handles.get(command.id).truncate(command.length);
        return null;
      case "handle_sync":
        await handles.get(command.id).sync?.();
        return null;
      case "handle_datasync":
        await handles.get(command.id).datasync?.();
        return null;
      case "handle_close":
        await handles.get(command.id).close();
        return null;
      default:
        throw new Error(`unknown host parity operation ${command.op}`);
    }
  };

  for (const command of commands) {
    try {
      results.push({ ok: (await execute(command)) ?? null });
    } catch (error) {
      results.push({ error: errorShape(error, root) });
    }
  }
  return results;
}

function commandsForPlatform() {
  // The numeric value is intentionally obtained from Node on this host. The
  // Rust oracle decodes this Node namespace with Linux/Darwin-specific O_* bits.
  const numericReadWriteCreate = constants.O_RDWR | constants.O_CREAT;
  return [
    { op: "capabilities" },
    { op: "mkdir", path: "/alpha/beta", recursive: true, mode: 0o755 },
    { op: "mkdir", path: "/alpha/beta", recursive: true, mode: 0o755 },
    { op: "readdir", path: "/alpha" },

    { op: "open", id: "create", path: "/alpha/beta/data", flags: "w+", mode: 0o640 },
    {
      op: "handle_write",
      id: "create",
      buffer: bytes("hello"),
      offset: 0,
      length: 5,
      position: null,
    },
    {
      op: "handle_write",
      id: "create",
      buffer: bytes("_XY_"),
      offset: 1,
      length: 2,
      position: 1,
    },
    {
      op: "handle_read",
      id: "create",
      buffer: [0xaa, 0xaa, 0xaa, 0xaa, 0xaa],
      offset: 1,
      length: 3,
      position: 1,
    },
    {
      op: "handle_read",
      id: "create",
      buffer: [0xbb, 0xbb],
      offset: 0,
      length: 2,
      position: null,
    },
    { op: "handle_stat", id: "create" },
    { op: "handle_sync", id: "create" },
    { op: "handle_datasync", id: "create" },
    { op: "handle_close", id: "create" },

    { op: "open", id: "cursor", path: "/alpha/beta/data", flags: "r", mode: 0o666 },
    {
      op: "handle_read",
      id: "cursor",
      buffer: [0, 0],
      offset: 0,
      length: 2,
      position: null,
    },
    {
      op: "handle_read",
      id: "cursor",
      buffer: [0, 0],
      offset: 0,
      length: 2,
      position: null,
    },
    {
      op: "handle_read",
      id: "cursor",
      buffer: [0, 0, 0],
      offset: 0,
      length: 3,
      position: 0,
    },
    { op: "handle_close", id: "cursor" },

    { op: "open", id: "append", path: "/alpha/beta/data", flags: "a+", mode: 0o666 },
    {
      op: "handle_write",
      id: "append",
      buffer: bytes("!"),
      offset: 0,
      length: 1,
      position: 0,
    },
    { op: "handle_close", id: "append" },
    {
      op: "open_numeric",
      id: "numeric",
      path: "/numeric",
      flags: numericReadWriteCreate,
      mode: 0o640,
    },
    {
      op: "handle_write",
      id: "numeric",
      buffer: [0, 65, 66],
      offset: 1,
      length: 1,
      position: 0,
    },
    { op: "handle_truncate", id: "numeric", length: 2 },
    { op: "handle_stat", id: "numeric" },
    { op: "handle_close", id: "numeric" },
    { op: "chmod", path: "/numeric", mode: 0o600 },
    { op: "stat", path: "/numeric" },
    { op: "truncate", path: "/numeric", length: 1 },
    { op: "stat", path: "/numeric" },

    { op: "open", id: "readonly", path: "/numeric", flags: "r", mode: 0o666 },
    {
      op: "handle_write",
      id: "readonly",
      buffer: bytes("x"),
      offset: 0,
      length: 1,
      position: 0,
    },
    { op: "handle_close", id: "readonly" },
    { op: "open", id: "missing", path: "/missing", flags: "r", mode: 0o666 },
    { op: "open", id: "exclusive-existing", path: "/numeric", flags: "wx", mode: 0o666 },

    { op: "mkdir", path: "/dir", recursive: false, mode: 0o755 },
    { op: "open", id: "write-dir", path: "/dir", flags: "w", mode: 0o666 },
    { op: "open", id: "read-dir", path: "/dir", flags: "r", mode: 0o666 },
    {
      op: "handle_read",
      id: "read-dir",
      buffer: [0, 0, 0, 0],
      offset: 0,
      length: 4,
      position: 0,
    },
    { op: "handle_close", id: "read-dir" },

    { op: "open", id: "doomed-create", path: "/doomed", flags: "w", mode: 0o666 },
    {
      op: "handle_write",
      id: "doomed-create",
      buffer: bytes("still here"),
      offset: 0,
      length: 10,
      position: null,
    },
    { op: "handle_close", id: "doomed-create" },
    { op: "open", id: "doomed-hold", path: "/doomed", flags: "r", mode: 0o666 },
    { op: "unlink", path: "/doomed" },
    { op: "stat", path: "/doomed" },
    {
      op: "handle_read",
      id: "doomed-hold",
      buffer: Array(12).fill(0),
      offset: 0,
      length: 12,
      position: 0,
    },
    { op: "handle_stat", id: "doomed-hold" },
    { op: "handle_close", id: "doomed-hold" },

    { op: "open", id: "move-create", path: "/from", flags: "w", mode: 0o666 },
    {
      op: "handle_write",
      id: "move-create",
      buffer: bytes("aaaa"),
      offset: 0,
      length: 4,
      position: null,
    },
    { op: "handle_close", id: "move-create" },
    { op: "open", id: "moved-hold", path: "/from", flags: "r+", mode: 0o666 },
    {
      op: "handle_write",
      id: "moved-hold",
      buffer: bytes("bb"),
      offset: 0,
      length: 2,
      position: 0,
    },
    { op: "rename", path: "/from", dest: "/to" },
    {
      op: "handle_write",
      id: "moved-hold",
      buffer: bytes("cc"),
      offset: 0,
      length: 2,
      position: 2,
    },
    { op: "handle_close", id: "moved-hold" },
    { op: "stat", path: "/from" },
    { op: "open", id: "to-reader", path: "/to", flags: "r", mode: 0o666 },
    {
      op: "handle_read",
      id: "to-reader",
      buffer: [0, 0, 0, 0],
      offset: 0,
      length: 4,
      position: 0,
    },
    { op: "handle_close", id: "to-reader" },

    { op: "open", id: "hardlink-create", path: "/original", flags: "w", mode: 0o666 },
    {
      op: "handle_write",
      id: "hardlink-create",
      buffer: bytes("shared"),
      offset: 0,
      length: 6,
      position: null,
    },
    { op: "handle_close", id: "hardlink-create" },
    { op: "link", path: "/original", dest: "/alias" },
    { op: "stat", path: "/original" },
    { op: "stat", path: "/alias" },
    { op: "unlink", path: "/original" },
    { op: "stat", path: "/alias" },

    { op: "mkdir", path: "/real", recursive: false, mode: 0o755 },
    { op: "open", id: "real-create", path: "/real/file", flags: "w", mode: 0o666 },
    {
      op: "handle_write",
      id: "real-create",
      buffer: bytes("inside"),
      offset: 0,
      length: 6,
      position: null,
    },
    { op: "handle_close", id: "real-create" },
    { op: "symlink", target: "real", path: "/alias-dir" },
    { op: "readlink", path: "/alias-dir" },
    { op: "lstat", path: "/alias-dir" },
    { op: "stat", path: "/alias-dir" },
    { op: "open", id: "alias-reader", path: "/alias-dir/file", flags: "r", mode: 0o666 },
    {
      op: "handle_read",
      id: "alias-reader",
      buffer: Array(8).fill(0),
      offset: 0,
      length: 8,
      position: 0,
    },
    { op: "handle_close", id: "alias-reader" },

    { op: "mkdir", path: "/deep/deeper", recursive: true, mode: 0o755 },
    { op: "symlink", target: "../../../outside", path: "/deep/deeper/escape" },
    { op: "mkdir", path: "/outside", recursive: false, mode: 0o755 },
    { op: "open", id: "inside-outside-create", path: "/outside/inside", flags: "w", mode: 0o666 },
    {
      op: "handle_write",
      id: "inside-outside-create",
      buffer: bytes("safe"),
      offset: 0,
      length: 4,
      position: null,
    },
    { op: "handle_close", id: "inside-outside-create" },
    { op: "stat", path: "/deep/deeper/escape/secret" },
    { op: "symlink", target: "/outside", path: "/absolute-escape" },
    { op: "stat", path: "/absolute-escape/inside" },
    { op: "open", id: "marker-create", path: "/marker", flags: "w", mode: 0o666 },
    {
      op: "handle_write",
      id: "marker-create",
      buffer: bytes("marker"),
      offset: 0,
      length: 6,
      position: null,
    },
    { op: "handle_close", id: "marker-create" },
    { op: "symlink", target: "/", path: "/top" },
    { op: "symlink", target: "../../..", path: "/up" },
    { op: "readdir", path: "/top/up/top" },
    { op: "symlink", target: "b", path: "/loop-a" },
    { op: "symlink", target: "a", path: "/loop-b" },
    { op: "stat", path: "/loop-a" },
    { op: "open", id: "loop-open", path: "/loop-a", flags: "r", mode: 0o666 },

    { op: "symlink", target: "nowhere", path: "/dangling" },
    { op: "open", id: "dangling-exclusive", path: "/dangling", flags: "wx", mode: 0o666 },
    { op: "stat", path: "/nowhere" },
    { op: "open", id: "dangling-follow", path: "/dangling", flags: "w", mode: 0o666 },
    { op: "handle_close", id: "dangling-follow" },
    { op: "stat", path: "/nowhere" },

    { op: "unlink", path: "/dir" },
    { op: "rmdir", path: "/numeric" },
    { op: "rmdir", path: "/" },
    { op: "rmdir", path: "/." },
    { op: "rmdir", path: "/a/.." },
    { op: "rename", path: "/missing-source", dest: "/missing-dest" },
    { op: "link", path: "/missing-source", dest: "/missing-dest" },
  ];
}

function rustResults(root, commands) {
  const input = `${commands.map((command) => JSON.stringify(command)).join("\n")}\n`;
  const output = execFileSync(
    "cargo",
    ["run", "--locked", "--quiet", "--example", "host_oracle", "--", root],
    { cwd: repo, env: process.env, input, encoding: "utf8" },
  );
  return output
    .trim()
    .split("\n")
    .filter((line) => line.length > 0)
    .map((line) => JSON.parse(line));
}

const sandbox = await mkdtemp(join(tmpdir(), "mount-rs-host-parity-"));
const typescriptRoot = join(sandbox, "typescript-root");
const rustRoot = join(sandbox, "rust-root");
const outside = join(sandbox, "outside");
await Promise.all([mkdir(typescriptRoot), mkdir(rustRoot), mkdir(outside)]);
await writeFile(join(outside, "secret"), "classified");

try {
  const commands = commandsForPlatform();
  const typescript = await runTypeScript(typescriptRoot, commands);
  const rust = rustResults(rustRoot, commands).map((result) => {
    if (result.error) {
      return { error: { ...result.error, path: normalizeHostPath(result.error.path, rustRoot), dest: normalizeHostPath(result.error.dest, rustRoot) } };
    }
    return result;
  });
  assert.equal(rust.length, typescript.length, "host parity result count diverged");
  const mismatches = [];
  for (let index = 0; index < commands.length; index++) {
    if (!isDeepStrictEqual(rust[index], typescript[index])) {
      mismatches.push(index);
    }
  }
  if (mismatches.length > 0) {
    for (const index of mismatches.slice(0, 12)) {
      console.error(`host parity mismatch at step ${index}: ${JSON.stringify(commands[index])}`);
      console.error(`  Rust: ${JSON.stringify(rust[index])}`);
      console.error(`  TS:   ${JSON.stringify(typescript[index])}`);
    }
    throw new Error(`mount-rs-host diverged from createNodeFsDriver at ${mismatches.length} step(s)`);
  }
  console.log(`mount-rs-host differential parity: PASS (${commands.length} operations)`);
} finally {
  await rm(sandbox, { recursive: true, force: true });
}
