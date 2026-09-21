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
} finally {
  await filesystem.shutdown()
}

assert.ok(listeningCount > 0, "the lifecycle test must exercise a real listener")
console.log("mount-rs N-API WebDAV concurrent lifecycle: PASS")
