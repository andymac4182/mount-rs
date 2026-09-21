"use strict"

// The native methods deliberately keep their small Promise<void> N-API ABI.
// This shared facade supplies the upstream server contract at the JavaScript
// boundary: one cached listen promise, one cached close promise, the original
// server as the listen result, and AsyncDisposable teardown.
const SERVER_WRAPPED = Symbol("mountRsServerLifecycleWrapped")
const CONNECTION_WRAPPED = Symbol("mountRsConnectionLifecycleWrapped")
const P9_SERVER_WRAPPED = Symbol("mountRsP9ServerWrapped")
const SERVER_STATE = new WeakMap()
const CONNECTION_STATE = new WeakMap()
const FACTORIES_WRAPPED = Symbol("mountRsStructuralFactoriesWrapped")
const S3_STREAM_WRAPPED = Symbol("mountRsS3StreamWrapped")
const WEBDAV_STREAM_WRAPPED = Symbol("mountRsWebdavStreamWrapped")

function installStructuralFactories(binding) {
  if (binding[FACTORIES_WRAPPED]) return
  Object.defineProperty(binding, FACTORIES_WRAPPED, { value: true })
  const mounts = new Map()
  async function releaseUnmounted() {
    for (const [mounted, release] of mounts) {
      if (!mounted.active) {
        await release()
        mounts.delete(mounted)
      }
    }
  }
  // liveMounts returns new wrappers for the same native lifecycle. Reconcile
  // owned adapters after unmount through any of those wrappers too.
  const nativeUnmount = binding.Mounted.prototype.unmount
  binding.Mounted.prototype.unmount = async function (...args) {
    try {
      return await nativeUnmount.apply(this, args)
    } finally {
      await releaseUnmounted()
    }
  }

  function inputs(source, buckets = false) {
    const owned = []
    const adapters = new Map()
    function adapt(driver) {
      if (driver instanceof binding.Filesystem) return driver
      if (!adapters.has(driver)) {
        const adapter = binding.createDriver(driver)
        adapters.set(driver, adapter)
        owned.push(adapter)
      }
      return adapters.get(driver)
    }
    const release = () => Promise.all(owned.map((driver) => driver.shutdown()))
    try {
      const value = buckets && source && typeof source === "object" && !(source instanceof binding.Filesystem) &&
        typeof source.stat !== "function" && "buckets" in source
        ? { ...source, buckets: Object.fromEntries(Object.entries(source.buckets).map(
          ([name, driver]) => [name, adapt(driver)],
        )) }
        : adapt(source)
      return { value, release, owned }
    } catch (error) {
      void release().catch(() => {})
      throw error
    }
  }

  for (const name of ["createNfsServer", "createP9Server", "createS3Server", "createWebdavServer"]) {
    const factory = binding[name]
    binding[name] = function (source, ...args) {
      const { value, release, owned } = inputs(source, name === "createS3Server")
      try {
        const server = factory(value, ...args)
        if (name === "createP9Server") {
          serverState(server).p9Options = args[0] ?? {}
        }
        if (owned.length) serverState(server).release = release
        return server
      } catch (error) {
        void release().catch(() => {})
        throw error
      }
    }
  }

  const nativeMount = binding.mount
  binding.mount = async function (driver, ...args) {
    const { value, release, owned } = inputs(driver)
    try {
      const mounted = await nativeMount(value, ...args)
      if (owned.length) {
        mounts.set(mounted, release)
      }
      return mounted
    } catch (error) {
      await release()
      throw error
    }
  }
  const unmountAll = binding.unmountAll
  binding.unmountAll = async function (...args) {
    try {
      return await unmountAll(...args)
    } finally {
      await releaseUnmounted()
    }
  }
}

function p9TransportError(state, error, peer) {
  const callback = state.p9Options && state.p9Options.onTransportError
  if (typeof callback !== "function") return
  try {
    callback(error, peer)
  } catch {
    // Transport notifications are observational. A callback exception must
    // not become an uncaught exception or leave the stream lifecycle wedged.
  }
}

function p9PeerOf(stream, fallback) {
  if (typeof stream.remoteAddress === "string") {
    return stream.remotePort === undefined
      ? stream.remoteAddress
      : `${stream.remoteAddress}:${stream.remotePort}`
  }
  return fallback
}

function p9ErrorCode(error) {
  return error && typeof error === "object" ? error.code : undefined
}

