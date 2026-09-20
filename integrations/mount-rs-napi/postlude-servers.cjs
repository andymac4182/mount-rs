"use strict"

// The native methods deliberately keep their small Promise<void> N-API ABI.
// This shared facade supplies the upstream server contract at the JavaScript
// boundary: one cached listen promise, one cached close promise, the original
// server as the listen result, and AsyncDisposable teardown.
const SERVER_WRAPPED = Symbol("mountRsServerLifecycleWrapped")
const CONNECTION_WRAPPED = Symbol("mountRsConnectionLifecycleWrapped")
const SERVER_STATE = new WeakMap()
const CONNECTION_STATE = new WeakMap()

function serverState(server) {
  let state = SERVER_STATE.get(server)
  if (state === undefined) {
    state = {}
    SERVER_STATE.set(server, state)
  }
  return state
}

function connectionState(connection) {
  let state = CONNECTION_STATE.get(connection)
  if (state === undefined) {
    state = {}
    CONNECTION_STATE.set(connection, state)
  }
  return state
}

function cachedPromise(invoke, transform) {
  try {
    return Promise.resolve(invoke()).then(transform)
  } catch (error) {
    return Promise.reject(error)
  }
}

function wrapServer(Server) {
  if (!Server || !Server.prototype || Server.prototype[SERVER_WRAPPED]) return

  const prototype = Server.prototype
  const nativeListen = prototype.listen
  const nativeClose = prototype.close
  if (typeof nativeListen !== "function" || typeof nativeClose !== "function") return

  function listen() {
    const state = serverState(this)
    if (state.listen !== undefined) return state.listen
    state.listen = cachedPromise(
      () => nativeListen.call(this),
      () => this,
    )
    return state.listen
  }

  function close() {
    const state = serverState(this)
    if (state.close !== undefined) return state.close
    state.close = cachedPromise(
      () => nativeClose.call(this),
      () => undefined,
    )
    return state.close
  }

  Object.defineProperty(prototype, "listen", {
    configurable: true,
    enumerable: false,
    writable: true,
    value: listen,
  })
  Object.defineProperty(prototype, "close", {
    configurable: true,
    enumerable: false,
    writable: true,
    value: close,
  })
  if (typeof Symbol.asyncDispose === "symbol") {
    Object.defineProperty(prototype, Symbol.asyncDispose, {
      configurable: true,
      enumerable: false,
      writable: true,
      value() {
        return this.close()
      },
    })
  }
  Object.defineProperty(prototype, SERVER_WRAPPED, { value: true })
}

function wrapP9Connection(P9Connection) {
  if (!P9Connection || !P9Connection.prototype || P9Connection.prototype[CONNECTION_WRAPPED]) {
    return
  }
  const prototype = P9Connection.prototype
  if (typeof prototype.waitClosed !== "function") return

  const nativeSession = Object.getOwnPropertyDescriptor(prototype, "session")
  if (nativeSession && typeof nativeSession.get === "function") {
    Object.defineProperty(prototype, "session", {
      configurable: true,
      enumerable: false,
      get() {
        const state = connectionState(this)
        if (state.session === undefined) {
          state.session = nativeSession.get.call(this)
        }
        return state.session
      },
    })
  }

  Object.defineProperty(prototype, "closed", {
    configurable: true,
    enumerable: false,
    get() {
      const state = connectionState(this)
      if (state.closed === undefined) {
        state.closed = cachedPromise(
          () => this.waitClosed(),
          () => undefined,
        )
      }
      return state.closed
    },
  })
  Object.defineProperty(prototype, CONNECTION_WRAPPED, { value: true })
}

module.exports = function installServers(binding) {
  for (const name of ["NfsServer", "P9Server", "S3Server", "WebdavServer"]) {
    wrapServer(binding && binding[name])
  }
  wrapP9Connection(binding && binding.P9Connection)
  return binding
}
