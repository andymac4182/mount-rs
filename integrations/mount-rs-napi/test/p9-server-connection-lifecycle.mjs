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

console.log("mount-rs N-API P9 native connection close idempotence: PASS")
