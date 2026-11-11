import assert from "node:assert/strict"
import net from "node:net"

import root from "../index.js"

const { Filesystem, createP9Server } = root

function waitUntil(predicate, label) {
  const deadline = Date.now() + 5_000
  return new Promise((resolve, reject) => {
    const poll = () => {
      if (predicate()) {
        resolve()
        return
      }
      if (Date.now() >= deadline) {
        reject(new Error(`${label} timed out`))
        return
      }
      setImmediate(poll)
    }
    poll()
  })
}

const server = createP9Server(Filesystem.memory(), { host: "127.0.0.1", port: 0 })
let socket

try {
  await server.listen()
  socket = net.createConnection({ host: "127.0.0.1", port: server.port })
  await new Promise((resolve, reject) => {
    socket.once("connect", resolve)
    socket.once("error", reject)
  })
  await waitUntil(() => server.connections === 1, "native 9P connection registration")

  const [connection] = server.clients
  assert.ok(connection)
  assert.equal(connection.isClosed, false)
  const closed = connection.closed
  const firstClose = connection.close()
  assert.strictEqual(firstClose, connection.close())
  assert.strictEqual(firstClose, connection.close())

  await Promise.all([firstClose, connection.waitClosed(), closed])
  assert.equal(connection.isClosed, true)
  assert.equal(server.connections, 0)
  assert.strictEqual(connection.close(), firstClose)
  await connection.close()
} finally {
  if (socket && !socket.destroyed) socket.destroy()
  await server.close()
}

function portIsReachable(port) {
  return new Promise((resolve) => {
    const probe = net.createConnection({ host: "127.0.0.1", port })
    probe.once("connect", () => {
      probe.destroy()
      resolve(true)
    })
    probe.once("error", () => resolve(false))
  })
}

// `listen` and `close` race in both orders while the state getters are read.
// Whichever wins, no listener is left behind once both have settled.
const racingFilesystem = Filesystem.memory()
let listened = 0
let refused = 0
try {
  for (let index = 0; index < 60; index++) {
    const racing = createP9Server(racingFilesystem, { host: "127.0.0.1", port: 0 })
    let boundPort = 0
    const startListen = () => racing.listen().then(() => {
      boundPort = racing.port
    })
    let closing
    let listening
    if (index % 3 === 0) {
      closing = racing.close()
      listening = startListen()
    } else if (index % 3 === 1) {
      listening = startListen()
      closing = racing.close()
    } else {
      listening = startListen()
      await new Promise((resolve) => setImmediate(resolve))
      closing = racing.close()
    }
    // The getters only load the published listener; none of them may throw
    // or block while it is being bound or closed.
    for (let read = 0; read < 20; read++) {
      assert.equal(typeof racing.port, "number")
      assert.equal(typeof racing.connections, "number")
      assert.ok(Array.isArray(racing.clients))
      racing.address()
    }
    const [closeResult, listenResult] = await Promise.allSettled([closing, listening])
    assert.equal(closeResult.status, "fulfilled", String(closeResult.reason))
    if (listenResult.status === "fulfilled") {
      listened++
    } else {
      refused++
      assert.match(String(listenResult.reason), /server is closed|closed while binding/)
    }
    // A close that raced a listen may still be finishing the listener the
    // listen published; a second close waits for nothing more than that.
    await racing.close()
    if (boundPort !== 0) {
      assert.equal(
        await portIsReachable(boundPort),
        false,
        `9P listener remained reachable at iteration ${index} on port ${boundPort}`,
      )
    }
    assert.equal(racing.connections, 0)
    assert.deepEqual(racing.clients, [])
  }
} finally {
  await racingFilesystem.shutdown()
}
assert.ok(listened + refused === 60)

console.log("mount-rs N-API P9 native connection close idempotence: PASS")
