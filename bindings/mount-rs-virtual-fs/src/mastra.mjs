import {
  DirectoryNotEmptyError,
  DirectoryNotFoundError,
  FileExistsError,
  FileNotFoundError,
  IsDirectoryError,
  NotDirectoryError,
  PermissionError,
  StaleFileError,
  WorkspaceReadOnlyError,
} from "@mastra/core/workspace";
import path from "node:path";

import {
  MountRsFilesystemBase,
  errorCode,
  isErrorCode,
  normalizeVirtualPath,
} from "./common.mjs";

const posix = path.posix;

function mapMastraError(error, pathValue, operation) {
  if (
    error instanceof FileNotFoundError ||
    error instanceof DirectoryNotFoundError ||
    error instanceof FileExistsError ||
    error instanceof IsDirectoryError ||
    error instanceof NotDirectoryError ||
    error instanceof DirectoryNotEmptyError ||
    error instanceof PermissionError ||
    error instanceof StaleFileError ||
    error instanceof WorkspaceReadOnlyError
  ) {
    return error;
  }
  const code = errorCode(error);
  const normalized = normalizeVirtualPath(pathValue);
  switch (code) {
    case "ENOENT":
      return operation === "readdir" || operation === "mkdir" || operation === "rmdir"
        ? new DirectoryNotFoundError(normalized)
        : new FileNotFoundError(normalized);
    case "EEXIST":
      return new FileExistsError(normalized);
    case "EISDIR":
      return new IsDirectoryError(normalized);
    case "ENOTDIR":
      return new NotDirectoryError(normalized);
    case "ENOTEMPTY":
      return new DirectoryNotEmptyError(normalized);
    case "EACCES":
    case "EPERM":
      return new PermissionError(normalized, operation);
    case "EROFS":
      return new WorkspaceReadOnlyError(operation);
    default:
      return error;
  }
}

function extensionMatches(name, extension) {
  if (!extension) return true;
  const extensions = Array.isArray(extension) ? extension : [extension];
  const actual = posix.extname(name);
  return extensions.some((value) => value === actual || value === actual.slice(1));
}

/**
 * A Mastra WorkspaceFilesystem backed by the supplied NodeRustFs Filesystem.
 * It intentionally has no basePath and no mount configuration: paths are
 * absolute inside the logical drive, never host paths.
 */
export class MountRsMastraFilesystem extends MountRsFilesystemBase {
  constructor(filesystem, options = {}) {
    super(filesystem, options);
    this.id = options.id ?? `mount-rs-${Math.random().toString(36).slice(2, 10)}`;
    this.name = options.name ?? "MountRsFilesystem";
    this.provider = "mount-rs";
    this.displayName = options.displayName;
    this.description = options.description ?? "Mount-rs logical filesystem without an OS mount";
    this.status = "ready";
    this.error = undefined;
  }

  async init() {
    if (this.status === "destroyed") throw new Error("filesystem has been destroyed");
    this.status = "ready";
  }

  async destroy() {
    if (this.status === "destroyed") return;
    this.status = "destroying";
    try {
      await this.close();
      this.status = "destroyed";
    } catch (error) {
      this.status = "error";
      this.error = error instanceof Error ? error.message : String(error);
      throw error;
    }
  }

  isReady() {
    return this.status === "ready";
  }

  getInfo() {
    return {
      id: this.id,
      name: this.name,
      provider: this.provider,
      status: this.status,
      error: this.error,
      readOnly: this.readOnly,
      metadata: { hostMount: false, backend: "NodeRustFs" },
    };
  }

  getInstructions() {
    return "This is a mount-independent mount-rs filesystem. Use absolute paths inside its logical namespace; no host filesystem path or OS mount is available.";
  }

  _assertMastraWritable(operation) {
    if (this.readOnly) throw new WorkspaceReadOnlyError(operation);
  }

  async readFile(pathValue, options) {
    try {
      const bytes = await super.readFileBuffer(pathValue);
      return options?.encoding ? Buffer.from(bytes).toString(options.encoding) : Buffer.from(bytes);
    } catch (error) {
      throw mapMastraError(error, pathValue, "readFile");
    }
  }

  async writeFile(pathValue, content, options = {}) {
    this._assertMastraWritable("writeFile");
    const normalized = normalizeVirtualPath(pathValue);
    try {
      const parent = posix.dirname(normalized);
      if (options.recursive === false) {
        try {
          const parentStats = await this._lstat(parent);
          if (!parentStats.isDirectory()) throw new NotDirectoryError(parent);
        } catch (error) {
          if (isErrorCode(error, "ENOENT")) throw new DirectoryNotFoundError(parent);
          throw error;
        }
      }
      if (options.expectedMtime) {
        try {
          const current = await this._stat(normalized);
          const actual = new Date(current.mtimeMs);
          if (actual.getTime() !== options.expectedMtime.getTime()) {
            throw new StaleFileError(normalized, options.expectedMtime, actual);
          }
        } catch (error) {
          if (!isErrorCode(error, "ENOENT")) throw error;
        }
      }
      if (options.overwrite === false) {
        try {
          await this._lstat(normalized);
          throw new FileExistsError(normalized);
        } catch (error) {
          if (!isErrorCode(error, "ENOENT")) throw error;
        }
      }
      await super.writeFile(normalized, Buffer.isBuffer(content) ? content : content, {
        overwrite: options.overwrite !== false,
      });
    } catch (error) {
      throw mapMastraError(error, normalized, "writeFile");
    }
  }

