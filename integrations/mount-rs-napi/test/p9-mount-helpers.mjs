import assert from "node:assert/strict"
import { createRequire } from "node:module"
import {
  P9_DEFAULT_MOUNT_MSIZE,
  P9_MAX_MOUNT_MSIZE,
  P9_UNIX_PATH_MAX,
  live9pMounts,
  mount9p,
  p9ClientProbe,
  p9MountOptions,
  p9Platform,
  socketPathRefusal,
  tcpSourceRefusal,
  unmountAll9p,
} from "@mount-rs/core/9p"

assert.equal(P9_DEFAULT_MOUNT_MSIZE, 128 * 1024 + 24)
assert.equal(P9_MAX_MOUNT_MSIZE, 1024 * 1024)
assert.equal(P9_UNIX_PATH_MAX, 108)
assert.equal(
  p9MountOptions({ trans: "unix" }),
  "trans=unix,version=9p2000.L,msize=131096,access=client,cache=none,uname=nobody,aname=/",
)
assert.equal(
  p9MountOptions(
    { trans: "tcp", port: 4567 },
    {
      mountMsize: 2_000_000,
      access: "client",
      cache: "loose",
      uname: "alice",
      aname: "/srv",
      readOnly: true,
      mountOptions: ["debug", "cache=none"],
    },
  ),
  "trans=tcp,port=4567,version=9p2000.L,msize=1048576,access=client,cache=loose,uname=alice,aname=/srv,ro,debug,cache=none",
)
assert.equal(p9MountOptions({ trans: "unix" }, { mountMsize: 1 }),
  "trans=unix,version=9p2000.L,msize=4096,access=client,cache=none,uname=nobody,aname=/")
assert.equal(p9MountOptions({ trans: "unix" }, { mountMsize: 1.5 }),
  "trans=unix,version=9p2000.L,msize=4096,access=client,cache=none,uname=nobody,aname=/")
assert.equal(p9MountOptions({ trans: "unix" }, { mountMsize: Number.POSITIVE_INFINITY }),
  "trans=unix,version=9p2000.L,msize=1048576,access=client,cache=none,uname=nobody,aname=/")
assert.equal(p9MountOptions({ trans: "unix" }, { mountMsize: Number.NaN }),
  "trans=unix,version=9p2000.L,msize=131096,access=client,cache=none,uname=nobody,aname=/")
assert.throws(() => p9MountOptions({ trans: "tcp", port: 0 }), /port/)
assert.throws(() => p9MountOptions({ trans: "tcp", port: 65_536 }), /port/)
assert.throws(() => p9MountOptions({ trans: "unix" }, { access: "client,ro" }), /comma/)
assert.equal(socketPathRefusal("a".repeat(P9_UNIX_PATH_MAX - 1)), undefined)
assert.match(socketPathRefusal("a".repeat(P9_UNIX_PATH_MAX)), /108/)
assert.equal(tcpSourceRefusal("127.0.0.1"), undefined)
assert.equal(tcpSourceRefusal("127.0.0.256")?.includes("dotted-quad"), true)
assert.equal(tcpSourceRefusal("localhost")?.includes("dotted-quad"), true)