class AttachedP9Connection {
  constructor(state, stream, options, session, id) {
    this._state = state
    this.stream = stream
    this.session = session
    this.peer = options.peer ?? p9PeerOf(stream, undefined)
    this.id = id
    this._own = options.own === undefined
      ? typeof stream.destroySoon === "function"
      : Boolean(options.own)
    this._maxFrame = Number.isSafeInteger(options.maxFrame) && options.maxFrame > 0
      ? options.maxFrame
      : 1024 * 1024
    this._maxInFlight = Math.max(
      1,
      Number.isSafeInteger(options.maxInFlight) ? options.maxInFlight : 16,
    )
    this._input = Buffer.alloc(0)
    this._frames = []
    this._inflight = 0
    this._paused = false
    this._stopped = false
    this._ending = undefined
    this._writeChain = Promise.resolve()
    this._writeRejectors = new Set()
    this._streamClosed = stream.destroyed
      ? Promise.resolve()
      : new Promise((resolve) => stream.once("close", resolve))
    this._closedResolve = undefined
    this._closed = new Promise((resolve) => {
      this._closedResolve = resolve
    })

    stream.on("data", (chunk) => this._feed(chunk))
    stream.on("end", () => { void this.close() })
    stream.on("close", () => { void this.close() })
    stream.on("error", (error) => {
      const code = p9ErrorCode(error)
      if (code !== "ECONNRESET" && code !== "EPIPE") {
        p9TransportError(this._state, error, this.peer)
      }
      void this.close()
    })
  }

  get closed() {
    return this._closed
  }

  get isClosed() {
    return this._stopped
  }

  close() {
    this._ending ??= this._finish(false)
    return this._ending
  }

  _drop() {
    this._ending ??= this._finish(true)
    return this._ending
  }

  _limit() {
    const negotiated = this.session.msize
    return Number.isInteger(negotiated) && negotiated > 0 ? negotiated : this._maxFrame
  }

  _feed(chunk) {
    if (this._stopped) return
    let bytes
    try {
      bytes = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk)
    } catch (error) {
      p9TransportError(this._state, error, this.peer)
      void this.close()
      return
    }
    this._input = this._input.length === 0 ? bytes : Buffer.concat([this._input, bytes])
    this._drainInput()
  }

  _drainInput() {
    if (this._stopped) return
    while (this._input.length >= 4 && this._frames.length + this._inflight < this._maxInFlight) {
      const size = this._input.readUInt32LE(0)
      if (size < 7 || size > this._limit()) {
        const error = new Error(`invalid 9P frame size ${size}; expected 7..${this._limit()}`)
        p9TransportError(this._state, error, this.peer)
        void this.close()
        return
      }
      if (this._input.length < size) break
      this._frames.push(Buffer.from(this._input.subarray(0, size)))
      this._input = this._input.subarray(size)
    }
    if (this._frames.length + this._inflight >= this._maxInFlight) {
      if (!this._paused && typeof this.stream.pause === "function") {
        this.stream.pause()
        this._paused = true
      }
    }
    this._pump()
  }

  _pump() {
    while (!this._stopped && this._frames.length > 0 && this._inflight < this._maxInFlight) {
      const frame = this._frames.shift()
      this._inflight += 1
      Promise.resolve()
        .then(() => this.session.handleCall(frame))
        .then((reply) => {
          if (reply !== null && reply !== undefined && !this._stopped) {
            this._writeChain = this._writeChain
              .then(() => this._writeFrame(reply))
              .catch((error) => {
                p9TransportError(this._state, error, this.peer)
                void this.close()
              })
            return this._writeChain
          }
        })
        .catch((error) => {
          p9TransportError(this._state, error, this.peer)
          void this.close()
        })
        .finally(() => {
          this._inflight -= 1
          if (!this._stopped && this._paused && this._frames.length + this._inflight < this._maxInFlight) {
            this._paused = false
            if (typeof this.stream.resume === "function") this.stream.resume()
          }
          this._drainInput()
        })
    }
  }

  _writeFrame(frame) {
    if (this._stopped) return Promise.resolve()
    return new Promise((resolve, reject) => {
      let callbackDone = false
      let drained = true
      let writeReturned = false
      let settled = false
      const finish = (error) => {
        if (settled) return
        if (error) {
          settled = true
          this._writeRejectors.delete(reject)
          reject(error)
          return
        }
        if (!writeReturned || !callbackDone || !drained) return
        settled = true
        this._writeRejectors.delete(reject)
        resolve()
      }
      this._writeRejectors.add(reject)
      const onDrain = () => {
        drained = true
        finish()
      }
      try {
        const accepted = this.stream.write(frame, (error) => {
          if (error) {
            finish(error)
            return
          }
          callbackDone = true
          finish()
        })
        writeReturned = true
        if (!accepted) {
          drained = false
          this.stream.once("drain", onDrain)
        }
        finish()
      } catch (error) {
        finish(error)
      }
    })
  }

  async _finish(hard) {
    if (this._stopped) return this._closed
    this._stopped = true
    this._input = Buffer.alloc(0)
    this._frames.length = 0
    if (this._paused && typeof this.stream.resume === "function") {
      this._paused = false
      this.stream.resume()
    }
    for (const reject of this._writeRejectors) {
      reject(new Error("9P connection closed"))
    }
    this._writeRejectors.clear()
    if (hard && this._own && !this.stream.destroyed && typeof this.stream.destroy === "function") {
      this.stream.destroy()
    } else if (!this.stream.destroyed && typeof this.stream.end === "function") {
      try { this.stream.end() } catch { /* stream teardown is already terminal */ }
    }
    try {
      await this.session.destroy()
    } catch (error) {
      p9TransportError(this._state, error, this.peer)
    }
    if (this._own && !this.stream.destroyed && typeof this.stream.destroy === "function") {
      this.stream.destroy()
    }
    if (this._own) await this._streamClosed
    this._state.attachments.delete(this)
    this._state.attachedStreams.delete(this.stream)
    this._closedResolve()
    return this._closed
  }
}

