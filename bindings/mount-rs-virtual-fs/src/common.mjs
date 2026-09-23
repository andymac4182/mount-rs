import { constants as fsConstants } from "node:fs";
import path from "node:path";

import { fsError } from "@mount-rs/core";

import { assertMountRsBackend } from "./backend.mjs";

const posix = path.posix;
const MAX_SYMLINK_DEPTH = 40;

export function normalizeVirtualPath(value) {
  if (typeof value !== "string") {
    throw new TypeError("filesystem paths must be strings");
  }
  return posix.normalize(value.startsWith("/") ? value : posix.resolve("/", value));
}

export function virtualParent(value) {
  return posix.dirname(normalizeVirtualPath(value));
}

export function virtualJoin(parent, name) {
  return normalizeVirtualPath(posix.join(normalizeVirtualPath(parent), name));
}

export function errorCode(error) {
  return error && typeof error === "object" && "code" in error ? error.code : undefined;
}

export function isErrorCode(error, code) {
  return errorCode(error) === code;
}

export function readOnlyError(operation, pathValue) {
  return fsError("EROFS", {
    syscall: operation,
    path: normalizeVirtualPath(pathValue),
    message: `read-only filesystem: ${operation}`,
  });
}

export function closedError() {
  return fsError("EBADF", {
    syscall: "filesystem",
    path: "/",
    message: "filesystem adapter is closed",
  });
}

function normalizeEncoding(encoding) {
  if (encoding === "utf-8") return "utf8";
  if (encoding === "binary") return "latin1";
  return encoding ?? "utf8";
}

export function toBytes(value, encoding) {
  if (value instanceof Uint8Array) return new Uint8Array(value);
  if (typeof value !== "string") {
    throw new TypeError("file content must be a string or Uint8Array");
  }
  return new Uint8Array(Buffer.from(value, normalizeEncoding(encoding)));
}

export function fromBytes(value, encoding = "utf8") {
  return Buffer.from(value).toString(normalizeEncoding(encoding));
}

export function toByteString(value) {
  let result = "";
  for (const byte of value) result += String.fromCharCode(byte);
  return result;
}

function hasExitedPath(pathValue, root) {
  return pathValue === root || pathValue.startsWith(`${root}/`);
}

export class PathIndex {
  #filesystem;
  #paths = new Set(["/"]);

  constructor(filesystem) {
    this.#filesystem = filesystem;
  }

  async refresh() {
    const next = new Set(["/"]);
    const visit = async (directory) => {
      const entries = await this.#filesystem.readdir(directory, { withFileTypes: true });
      for (const entry of entries) {
        const child = virtualJoin(directory, entry.name);
        next.add(child);
        if (entry.isDirectory()) await visit(child);
      }
    };
    await visit("/");
    this.#paths = next;
    return this.snapshot();
  }

  add(value) {
    let current = normalizeVirtualPath(value);
    while (true) {
      this.#paths.add(current);
      if (current === "/") break;
      current = posix.dirname(current);
    }
  }

  addEntry(parent, entry) {
    this.add(virtualJoin(parent, entry.name));
  }

  remove(value) {
    const normalized = normalizeVirtualPath(value);
    for (const current of this.#paths) {
      if (hasExitedPath(current, normalized)) this.#paths.delete(current);
    }
    this.#paths.add("/");
  }

  rename(oldValue, newValue) {
    const oldPath = normalizeVirtualPath(oldValue);
    const newPath = normalizeVirtualPath(newValue);
    const replacements = [];
    for (const current of this.#paths) {
      if (hasExitedPath(current, oldPath)) {
        replacements.push([current, posix.join(newPath, posix.relative(oldPath, current))]);
      }
    }
    for (const current of this.#paths) {
      if (hasExitedPath(current, newPath)) this.#paths.delete(current);
    }
    for (const [oldPathValue] of replacements) this.#paths.delete(oldPathValue);
    this.add(newPath);
    for (const [, replacement] of replacements) this.#paths.add(normalizeVirtualPath(replacement));
  }

