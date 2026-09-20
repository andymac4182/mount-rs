"use strict"

// napi-rs can only construct an Error from a status/reason pair. The native
// crate encodes the core FsError fields in a private, hex-safe reason; this
// package-level postlude restores the observable node:fs error shape without
// making the Rust crate depend on a JSON or JS-runtime bridge.
const ERROR_MARKER = "__mount_rs_error_v1__"
const RANGE_ERROR_MARKER = "__mount_rs_range_error_v1__"

function decode(value) {
  if (value === "-") return undefined
  return Buffer.from(value, "hex").toString("utf8")
}

function replaceStack(error, oldMessage, newMessage) {
  if (typeof error.stack === "string") {
    error.stack = error.stack.replace(oldMessage, newMessage)
  }
  return error
}

function structuredError(error) {
  if (!error || typeof error.message !== "string") return error
  const fields = error.message.split("|")
  if (fields[0] === ERROR_MARKER && fields.length === 7) {
    const [, code, errno, syscall, path, dest, message] = fields
    const oldMessage = error.message
    const decodedMessage = decode(message)
    // Preserve node:fs's useful property order as far as the N-API-created
    // Error permits: errno precedes code, then optional context fields.
    delete error.code
    error.errno = Number(errno)
    error.code = code
    error.message = decodedMessage
    const decodedSyscall = decode(syscall)
    const decodedPath = decode(path)
    const decodedDest = decode(dest)
    if (decodedSyscall !== undefined) error.syscall = decodedSyscall
    if (decodedPath !== undefined) error.path = decodedPath
    if (decodedDest !== undefined) error.dest = decodedDest
    return replaceStack(error, oldMessage, decodedMessage)
  }

  if (fields[0] === RANGE_ERROR_MARKER && fields.length === 4) {
    const [, name, expected, value] = fields
    const message = `The value of "${decode(name)}" is out of range. It must be ${decode(expected)}. Received ${decode(value)}`
    const result = new RangeError(message)
    result.code = "ERR_OUT_OF_RANGE"
    return result
  }
  return error
}

function wrapAsync(prototype, name, transform = (args) => args, resultTransform = (value) => value) {
  const original = prototype && prototype[name]
  if (typeof original !== "function" || original.__mountRsWrapped) return
  function wrapped(...args) {
    let result
    try {
      result = original.apply(this, transform(args))
    } catch (error) {
      throw structuredError(error)
    }
    return Promise.resolve(result).then(resultTransform).catch((error) => {
      throw structuredError(error)
    })
  }
  Object.defineProperty(wrapped, "__mountRsWrapped", { value: true })
  Object.defineProperty(prototype, name, {
    configurable: true,
    enumerable: false,
    writable: true,
    value: wrapped,
  })
}

function timeArgument(value) {
  return value instanceof Date ? value.getTime() / 1000 : value
}

module.exports = function install(binding) {
  const Filesystem = binding.Filesystem
  const FileHandle = binding.FileHandle
  const Mountx = binding.JsMountx
  if (!Filesystem) return binding

  // napi-rs defines static factory properties as non-writable. Instance
  // operations are wrapped below; the facade at the end adapts factory
  // failures without changing native instance prototype behavior.
  for (const name of [
    "stat",
    "lstat",
    "statfs",
    "readdir",
    "open",
    "readFile",
    "writeFile",
    "rmdir",
    "unlink",
    "rename",
    "link",
    "symlink",
    "readlink",
    "chmod",
    "chown",
    "lchown",
    "truncate",
    "mknod",
  ]) {
    wrapAsync(Filesystem.prototype, name)
  }
  // napi-rs represents Rust Option<T> as null, while mountx's FsDriver
  // contract uses undefined for an existing/non-recursive mkdir result.
  wrapAsync(Filesystem.prototype, "mkdir", undefined, (value) =>
    value === null ? undefined : value,
  )
  wrapAsync(Filesystem.prototype, "utimes", (args) => [
    args[0],
    timeArgument(args[1]),
    timeArgument(args[2]),
  ])
  wrapAsync(Filesystem.prototype, "lutimes", (args) => [
    args[0],
    timeArgument(args[1]),
    timeArgument(args[2]),
  ])

  if (FileHandle) {
    for (const name of ["read", "write", "stat", "truncate", "sync", "datasync", "close"]) {
      wrapAsync(FileHandle.prototype, name)
    }
  }
  if (Mountx) {
    wrapAsync(Mountx.prototype, "mknod")
  }

  // Static N-API factory properties cannot be replaced in place. A small JS
  // facade adapts backend-open failures just like instance operations. Its
  // custom instanceof hook keeps native factory results recognizable as
  // Filesystem instances even though the static facade is a separate class.
  class FilesystemFacade {
    static memory() {
      return Filesystem.memory()
    }

    static sqlite(path) {
      return Filesystem.sqlite(path).catch((error) => {
        throw structuredError(error)
      })
    }

    static pglite(connectionString) {
      return Filesystem.pglite(connectionString).catch((error) => {
        throw structuredError(error)
      })
    }

    static r2(options) {
      return Filesystem.r2(options).catch((error) => {
        throw structuredError(error)
      })
    }
  }
  // Preserve prototype introspection for consumers that inspect the exported
  // constructor (`Filesystem.prototype.stat`, etc.) even though the static
  // factory properties require a facade.
  Object.setPrototypeOf(FilesystemFacade.prototype, Filesystem.prototype)
  Object.defineProperty(FilesystemFacade, Symbol.hasInstance, {
    value: (value) => value instanceof Filesystem,
  })
  Object.defineProperty(FilesystemFacade, "name", { value: "Filesystem" })
  binding.Filesystem = FilesystemFacade
  return binding
}
