"use strict"

// Keep lock ownership and ordering in the native PathLock. This postlude only
// preserves the JavaScript callback's result or thrown identity across the
// native Promise<void> callback boundary; it is not a second lock.
const WRAPPED = "__mountRsUtilitiesWrapped"

function wrapMethod(prototype, name) {
  const original = prototype[name]
  if (typeof original !== "function" || original[WRAPPED]) return

  function wrapped(callback) {
    let outcome
    const guarded = async () => {
      try {
        outcome = { ok: true, value: await callback() }
      } catch (error) {
        outcome = { ok: false, error }
      }
    }

    const invoke = () => {
      try {
        return original.call(this, guarded)
      } catch (error) {
        return Promise.reject(error)
      }
    }
    // Upstream `write()` queues its run function on #tail. Deferring only the
    // native submission preserves the observable same-turn case where a read
    // called immediately after write() still enters before that writer gate.
    const scheduled =
      name === "write"
        ? new Promise((resolve) => queueMicrotask(() => resolve(invoke())))
        : invoke()
    return Promise.resolve(scheduled).then(() => {
      if (outcome === undefined) {
        throw new Error(`native PathLock.${name} completed without invoking its callback`)
      }
      if (!outcome.ok) throw outcome.error
      return outcome.value
    })
  }

  Object.defineProperty(wrapped, WRAPPED, { value: true })
  Object.defineProperty(prototype, name, {
    configurable: true,
    enumerable: false,
    writable: true,
    value: wrapped,
  })
}

module.exports = function installUtilities(binding) {
  const PathLock = binding && binding.PathLock
  if (!PathLock || !PathLock.prototype) return binding
  wrapMethod(PathLock.prototype, "read")
  wrapMethod(PathLock.prototype, "write")
  return binding
}
