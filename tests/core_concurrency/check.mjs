import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL, fileURLToPath } from "node:url";

const packetDir = fileURLToPath(new URL(".", import.meta.url));
const repo = fileURLToPath(new URL("../..", import.meta.url));
const packet = JSON.parse(readFileSync(join(packetDir, "scenarios.json"), "utf8"));
const source = process.env.MOUNTX_SOURCE;

function output(status, fields = {}) {
  console.log(
    JSON.stringify({
      packet: packet.packet,
      status,
      ...fields,
    }),
  );
}

if (!source) {
  output("SKIP", {
    reason: "MOUNTX_SOURCE is unset; the pinned TypeScript oracle is required",
    scenarios: 0,
    unsupported: packet.unsupported.length,
    failures: 0,
  });
  process.exit(0);
}

function oracleRevision() {
  return execFileSync("git", ["-C", source, "rev-parse", "HEAD"], {
    encoding: "utf8",
  }).trim();
}

function bytes(command) {
  return Uint8Array.from(command.data);
}

function position(command) {
  return command.position === undefined || command.position === null
    ? undefined
    : command.position;
}

function kind(stats) {
  if (stats.isFile()) return "File";
  if (stats.isDirectory()) return "Directory";
  if (stats.isSymbolicLink()) return "Symlink";
  if (stats.isBlockDevice()) return "BlockDevice";
  if (stats.isCharacterDevice()) return "CharacterDevice";
  if (stats.isFIFO()) return "Fifo";
  if (stats.isSocket()) return "Socket";
  throw new Error("unknown stat kind");
}

function stableStats(stats) {
  return {
    mode: stats.mode,
    nlink: stats.nlink,
    uid: stats.uid,
    gid: stats.gid,
    rdev: stats.rdev,
    size: stats.size,
    blksize: stats.blksize,
    blocks: stats.blocks,
    kind: kind(stats),
  };
}

function entry(command, result) {
  return { ...command, result };
}

