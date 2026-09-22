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
  assert.strictEqual(connection, server.clients[0])
  assert.strictEqual(connection.session, connection.session)
  assert.strictEqual(connection.closed, connection.closed)
  const closed = connection.closed

  await connection.close()
  await closed
  await connection.waitClosed()
  await waitUntil(() => server.connections === 0, "native 9P connection removal")
  assert.deepEqual(server.clients, [])
} finally {
  if (socket && !socket.destroyed) socket.destroy()
  await server.close()
}

console.log("mount-rs N-API P9 server connection identity: PASS")
