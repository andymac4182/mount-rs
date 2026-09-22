import assert from "node:assert/strict"
import net from "node:net"

import { Filesystem, createWebdavServer } from "../index.js"

function portIsReachable(port) {
  return new Promise((resolve) => {
    const socket = net.createConnection({ host: "127.0.0.1", port })
    socket.once("connect", () => {
      socket.destroy()
      resolve(true)
    })
    socket.once("error", () => resolve(false))
  })
}

async function waitUntil(predicate, label) {
  const deadline = Date.now() + 1000
  while (!predicate()) {
    if (Date.now() >= deadline) throw new Error(`${label} timed out`)
    await new Promise((resolve) => setImmediate(resolve))
  }
}

const filesystem = Filesystem.memory()
let listeningCount = 0

try {
  for (let index = 0; index < 40; index++) {
    const server = createWebdavServer(filesystem, {
      host: "127.0.0.1",
      port: 0,
    })
    const events = []
    const startListen = () => server.listen().then(
      () => {
        events.push("listen")
        listeningCount++
        return server
      },
      (error) => {
        events.push("listen-error")
        throw error
      },
    )
    const startClose = () => server.close().then(() => {
      events.push("close")
    })
    let closing
    let listening
    if (index % 2 === 0) {
      closing = startClose()
      listening = startListen()
    } else {
      listening = startListen()
      closing = startClose()
    }
    const [closeResult, listenResult] = await Promise.allSettled([closing, listening])

    assert.equal(closeResult.status, "fulfilled")
    if (listenResult.status === "fulfilled") {
      assert.ok(events.includes("listen"))
      assert.equal(
        await portIsReachable(server.port),
        false,
        `WebDAV listener remained reachable at iteration ${index} on port ${server.port} (${events.join(",")})`,
      )
    } else {
      assert.match(String(listenResult.reason), /server is closed/)
    }
  }

  const stalledServer = createWebdavServer(filesystem, {
    host: "127.0.0.1",
    port: 0,
    drainTimeout: 25,
  })
  let stalledSocket
  try {
    await stalledServer.listen()
    stalledSocket = await new Promise((resolve, reject) => {
      const socket = net.createConnection({ host: "127.0.0.1", port: stalledServer.port })
      socket.once("connect", () => resolve(socket))
      socket.once("error", reject)
    })
    stalledSocket.write(
      `PUT /stalled HTTP/1.1\r\nHost: 127.0.0.1:${stalledServer.port}\r\n` +
      "Content-Length: 4\r\nConnection: keep-alive\r\n\r\nx",
    )
    await waitUntil(() => stalledServer.connections > 0, "WebDAV N-API stalled connection")
    await waitUntil(
      () => stalledServer.session.stats.requests > 0,
      "WebDAV N-API stalled request dispatch",
    )

    const firstClose = stalledServer.close()
    assert.strictEqual(firstClose, stalledServer.close(), "failed close is cached while in flight")
    await assert.rejects(firstClose, /WebDAV close failed.*timed out/i)
    await waitUntil(
      () => stalledServer.connections === 0,
      "WebDAV N-API forced stalled connection cancellation",
    )

    const socketClosed = stalledSocket.destroyed
      ? Promise.resolve()
      : new Promise((resolve) => stalledSocket.once("close", resolve))
    stalledSocket.destroy()
    await socketClosed
    stalledSocket = undefined
    await waitUntil(
      () => stalledServer.connections === 0,
      "WebDAV N-API stalled connection drain",
    )
    await stalledServer.close()
  } finally {
    if (stalledSocket && !stalledSocket.destroyed) stalledSocket.destroy()
    await stalledServer.close().catch(() => {})
  }
} finally {
  await filesystem.shutdown()
}

assert.ok(listeningCount > 0, "the lifecycle test must exercise a real listener")
console.log("mount-rs N-API WebDAV concurrent lifecycle: PASS")