  snapshot() {
    return [...this.#paths].sort();
  }
}

async function resolveVirtualPath(filesystem, inputPath, { allowMissing = false, followFinal = true } = {}) {
  let pending = normalizeVirtualPath(inputPath);
  let links = 0;

  while (true) {
    const parts = pending === "/" ? [] : pending.slice(1).split("/");
    let current = "/";
    let restarted = false;

    for (let index = 0; index < parts.length; index += 1) {
      const candidate = virtualJoin(current, parts[index]);
      let entry;
      try {
        entry = await filesystem.lstat(candidate);
      } catch (error) {
        if (allowMissing && isErrorCode(error, "ENOENT")) {
          const remainder = parts.slice(index).join("/");
          return normalizeVirtualPath(remainder ? posix.join(current, remainder) : current);
        }
        throw error;
      }
      const isFinal = index === parts.length - 1;
      if (entry.isSymbolicLink() && (followFinal || !isFinal)) {
        links += 1;
        if (links > MAX_SYMLINK_DEPTH) {
          throw fsError("ELOOP", {
            syscall: "realpath",
            path: normalizeVirtualPath(inputPath),
          });
        }
        const target = await filesystem.readlink(candidate);
        const targetPath = target.startsWith("/") ? target : posix.join(current, target);
        const remainder = parts.slice(index + 1).join("/");
        pending = normalizeVirtualPath(remainder ? posix.join(targetPath, remainder) : targetPath);
        restarted = true;
        break;
      }
      current = candidate;
    }

    if (!restarted) return current;
  }
}

export async function resolveVirtualRealpath(filesystem, inputPath) {
  return resolveVirtualPath(filesystem, inputPath);
}

function copyInvalidError(source, destination, message) {
  return fsError("EINVAL", {
    syscall: "copy",
    path: destination,
    message: `${message}: ${source} -> ${destination}`,
  });
}

function sameStatIdentity(left, right) {
  return left.dev !== undefined &&
    right.dev !== undefined &&
    left.ino !== undefined &&
    right.ino !== undefined &&
    String(left.dev) === String(right.dev) &&
    String(left.ino) === String(right.ino);
}

function statsToEntry(entry) {
  return {
    name: entry.name,
    isFile: entry.isFile(),
    isDirectory: entry.isDirectory(),
    isSymbolicLink: entry.isSymbolicLink(),
  };
}

function statsToJustBashStats(stats) {
  return {
    isFile: stats.isFile(),
    isDirectory: stats.isDirectory(),
    isSymbolicLink: stats.isSymbolicLink(),
    mode: stats.mode,
    size: stats.size,
    mtime: new Date(stats.mtimeMs),
    dev: stats.dev,
    ino: stats.ino,
  };
}

export class MountRsFilesystemBase {
  constructor(filesystem, options = {}) {
    this.filesystem = assertMountRsBackend(filesystem);
    this.readOnly = filesystem.capabilities?.readOnly === true || options.readOnly === true;
    this.pathIndex = new PathIndex(filesystem);
    this.closeOnDestroy = options.closeOnDestroy === true;
    this.active = 0;
    this.closing = false;
    this.closed = false;
    this.closePromise = undefined;
  }

  async _run(callback) {
    if (this.closing || this.closed) throw closedError();
    this.active += 1;
    try {
      return await callback();
    } finally {
      this.active -= 1;
      this._resolveIdle?.();
    }
  }

  _assertWritable(operation, pathValue) {
    if (this.readOnly) throw readOnlyError(operation, pathValue);
  }

  _path(value) {
    return normalizeVirtualPath(value);
  }

  async _refreshPaths() {
    return this._run(() => this.pathIndex.refresh());
  }

  async refreshPaths() {
    return this._refreshPaths();
  }