function wrapP9Server(P9Server) {
  if (!P9Server || !P9Server.prototype || P9Server.prototype[P9_SERVER_WRAPPED]) return
  const prototype = P9Server.prototype
  const nativeClients = prototype.clients
  const nativeConnections = Object.getOwnPropertyDescriptor(prototype, "connections")
  if (typeof nativeClients !== "function") return

  function stateFor(server) {
    const state = serverState(server)
    state.attachments ??= new Set()
    state.attachedStreams ??= new Map()
    state.nextAttachedId ??= 1_000_000_000_000
    return state
  }

  Object.defineProperty(prototype, "attach", {
    configurable: true,
    enumerable: false,
    writable: true,
    value(stream, options = {}) {
      const state = stateFor(this)
      if (state.close !== undefined) throw new Error("mount-rs: this 9P server is closed")
      if (!stream || typeof stream.on !== "function" || typeof stream.write !== "function") {
        throw new TypeError("mount-rs: 9P attach requires a Node Duplex stream")
      }
      if (state.attachedStreams.has(stream)) {
        throw new Error("mount-rs: that stream is already attached to this 9P server")
      }
      const sessionFactory = this._createAttachedSession
      if (typeof sessionFactory !== "function") {
        throw new Error("mount-rs: 9P attached streams are unavailable in this native build")
      }
      const session = sessionFactory.call(this)
      const connection = new AttachedP9Connection(
        state,
        stream,
        { ...(state.p9Options ?? {}), ...(options ?? {}) },
        session,
        state.nextAttachedId++,
      )
      state.attachedStreams.set(stream, connection)
      state.attachments.add(connection)
      return connection
    },
  })

  Object.defineProperty(prototype, "clients", {
    configurable: true,
    enumerable: false,
    writable: true,
    value() {
      const state = stateFor(this)
      return [...nativeClients.call(this), ...state.attachments]
    },
  })

  if (nativeConnections && typeof nativeConnections.get === "function") {
    Object.defineProperty(prototype, "connections", {
      configurable: true,
      enumerable: false,
      get() {
        const state = stateFor(this)
        return nativeConnections.get.call(this) + state.attachments.size
      },
    })
  }
  Object.defineProperty(prototype, P9_SERVER_WRAPPED, { value: true })
}

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
      async () => {
        if (state.attachments && state.attachments.size > 0) {
          await Promise.all([...state.attachments].map((connection) => connection._drop()))
        }
        return nativeClose.call(this)
      },
      async () => {
        if (state.release) {
          await state.release()
          state.release = undefined
        }
      },
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
  if (!Object.getOwnPropertyDescriptor(prototype, "stream")) {
    Object.defineProperty(prototype, "stream", {
      configurable: true,
      enumerable: false,
      get() {
        return undefined
      },
    })
  }
  Object.defineProperty(prototype, CONNECTION_WRAPPED, { value: true })
}

