"use strict"

const calls = []
const NativeClass = class {}
let nextError

module.exports = new Proxy(
  {
    __napiBindingTarget: "storage-benchmark-capture",
    calls,
    async createChunkedDriver(options) {
      calls.push(options)
      if (nextError) {
        const error = nextError
        nextError = undefined
        throw error
      }
      return { async shutdown() {} }
    },
    failNext(error) {
      nextError = error
    },
    errnoCodes() {
      return {}
    },
    fsError(code) {
      return Object.assign(new Error(code), { code })
    },
    joinPathParts(parts) {
      return parts.join("/")
    },
    normalizePath(path) {
      return path
    },
  },
  {
    get(target, property) {
      if (property in target) return target[property]
      if (typeof property === "string" && /^[A-Z]/u.test(property)) return NativeClass
      return () => undefined
    },
  },
)