  async close() {
    if (this.closePromise) return this.closePromise;
    this.closePromise = (async () => {
      this.closing = true;
      if (this.active > 0) {
        await new Promise((resolve) => {
          this._resolveIdle = () => {
            if (this.active === 0) {
              this._resolveIdle = undefined;
              resolve();
            }
          };
        });
      }
      if (this.closeOnDestroy && typeof this.filesystem.shutdown === "function") {
        await this.filesystem.shutdown();
      }
      this.closed = true;
    })();
    return this.closePromise;
  }

  getAllPaths() {
    return this.pathIndex.snapshot();
  }

  resolvePath(base, value) {
    return normalizeVirtualPath(posix.resolve(this._path(base), value));
  }

  async _entries(pathValue) {
    const directory = this._path(pathValue);
    const entries = await this.filesystem.readdir(directory, { withFileTypes: true });
    for (const entry of entries) this.pathIndex.addEntry(directory, entry);
    return entries;
  }

  async _stat(pathValue) {
    return this.filesystem.stat(this._path(pathValue));
  }

  async _lstat(pathValue) {
    return this.filesystem.lstat(this._path(pathValue));
  }

  async _readBytes(pathValue) {
    return new Uint8Array(await this.filesystem.readFile(this._path(pathValue)));
  }

  async _writeHandleFully(handle, bytes, position, pathValue) {
    let offset = 0;
    while (offset < bytes.byteLength) {
      const remaining = bytes.byteLength - offset;
      const result = await handle.write(
        bytes,
        offset,
        remaining,
        position === null ? null : position + offset,
      );
      const bytesWritten = result?.bytesWritten;
      if (!Number.isSafeInteger(bytesWritten) || bytesWritten <= 0 || bytesWritten > remaining) {
        throw fsError("EIO", {
          syscall: "write",
          path: this._path(pathValue),
          message: "filesystem handle made no forward write progress",
        });
      }
      offset += bytesWritten;
    }
  }

  async _writeBytes(pathValue, bytes, { createParents = true, overwrite = true, onCreate, createMode = 0o666 } = {}) {
    const filePath = this._path(pathValue);
    if (createParents) await this.filesystem.mkdir(virtualParent(filePath), { recursive: true });
    if (!overwrite) {
      const flags = fsConstants.O_WRONLY | fsConstants.O_CREAT | fsConstants.O_EXCL;
      const handle = await this.filesystem.open(filePath, flags, createMode);
      try {
        await onCreate?.(handle);
        await this._writeHandleFully(handle, bytes, 0, filePath);
      } finally {
        await handle.close();
      }
    } else {
      await this.filesystem.writeFile(filePath, bytes);
    }
    this.pathIndex.add(filePath);
  }

  async _appendBytes(pathValue, bytes) {
    const filePath = this._path(pathValue);
    await this.filesystem.mkdir(virtualParent(filePath), { recursive: true });
    const flags = fsConstants.O_WRONLY | fsConstants.O_CREAT | fsConstants.O_APPEND;
    const handle = await this.filesystem.open(filePath, flags, 0o666);
    try {
      await this._writeHandleFully(handle, bytes, null, filePath);
    } finally {
      await handle.close();
    }
    this.pathIndex.add(filePath);
  }

  async _remove(pathValue, { recursive = false, force = false, expectedIdentity } = {}) {
    const filePath = this._path(pathValue);
    let stats;
    try {
      stats = await this.filesystem.lstat(filePath);
    } catch (error) {
      if (force && isErrorCode(error, "ENOENT")) return;
      throw error;
    }

    if (expectedIdentity &&
        (!sameStatIdentity(expectedIdentity, stats) ||
          expectedIdentity.isFile() !== stats.isFile() ||
          expectedIdentity.isDirectory() !== stats.isDirectory() ||
          expectedIdentity.isSymbolicLink() !== stats.isSymbolicLink())) {
      throw fsError("EAGAIN", {
        syscall: "unlink",
        path: filePath,
        message: "entry changed before removal",
      });
    }

    if (stats.isDirectory()) {
      if (!recursive) {
        await this.filesystem.rmdir(filePath);
      } else {
        const entries = await this._entries(filePath);
        for (const entry of entries) {
          await this._remove(virtualJoin(filePath, entry.name), { recursive: true });
        }
        await this.filesystem.rmdir(filePath);
      }
    } else {
      await this.filesystem.unlink(filePath);
    }
    this.pathIndex.remove(filePath);
  }