const require = createRequire(import.meta.url)
const p9Module = require("@mount-rs/core/9p")
const nativeMount = p9Module.mount
const nativeUnmountAll = p9Module.unmountAll
const signalCounts = {
  SIGINT: process.listenerCount("SIGINT"),
  SIGTERM: process.listenerCount("SIGTERM"),
}
let resolveClosed
const closed = new Promise((resolve) => { resolveClosed = resolve })
const fakeMounted = { closed }
p9Module.mount = async () => fakeMounted
try {
  assert.equal(await p9Module.mount9p({}, "/tmp/mount-rs-p9-signal-test"), fakeMounted)
  assert.equal(process.listenerCount("SIGINT"), signalCounts.SIGINT + 1)
  assert.equal(process.listenerCount("SIGTERM"), signalCounts.SIGTERM + 1)

  let signalUnmounts = 0
  p9Module.unmountAll = async () => {
    signalUnmounts += 1
    return []
  }
  const preserveExit = () => {}
  process.on("SIGINT", preserveExit)
  try {
    process.emit("SIGINT")
    await new Promise((resolve) => setImmediate(resolve))
    assert.equal(signalUnmounts, 1)
    assert.equal(process.listenerCount("SIGINT"), signalCounts.SIGINT + 1)
    assert.equal(process.listenerCount("SIGTERM"), signalCounts.SIGTERM)
  } finally {
    process.off("SIGINT", preserveExit)
    p9Module.unmountAll = nativeUnmountAll
  }

  resolveClosed()
  await new Promise((resolve) => setImmediate(resolve))
  assert.equal(process.listenerCount("SIGINT"), signalCounts.SIGINT)
  assert.equal(process.listenerCount("SIGTERM"), signalCounts.SIGTERM)

  p9Module.mount = async () => ({ closed: Promise.resolve() })
  await p9Module.mount9p({}, "/tmp/mount-rs-p9-no-signal-test", { signals: false })
  assert.equal(process.listenerCount("SIGINT"), signalCounts.SIGINT)
  assert.equal(process.listenerCount("SIGTERM"), signalCounts.SIGTERM)
} finally {
  resolveClosed()
  p9Module.mount = nativeMount
  p9Module.unmountAll = nativeUnmountAll
}

const probe = p9ClientProbe()
assert.equal(typeof probe.usable, "boolean")
assert.equal(typeof probe.kernel, "boolean")
assert.equal(typeof probe.transport, "boolean")
assert.equal(typeof probe.modules, "boolean")
assert.equal(typeof probe.root, "boolean")
assert.equal(p9Platform(), process.platform === "linux" ? "linux" : undefined)
assert.equal(p9Platform("linux"), "linux")
assert.equal(p9Platform("darwin"), undefined)
const simulatedDarwinProbe = p9ClientProbe("darwin")
assert.equal(simulatedDarwinProbe.usable, false)
assert.equal(simulatedDarwinProbe.platform, undefined)
assert.equal(simulatedDarwinProbe.kernel, false)
assert.equal(simulatedDarwinProbe.transport, false)
assert.equal(simulatedDarwinProbe.modules, false)
assert.match(simulatedDarwinProbe.reason, /darwin/)
const simulatedLinuxProbe = p9ClientProbe("linux")
assert.equal(simulatedLinuxProbe.platform, "linux")
assert.equal(typeof simulatedLinuxProbe.root, "boolean")
if (process.platform === "linux") {
  assert.deepEqual(simulatedLinuxProbe, probe)
} else {
  assert.equal(simulatedLinuxProbe.usable, false)
  assert.equal(simulatedLinuxProbe.kernel, false)
  assert.equal(simulatedLinuxProbe.transport, false)
  assert.equal(simulatedLinuxProbe.modules, false)
}
assert.deepEqual(await live9pMounts(), [])
assert.deepEqual(await unmountAll9p(), [])

await assert.rejects(
  () => mount9p({}, "/tmp/mount-rs-p9-invalid-driver"),
  (error) => error?.message === "FsDriver method 'stat' must be a function",
)

await assert.rejects(
  () => mount9p({}, "/tmp/mount-rs-p9-invalid-server", { server: {} }),
  (error) => error instanceof TypeError && error.message === "9P mount server must be a P9Server",
)

let serverListenCalls = 0
const probeServer = {
  async listen() {
    serverListenCalls += 1
    return this
  },
}
await assert.rejects(
  () => mount9p({}, "/tmp/mount-rs-p9-invalid-driver-with-server", { server: probeServer }),
  (error) => error?.message === "FsDriver method 'stat' must be a function",
)
assert.equal(serverListenCalls, probe.usable ? 1 : 0)

console.log("mount-rs N-API P9 mount helpers: PASS")
