import assert from "node:assert/strict"
import net from "node:net"
import { Duplex } from "node:stream"

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

function memoryDuplex() {
  return new Duplex({
    read() {},
    write(_chunk, _encoding, callback) {
      callback()
    },
  })
}

async function connect(server) {
  const socket = net.createConnection({ host: "127.0.0.1", port: server.port })
  await new Promise((resolve, reject) => {
    socket.once("connect", resolve)
    socket.once("error", reject)
  })
  await waitUntil(() => server.connections >= 1, "native 9P connection registration")
  return socket
}

const server = createP9Server(Filesystem.memory(), { host: "127.0.0.1", port: 0 })
let firstSocket
let secondSocket
let firstNative
let secondNative
let firstAttached
let secondAttached

try {
  await server.listen()

  // Native first: attach() must append after an already accepted native peer.
  firstSocket = await connect(server)
  firstNative = server.clients[0]
  assert.ok(firstNative)
  firstAttached = server.attach(memoryDuplex(), { own: false, peer: "order-native-first" })
  assert.deepEqual(server.clients, [firstNative, firstAttached])

  await firstNative.close()
  await firstNative.waitClosed()
  await firstAttached.close()
  await firstAttached.waitClosed()
  await waitUntil(() => server.connections === 0, "native-first connection cleanup")
  assert.deepEqual(server.clients, [])

  // Attached first: a native peer accepted later must append after it.
  secondAttached = server.attach(memoryDuplex(), { own: false, peer: "order-attached-first" })
  assert.deepEqual(server.clients, [secondAttached])
  secondSocket = await connect(server)
  await waitUntil(() => server.connections === 2, "attached-first connection registration")
  secondNative = server.clients.find((connection) => connection !== secondAttached)
  assert.ok(secondNative)
  assert.deepEqual(server.clients, [secondAttached, secondNative])
} finally {
  if (firstSocket && !firstSocket.destroyed) firstSocket.destroy()
  if (secondSocket && !secondSocket.destroyed) secondSocket.destroy()
  await server.close()
}

console.log("mount-rs N-API P9 mixed client arrival order: PASS")