function wrapNfsConnection(NfsConnection) {
  if (!NfsConnection || !NfsConnection.prototype || NfsConnection.prototype[CONNECTION_WRAPPED]) {
    return
  }
  const prototype = NfsConnection.prototype
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

function toReadableStream(value, label = "S3") {
  const ReadableStreamConstructor = globalThis.ReadableStream
  if (typeof ReadableStreamConstructor !== "function") {
    throw new TypeError(`${label} streaming requires globalThis.ReadableStream`)
  }
  if (value === null || value === undefined) {
    return new ReadableStreamConstructor({
      start(controller) {
        controller.close()
      },
    })
  }
  if (typeof value.getReader === "function") return value
  const asyncIterator = value[Symbol.asyncIterator]
  if (typeof asyncIterator !== "function") {
    throw new TypeError(`${label} request body must be an AsyncIterable or ReadableStream`)
  }
  const iterator = asyncIterator.call(value)
  return new ReadableStreamConstructor({
    async pull(controller) {
      const next = await iterator.next()
      if (next.done) {
        controller.close()
        return
      }
      if (!(next.value instanceof Uint8Array)) {
        throw new TypeError("S3 request body chunks must be Uint8Array values")
      }
      controller.enqueue(Buffer.from(next.value))
    },
    async cancel(reason) {
      if (typeof iterator.return === "function") await iterator.return(reason)
    },
  })
}

function bodyAsyncIterator(nativeBody) {
  let done = false
  return {
    async next() {
      if (done) return { value: undefined, done: true }
      try {
        const chunk = await nativeBody.readChunk()
        if (chunk === null) {
          done = true
          return { value: undefined, done: true }
        }
        return { value: chunk, done: false }
      } catch (error) {
        done = true
        throw error
      }
    },
    async return(value) {
      if (!done) {
        done = true
        await nativeBody.close()
      }
      return { value, done: true }
    },
    async throw(error) {
      await this.return()
      throw error
    },
    [Symbol.asyncIterator]() {
      return this
    },
  }
}

function wrapS3Session(S3Session) {
  if (!S3Session || !S3Session.prototype || S3Session.prototype[S3_STREAM_WRAPPED]) {
    return
  }
  const prototype = S3Session.prototype
  const nativeHandleRequestStream = prototype.handleRequestStream
  if (typeof nativeHandleRequestStream !== "function") return
  Object.defineProperty(prototype, "handleRequestStream", {
    configurable: true,
    enumerable: false,
    writable: true,
    value(head, body) {
      try {
        const request = nativeHandleRequestStream.call(this, head, toReadableStream(body))
        return Promise.resolve(request.run()).then((response) => {
          if (response === null || response.body === null || response.body === undefined) {
            return response
          }
          return { status: response.status, headers: response.headers, body: bodyAsyncIterator(response.body) }
        })
      } catch (error) {
        return Promise.reject(error)
      }
    },
  })
  Object.defineProperty(prototype, S3_STREAM_WRAPPED, { value: true })
}

function wrapWebdavSession(WebdavSession) {
  if (!WebdavSession || !WebdavSession.prototype || WebdavSession.prototype[WEBDAV_STREAM_WRAPPED]) {
    return
  }
  const prototype = WebdavSession.prototype
  const nativeHandleRequestStream = prototype.handleRequestStream
  if (typeof nativeHandleRequestStream !== "function") return
  Object.defineProperty(prototype, "handleRequestStream", {
    configurable: true,
    enumerable: false,
    writable: true,
    value(head, body) {
      try {
        const request = nativeHandleRequestStream.call(this, head, toReadableStream(body, "WebDAV"))
        return Promise.resolve(request.run()).then((response) => {
          if (response === null || response.body === null || response.body === undefined) {
            return response
          }
          return { status: response.status, headers: response.headers, body: bodyAsyncIterator(response.body) }
        })
      } catch (error) {
        return Promise.reject(error)
      }
    },
  })
  Object.defineProperty(prototype, WEBDAV_STREAM_WRAPPED, { value: true })
}

module.exports = function installServers(binding) {
  wrapP9Server(binding && binding.P9Server)
  for (const name of ["NfsServer", "P9Server", "S3Server", "WebdavServer"]) {
    wrapServer(binding && binding[name])
  }
  wrapP9Connection(binding && binding.P9Connection)
  wrapNfsConnection(binding && binding.NfsConnection)
  wrapS3Session(binding && binding.S3Session)
  wrapWebdavSession(binding && binding.WebdavSession)
  installStructuralFactories(binding)
  return binding
}