  async _copy(sourceValue, destinationValue, { recursive = false, overwrite = true, onCreate } = {}) {
    const source = this._path(sourceValue);
    const destination = this._path(destinationValue);
    const sourceStats = await this.filesystem.lstat(source);
    const sourceEntryPath = await resolveVirtualPath(this.filesystem, source, { followFinal: false });
    const destinationEntryPath = await resolveVirtualPath(this.filesystem, destination, {
      allowMissing: true,
      followFinal: false,
    });
    const sourceTargetPath = sourceStats.isSymbolicLink()
      ? sourceEntryPath
      : await resolveVirtualPath(this.filesystem, source);
    const destinationTargetPath = await resolveVirtualPath(this.filesystem, destination, {
      allowMissing: true,
    });

    if (sourceEntryPath === destinationEntryPath || sourceTargetPath === destinationTargetPath) {
      throw copyInvalidError(source, destination, "source and destination refer to the same path");
    }
    if (
      sourceStats.isDirectory() &&
      (hasExitedPath(destinationEntryPath, sourceEntryPath) ||
        hasExitedPath(destinationTargetPath, sourceTargetPath))
    ) {
      throw copyInvalidError(source, destination, "cannot copy a directory into itself");
    }

    let destinationStats;
    try {
      destinationStats = await this.filesystem.lstat(destination);
    } catch (error) {
      if (!isErrorCode(error, "ENOENT")) throw error;
    }
    if (destinationStats && !sourceStats.isSymbolicLink()) {
      const sourceTargetStats = await this.filesystem.stat(source);
      const destinationTargetStats = await this.filesystem.stat(destination);
      if (sameStatIdentity(sourceTargetStats, destinationTargetStats)) {
        throw copyInvalidError(source, destination, "source and destination refer to the same file");
      }
    }

    if (sourceStats.isSymbolicLink()) {
      const symlinkTarget = await this.filesystem.readlink(source);
      if (!overwrite) {
        try {
          await this.filesystem.lstat(destination);
          throw fsError("EEXIST", { syscall: "copy", path: destination });
        } catch (error) {
          if (!isErrorCode(error, "ENOENT")) throw error;
        }
      } else {
        await this._remove(destination, { recursive: true, force: true });
      }
      await this.filesystem.mkdir(virtualParent(destination), { recursive: true });
      await this.filesystem.symlink(symlinkTarget, destination);
      await onCreate?.();
      this.pathIndex.add(destination);
      return;
    }

    if (sourceStats.isDirectory()) {
      if (!recursive) throw fsError("EISDIR", { syscall: "copy", path: source });
      await this.filesystem.mkdir(destination, { recursive: true });
      this.pathIndex.add(destination);
      for (const entry of await this._entries(source)) {
        await this._copy(
          virtualJoin(source, entry.name),
          virtualJoin(destination, entry.name),
          { recursive: true, overwrite },
        );
      }
      return;
    }

    if (!overwrite) {
      try {
        await this.filesystem.lstat(destination);
        throw fsError("EEXIST", { syscall: "copy", path: destination });
      } catch (error) {
        if (!isErrorCode(error, "ENOENT")) throw error;
      }
    }
    const sourceMode = sourceStats.mode & 0o7777;
    await this._writeBytes(destination, await this._readBytes(source), {
      createParents: true,
      overwrite,
      onCreate,
      createMode: sourceMode,
    });
    if (overwrite) await this.filesystem.chmod(destination, sourceMode);
  }

  async readFileBuffer(pathValue) {
    return this._run(() => this._readBytes(pathValue));
  }

  async readFileBytes(pathValue) {
    return this._run(async () => toByteString(await this._readBytes(pathValue)));
  }

  async readFile(pathValue, options) {
    return this._run(async () => {
      const encoding = typeof options === "string" ? options : options?.encoding;
      return fromBytes(await this._readBytes(pathValue), encoding ?? "utf8");
    });
  }