async function execute(fs, handles, command) {
  switch (command.op) {
    case "mkdir":
    case "mkdir_recursive":
      return (
        (await fs.mkdir(command.path, {
          recursive: command.op === "mkdir_recursive",
          mode: command.mode,
        })) ?? null
      );
    case "write_file":
      await fs.writeFile(command.path, bytes(command));
      return null;
    case "read":
      return Array.from(await fs.readFile(command.path));
    case "read_chunks": {
      const data = await fs.readFile(command.path);
      const chunks = [];
      for (let offset = 0; offset < data.byteLength; offset += command.chunk_size) {
        chunks.push(Array.from(data.subarray(offset, offset + command.chunk_size)));
      }
      if (command.sort === "bytes") {
        chunks.sort((left, right) => {
          for (let index = 0; index < Math.min(left.length, right.length); index++) {
            if (left[index] !== right[index]) return left[index] - right[index];
          }
          return left.length - right.length;
        });
      }
      return chunks;
    }
    case "list": {
      const entries = (await fs.readdir(command.path, { withFileTypes: true })).map((item) => ({
        name: item.name,
        type: item.isFile()
          ? "File"
          : item.isDirectory()
            ? "Directory"
            : item.isSymbolicLink()
              ? "Symlink"
              : item.isBlockDevice()
                ? "BlockDevice"
                : item.isCharacterDevice()
                  ? "CharacterDevice"
                  : item.isFIFO()
                    ? "Fifo"
                    : "Socket",
      }));
      if (command.sort === "name") entries.sort((left, right) => left.name.localeCompare(right.name));
      return entries;
    }
    case "stat":
      return stableStats(await fs.stat(command.path));
    case "lstat":
      return stableStats(await fs.lstat(command.path));
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
    case "chmod":
      await fs.chmod(command.path, command.mode);
      return null;
    case "chown":
      await fs.chown(command.path, command.uid, command.gid);
      return null;
    case "symlink":
      await fs.symlink(command.target, command.path);
      return null;
    case "readlink":
      return await fs.readlink(command.path);
    case "utimes":
      await fs.utimes(command.path, command.atime_ms / 1000, command.mtime_ms / 1000);
      return null;
    case "lutimes":
      await fs.lutimes(command.path, command.atime_ms / 1000, command.mtime_ms / 1000);
      return null;
    case "mknod":
      await fs.mountx.mknod(command.path, command.mode, command.dev);
      return null;
    case "open":
      handles.set(command.id, await fs.open(command.path, command.flags, command.mode));
      return null;
    case "open_discard": {
      const opened = await fs.open(command.path, command.flags, command.mode);
      await opened.close();
      return null;
    }
    case "handle_read": {
      const buffer = new Uint8Array(command.count);
      const { bytesRead } = await handles.get(command.id).read(buffer, 0, command.count, position(command));
      return Array.from(buffer.subarray(0, bytesRead));
    }
    case "handle_write": {
      const { bytesWritten } = await handles.get(command.id).write(
        bytes(command),
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
      await handles.get(command.id).sync?.();
      return null;
    case "handle_datasync":
      await handles.get(command.id).datasync?.();
      return null;
    case "handle_close":
      await handles.get(command.id).close();
      return null;
    default:
      throw new Error(`unknown concurrency operation ${command.op}`);
  }
}

async function runCommand(fs, handles, command) {
  try {
    return entry(command, { ok: await execute(fs, handles, command) });
  } catch (error) {
    return entry(command, { error: error?.code ?? String(error) });
  }
}

async function runScenario(createFs, scenario) {
  const fs = createFs();
  const handles = new Map();
  const setup = [];
  for (const command of scenario.setup) setup.push(await runCommand(fs, handles, command));

  const rounds = [];
  for (const round of scenario.rounds) {
    const operations = await Promise.all(round.ops.map((command) => runCommand(fs, handles, command)));
    if (round.mode === "multiset") {
      for (const operation of operations) delete operation.actor;
      operations.sort((left, right) => JSON.stringify(left.result).localeCompare(JSON.stringify(right.result)));
    } else if (round.mode !== "ordered") {
      throw new Error(`unknown round mode ${round.mode}`);
    }
    rounds.push({ mode: round.mode, operations });
  }

  const observations = [];
  for (const command of scenario.observe) observations.push(await runCommand(fs, handles, command));
  return { id: scenario.id, setup, rounds, observations };
}

async function main() {
  const revision = oracleRevision();
  assert.equal(revision, packet.oracle_revision, `oracle revision ${revision} is not pinned`);
  const [{ createMemoryDriver }, { createLoopback }] = await Promise.all([
    import(pathToFileURL(join(source, "src/drivers/memory.ts")).href),
    import(pathToFileURL(join(source, "src/harness.ts")).href),
  ]);
  const createFs = () =>
    createLoopback(createMemoryDriver({ uid: 501, gid: 20, umask: 0, rootMode: 0o755 }));
  const scenarios = [];
  for (const scenario of packet.scenarios) scenarios.push(await runScenario(createFs, scenario));

  const rustOutput = execFileSync(
    "cargo",
    ["run", "--quiet", "--locked", "--manifest-path", "tests/core_concurrency/Cargo.toml"],
    {
      cwd: repo,
      env: {
        ...process.env,
        CARGO_TARGET_DIR:
          process.env.CARGO_TARGET_DIR ?? join(tmpdir(), "mount-rs-w01-core-concurrency-target"),
      },
      encoding: "utf8",
    },
  );
  const rustLines = rustOutput.trim().split("\n");
  assert.equal(rustLines.length, 1, "Rust harness must emit one JSON report line");
  const rust = JSON.parse(rustLines[0]);
  assert.equal(rust.packet, packet.packet);
  assert.deepEqual(rust.scenarios, scenarios, "Rust and mountx concurrency observations diverged");
  assert.deepEqual(rust.unsupported, packet.unsupported);

  output("PASS", {
    scenarios: packet.scenarios.length,
    unsupported: packet.unsupported.length,
    failures: 0,
    oracle: revision,
  });
}

try {
  await main();
} catch (error) {
  console.error(error?.stack ?? error);
  output("FAIL", {
    scenarios: packet.scenarios.length,
    unsupported: packet.unsupported.length,
    failures: 1,
    oracle: source ? "unverified" : null,
    reason: error?.message ?? String(error),
  });
  process.exitCode = 1;
}
