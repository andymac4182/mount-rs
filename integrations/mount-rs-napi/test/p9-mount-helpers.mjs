import assert from "node:assert/strict"
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

const probe = p9ClientProbe()
assert.equal(typeof probe.usable, "boolean")
assert.equal(typeof probe.kernel, "boolean")
assert.equal(typeof probe.transport, "boolean")
assert.equal(typeof probe.modules, "boolean")
assert.equal(typeof probe.root, "boolean")
assert.equal(p9Platform(), process.platform === "linux" ? "linux" : undefined)
assert.deepEqual(await live9pMounts(), [])
assert.deepEqual(await unmountAll9p(), [])

await assert.rejects(
  () => mount9p({}, "/tmp/mount-rs-p9-invalid-driver"),
  (error) => error?.message === "FsDriver method 'stat' must be a function",
)

console.log("mount-rs N-API P9 mount helpers: PASS")