  async writeFile(pathValue, content, options) {
    return this._run(async () => {
      this._assertWritable("write", pathValue);
      const encoding = typeof options === "string" ? options : options?.encoding;
      await this._writeBytes(this._path(pathValue), toBytes(content, encoding), {
        createParents: true,
        overwrite: options?.overwrite !== false,
      });
    });
  }

  async appendFile(pathValue, content, options) {
    return this._run(async () => {
      this._assertWritable("append", pathValue);
      const encoding = typeof options === "string" ? options : options?.encoding;
      await this._appendBytes(pathValue, toBytes(content, encoding));
    });
  }

  async exists(pathValue) {
    return this._run(async () => {
      try {
        await this.filesystem.stat(this._path(pathValue));
        return true;
      } catch {
        return false;
      }
    });
  }

  async stat(pathValue) {
    return this._run(async () => statsToJustBashStats(await this._stat(pathValue)));
  }

  async lstat(pathValue) {
    return this._run(async () => statsToJustBashStats(await this._lstat(pathValue)));
  }

  async mkdir(pathValue, options) {
    return this._run(async () => {
      this._assertWritable("mkdir", pathValue);
      const directory = this._path(pathValue);
      await this.filesystem.mkdir(directory, { recursive: options?.recursive === true });
      this.pathIndex.add(directory);
    });
  }

  async readdir(pathValue) {
    return this._run(async () => (await this._entries(pathValue)).map((entry) => entry.name));
  }

  async readdirWithFileTypes(pathValue) {
    return this._run(async () => (await this._entries(pathValue)).map(statsToEntry));
  }

  async rm(pathValue, options = {}) {
    return this._run(async () => {
      this._assertWritable("rm", pathValue);
      await this._remove(pathValue, options);
    });
  }

  async rmdir(pathValue, options = {}) {
    return this._run(async () => {
      this._assertWritable("rmdir", pathValue);
      await this._remove(pathValue, { recursive: options.recursive === true, force: options.force === true });
    });
  }

  async cp(source, destination, options = {}) {
    return this._run(async () => {
      this._assertWritable("cp", destination);
      await this._copy(source, destination, options);
    });
  }

  async mv(source, destination) {
    return this._run(async () => {
      this._assertWritable("mv", destination);
      await this.filesystem.rename(this._path(source), this._path(destination));
      this.pathIndex.rename(source, destination);
      try {
        await this._lstat(source);
        this.pathIndex.add(source);
      } catch (error) {
        if (!isErrorCode(error, "ENOENT")) this.pathIndex.add(source);
        // Renaming two hardlink names to the same inode can leave both names.
      }
    });
  }

  async chmod(pathValue, mode) {
    return this._run(async () => {
      this._assertWritable("chmod", pathValue);
      await this.filesystem.chmod(this._path(pathValue), mode);
    });
  }

  async symlink(target, linkPath) {
    return this._run(async () => {
      this._assertWritable("symlink", linkPath);
      await this.filesystem.symlink(target, this._path(linkPath));
      this.pathIndex.add(linkPath);
    });
  }

  async link(existingPath, newPath) {
    return this._run(async () => {
      this._assertWritable("link", newPath);
      await this.filesystem.link(this._path(existingPath), this._path(newPath));
      this.pathIndex.add(newPath);
    });
  }

  async readlink(pathValue) {
    return this._run(() => this.filesystem.readlink(this._path(pathValue)));
  }

  async realpath(pathValue) {
    return this._run(() => resolveVirtualRealpath(this.filesystem, pathValue));
  }

  async utimes(pathValue, atime, mtime) {
    return this._run(async () => {
      this._assertWritable("utimes", pathValue);
      await this.filesystem.utimes(this._path(pathValue), atime, mtime);
    });
  }
}

export function toJustBashStats(stats) {
  return statsToJustBashStats(stats);
}

export function toJustBashDirent(entry) {
  return statsToEntry(entry);
}
