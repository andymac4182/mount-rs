"use strict"

// Adapted from mountx/src/harness.ts at 85361a8212ff9bff8e69f62fa8993ef2c2ec51e8.
// Copyright (c) Pooya Parsa <pooya@pi0.io>; MIT, see THIRD_PARTY_NOTICES.md.
// Keep JS identities and callback arguments at this boundary; path/error
// primitives still use the Rust core exposed by the native binding.
module.exports = function harness({ normalizePath, fsError }) {
  const encoder = new TextEncoder()

  function resolveCapabilities(driver) {
    const declared = driver.capabilities ?? {}
    const has = (name) => typeof driver[name] === "function"
    return {
      handles: declared.handles ?? false,
      atomicRename: declared.atomicRename ?? false,
      readOnly: declared.readOnly ?? false,
      durableWrites: declared.durableWrites ?? false,
      hardlinks: declared.hardlinks ?? has("link"),
      symlinks: declared.symlinks ?? (has("symlink") && has("readlink") && has("lstat")),
      permissions: declared.permissions ?? has("chmod"),
      times: declared.times ?? has("utimes"),
      truncate: declared.truncate ?? has("truncate"),
      caseSensitive: declared.caseSensitive ?? true,
      statfs: declared.statfs ?? has("statfs"),
      extensions: declared.extensions ?? Object.keys(driver.mountx ?? {}),
    }
  }

  function createLoopback(driver) {
    function use(name) {
      const method = driver[name]
      if (typeof method !== "function") {
        return () => { throw fsError("ENOSYS", { syscall: name }) }
      }
      return method.bind(driver)
    }
    const stat = use("stat")
    const lstat = use("lstat")
    const statfs = use("statfs")
    const readdir = use("readdir")
    const open = use("open")
    const mkdir = use("mkdir")
    const rmdir = use("rmdir")
    const unlink = use("unlink")
    const rename = use("rename")
    const link = use("link")
    const symlink = use("symlink")
    const readlink = use("readlink")
    const chmod = use("chmod")
    const chown = use("chown")
    const lchown = use("lchown")
    const truncate = use("truncate")
    const utimes = use("utimes")
    const lutimes = use("lutimes")
    const loopback = {
      driver,
      capabilities: resolveCapabilities(driver),
      mountx: driver.mountx,
      stat: async (path) => stat(normalizePath(path)),
      lstat: async (path) => lstat(normalizePath(path)),
      statfs: async (path) => statfs(normalizePath(path)),
      readdir: async (path, options) => readdir(normalizePath(path), options),
      open: async (path, flags, mode) => open(normalizePath(path), flags, mode),
      mkdir: async (path, options) => mkdir(normalizePath(path), options),
      rmdir: async (path) => rmdir(normalizePath(path)),
      unlink: async (path) => unlink(normalizePath(path)),
      rename: async (oldPath, newPath) => rename(normalizePath(oldPath), normalizePath(newPath)),
      link: async (existingPath, newPath) => link(normalizePath(existingPath), normalizePath(newPath)),
      symlink: async (target, path, type) => symlink(target, normalizePath(path), type),
      readlink: async (path) => readlink(normalizePath(path)),
      chmod: async (path, mode) => chmod(normalizePath(path), mode),
      chown: async (path, uid, gid) => chown(normalizePath(path), uid, gid),
      lchown: async (path, uid, gid) => lchown(normalizePath(path), uid, gid),
      truncate: async (path, length) => truncate(normalizePath(path), length),
      utimes: async (path, atime, mtime) => utimes(normalizePath(path), atime, mtime),
      lutimes: async (path, atime, mtime) => lutimes(normalizePath(path), atime, mtime),
      async readFile(path) {
        const handle = await loopback.open(path, "r")
        try {
          const chunks = []
          let total = 0
          for (;;) {
            const buffer = new Uint8Array(64 * 1024)
            const { bytesRead } = await handle.read(buffer, 0, buffer.byteLength, total)
            if (bytesRead === 0) break
            chunks.push(buffer.subarray(0, bytesRead))
            total += bytesRead
          }
          const data = new Uint8Array(total)
          let offset = 0
          for (const chunk of chunks) {
            data.set(chunk, offset)
            offset += chunk.byteLength
          }
          return data
        } finally {
          await handle.close()
        }
      },
      async writeFile(path, data) {
        const bytes = typeof data === "string" ? encoder.encode(data) : data
        const handle = await loopback.open(path, "w", 0o666)
        try {
          let written = 0
          while (written < bytes.byteLength) {
            const remaining = bytes.byteLength - written
            const { bytesWritten } = await handle.write(bytes, written, remaining, written)
            if (!Number.isInteger(bytesWritten) || bytesWritten <= 0 || bytesWritten > remaining) {
              throw fsError("EIO", { syscall: "write", path })
            }
            written += bytesWritten
          }
        } finally {
          await handle.close()
        }
      },
    }
    return loopback
  }
  return { createLoopback, resolveCapabilities }
}