  async appendFile(pathValue, content) {
    this._assertMastraWritable("appendFile");
    try {
      await super.appendFile(pathValue, content);
    } catch (error) {
      throw mapMastraError(error, pathValue, "appendFile");
    }
  }

  async deleteFile(pathValue, options = {}) {
    this._assertMastraWritable("deleteFile");
    try {
      const stats = await this._lstat(pathValue);
      if (stats.isDirectory()) throw new IsDirectoryError(normalizeVirtualPath(pathValue));
      await super.rm(pathValue, { force: options.force === true });
    } catch (error) {
      if (options.force === true && isErrorCode(error, "ENOENT")) return;
      throw mapMastraError(error, pathValue, "deleteFile");
    }
  }

  async copyFile(source, destination, options = {}) {
    this._assertMastraWritable("copyFile");
    try {
      await super.cp(source, destination, {
        recursive: options.recursive === true,
        overwrite: options.overwrite !== false,
      });
    } catch (error) {
      throw mapMastraError(error, errorCode(error) === "EEXIST" ? destination : source, "copyFile");
    }
  }

  async moveFile(source, destination, options = {}) {
    this._assertMastraWritable("moveFile");
    const normalizedDestination = normalizeVirtualPath(destination);
    try {
      if (options.overwrite === false) {
        try {
          await this._lstat(normalizedDestination);
          throw new FileExistsError(normalizedDestination);
        } catch (error) {
          if (!isErrorCode(error, "ENOENT")) throw error;
        }
      }
      await this.filesystem.mkdir(posix.dirname(normalizedDestination), { recursive: true });
      await super.mv(source, normalizedDestination);
    } catch (error) {
      throw mapMastraError(error, errorCode(error) === "EEXIST" ? normalizedDestination : source, "moveFile");
    }
  }

  async mkdir(pathValue, options = {}) {
    this._assertMastraWritable("mkdir");
    try {
      await super.mkdir(pathValue, { recursive: options.recursive ?? true });
    } catch (error) {
      throw mapMastraError(error, pathValue, "mkdir");
    }
  }

  async rmdir(pathValue, options = {}) {
    this._assertMastraWritable("rmdir");
    try {
      const stats = await this._lstat(pathValue);
      if (!stats.isDirectory()) throw new NotDirectoryError(normalizeVirtualPath(pathValue));
      await super.rmdir(pathValue, {
        recursive: options.recursive === true,
        force: options.force === true,
      });
    } catch (error) {
      if (options.force === true && isErrorCode(error, "ENOENT")) return;
      throw mapMastraError(error, pathValue, "rmdir");
    }
  }

  async readdir(pathValue, options = {}) {
    try {
      const entries = await super.readdirWithFileTypes(pathValue);
      const result = [];
      for (const entry of entries) {
        if (!extensionMatches(entry.name, options.extension)) continue;
        const child = posix.join(normalizeVirtualPath(pathValue), entry.name);
        let type = entry.isDirectory ? "directory" : "file";
        let symlinkTarget;
        if (entry.isSymbolicLink) {
          symlinkTarget = await super.readlink(child);
          try {
            type = (await this._stat(child)).isDirectory() ? "directory" : "file";
          } catch {
            type = "file";
          }
        }
        const fileEntry = {
          name: entry.name,
          type,
          isSymlink: entry.isSymbolicLink || undefined,
          symlinkTarget,
        };
        if (type === "file" && !entry.isSymbolicLink) {
          try {
            fileEntry.size = (await this._stat(child)).size;
          } catch {
            // The entry may disappear concurrently; the directory listing is
            // still useful and Mastra permits size to be omitted.
          }
        }
        result.push(fileEntry);
      }

      if (options.recursive) {
        const nested = [];
        const maxDepth = options.maxDepth;
        if (maxDepth === undefined || maxDepth > 1) {
          for (const entry of result) {
            if (entry.type !== "directory" || entry.isSymlink) continue;
            const child = posix.join(normalizeVirtualPath(pathValue), entry.name);
            const children = await this.readdir(child, {
              ...options,
              maxDepth: maxDepth === undefined ? undefined : maxDepth - 1,
            });
            for (const nestedEntry of children) {
              nested.push({ ...nestedEntry, name: posix.join(entry.name, nestedEntry.name) });
            }
          }
        }
        return [...result, ...nested];
      }
      return result;
    } catch (error) {
      throw mapMastraError(error, pathValue, "readdir");
    }
  }

  async exists(pathValue) {
    return super.exists(pathValue);
  }

  async stat(pathValue) {
    try {
      const normalized = normalizeVirtualPath(pathValue);
      const stats = await this._stat(normalized);
      return {
        name: normalized === "/" ? "/" : posix.basename(normalized),
        path: normalized,
        type: stats.isDirectory() ? "directory" : "file",
        size: stats.size,
        createdAt: new Date(stats.birthtimeMs),
        modifiedAt: new Date(stats.mtimeMs),
      };
    } catch (error) {
      throw mapMastraError(error, pathValue, "stat");
    }
  }

  async realpath(pathValue) {
    try {
      return await super.realpath(pathValue);
    } catch (error) {
      throw mapMastraError(error, pathValue, "realpath");
    }
  }
}

export function createMastraFilesystem(filesystem, options = {}) {
  return new MountRsMastraFilesystem(filesystem, options);
}
