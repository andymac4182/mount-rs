import assert from "node:assert/strict"
import { mkdtemp, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { createChunkedDriver, createNfsServer, createP9Server, createWebdavServer, createS3Server, mount } from "../index.js"

const suffix = `${process.pid}-${Date.now()}`

function memoryStores() {
  return {
    metadata: { kind: "memory" },
    blocks: { kind: "memory" },
  }
}

async function openDriver(options = {}) {
  return createChunkedDriver({
    ...memoryStores(),
    ...options,
    owner: options.owner ?? `mount-rs-napi-chunked-${suffix}`,
    chunkSize: options.chunkSize ?? 4096,
    ttlMs: options.ttlMs ?? 30_000,
  })
}

async function assertCode(operation, code) {
  await assert.rejects(operation, (error) => {
    assert.equal(error.code, code, `${code} error code`)
    return true
  })
}

async function exercise(options, expected) {
  const driver = await openDriver(options)
  const payload = Buffer.alloc(4096 * 2 + 17, expected.charCodeAt(0))
  const path = `/chunked-${expected}`
  try {
    await driver.writeFile(path, payload)
    assert.deepEqual(Buffer.from(await driver.readFile(path)), payload, `${expected} round trip`)
    assert.equal((await driver.stat(path)).size, payload.length)
  } finally {
    await driver.shutdown()
    // shutdown is deliberately idempotent so cleanup paths can safely call it
    // after the normal close path.
    await driver.shutdown()
  }
}

const directory = await mkdtemp(join(tmpdir(), "mount-rs-napi-chunked-"))
try {
  await assertCode(() => openDriver({ inodeUpdates: true }), "EINVAL")
  await assertCode(() => openDriver({ inodeUpdates: true, concurrentWrites: true, ownershipMode: "shared" }), "EINVAL")
  const inodeOptions = {
    metadata: { kind: "sqlite", uri: join(directory, "inode-metadata.sqlite") },
    blocks: { kind: "sqlite", uri: join(directory, "inode-blocks.sqlite") },
    concurrentWrites: true,
    inodeUpdates: true,
  }
  if (process.platform === "win32") {
    // The inode layout requires the same qualified physical-file guard as
    // concurrent SQLite, which Windows does not yet provide.
    await assertCode(() => openDriver(inodeOptions), "ENOTSUP")
  } else {
    const inodeDriver = await openDriver(inodeOptions)
    await inodeDriver.writeFile("/inode", Buffer.from("unchanged boundary"))
    const inodeHandle = await inodeDriver.open("/inode", "r+")
    const patch = Buffer.from("CHANGED")
    assert.equal((await inodeHandle.write(patch, 0, patch.length, 2)).bytesWritten, patch.length)
    await inodeHandle.close()
    const expectedInode = Buffer.from("unchanged boundary")
    patch.copy(expectedInode, 2)
    assert.deepEqual(Buffer.from(await inodeDriver.readFile("/inode")), expectedInode)
    await inodeDriver.shutdown()
    const reopenedInode = await openDriver(inodeOptions)
    assert.deepEqual(Buffer.from(await reopenedInode.readFile("/inode")), expectedInode)
    await reopenedInode.unlink("/inode")
    await reopenedInode.shutdown()
    await assert.rejects(() => openDriver({ ...inodeOptions, inodeUpdates: false }))
  }
  for (const ownershipMode of ["invalid", "shared"]) {
    await assertCode(() => openDriver({ ownershipMode }), "EINVAL")
  }
  for (const [ownershipMode, concurrentWrites] of [["exclusive", true], ["shared", false]]) {
    await assertCode(() => openDriver({ ownershipMode, concurrentWrites }), "EINVAL")
  }
  for (const ownershipMode of [undefined, "exclusive"]) {
    await assertCode(() => openDriver({ ownershipMode, checkoutPath: "/" }), "EINVAL")
  }
  await exercise({ ownershipMode: "exclusive" }, "exclusive")

  const defaults = await openDriver()
  try {
    const root = await defaults.stat("/")
    assert.equal(root.uid, process.getuid?.() ?? 0, "default chunked uid")
    assert.equal(root.gid, process.getgid?.() ?? 0, "default chunked gid")
    await assertCode(() => defaults.reconcileBlocks(1_000), "ENOTSUP")
    await assertCode(() => defaults.reconcileBlocks(0), "ERR_OUT_OF_RANGE")
  } finally {
    await defaults.shutdown()
  }

  await exercise(
    {
      metadata: { kind: "memory" },
      blocks: { kind: "sqlite", uri: join(directory, "mixed-blocks.sqlite") },
    },
    "mixed",
  )

  // SQLite is the durable local provider: a new driver sees the namespace and
  // immutable block bytes after the first driver's explicit shutdown.
  const sqliteOptions = {
    metadata: { kind: "sqlite", uri: join(directory, "metadata.sqlite") },
    blocks: { kind: "sqlite", uri: join(directory, "blocks.sqlite") },
  }
  const first = await openDriver({ ...sqliteOptions, ownershipMode: "exclusive", owner: `sqlite-first-${suffix}` })
  await first.writeFile("/reopen.txt", Buffer.from("durable sqlite"))
  await first.shutdown()
  const reopened = await openDriver({ ...sqliteOptions, owner: `sqlite-second-${suffix}` })
  try {
    assert.equal(Buffer.from(await reopened.readFile("/reopen.txt")).toString(), "durable sqlite")
  } finally {
    await reopened.shutdown()
  }

  const concurrentSqlite = {
    metadata: { kind: "sqlite", uri: join(directory, "concurrent-metadata.sqlite") },
    blocks: { kind: "sqlite", uri: join(directory, "concurrent-blocks.sqlite") },
    concurrentWrites: true,
  }
  if (process.platform === "win32") {
    // Windows has no qualified physical-file guard for concurrent SQLite.
    await assertCode(
      () => openDriver({ ...concurrentSqlite, owner: `concurrent-unsupported-${suffix}` }),
      "ENOTSUP",
    )
    console.log("mount-rs N-API concurrent SQLite: PASS (Windows ENOTSUP)")
  } else {
    const writerA = await openDriver({ ...concurrentSqlite, owner: `concurrent-a-${suffix}` })
    const writerB = await openDriver({ ...concurrentSqlite, owner: `concurrent-b-${suffix}` })
    try {
      await writerA.writeFile("/from-a.txt", Buffer.from("writer a"))
      await writerB.writeFile("/from-b.txt", Buffer.from("writer b"))
      assert.equal(Buffer.from(await writerA.readFile("/from-b.txt")).toString(), "writer b")
      assert.equal(Buffer.from(await writerB.readFile("/from-a.txt")).toString(), "writer a")
    } finally {
      await writerB.shutdown()
      await writerA.shutdown()
    }
  }

  if (process.platform !== "win32") {
    const delegated = {
      metadata: { kind: "sqlite", uri: join(directory, "delegated-metadata.sqlite") },
      blocks: { kind: "sqlite", uri: join(directory, "delegated-blocks.sqlite") },
      ownershipMode: "shared",
    }
    const bootstrap = await openDriver({ ...delegated, checkoutPath: "/" })
    try {
      for (const createServer of [createNfsServer, createP9Server, createWebdavServer, createS3Server]) {
        assert.throws(() => createServer(bootstrap), (error) => error.code === "ENOTSUP")
      }
      assert.throws(() => createS3Server({ buckets: { owned: bootstrap } }), (error) => error.code === "ENOTSUP")
      await assertCode(() => mount(bootstrap, join(directory, "unsupported-mount"), { nfsSharedView: true }), "ENOTSUP")
      for (const transport of ["nfs", "9p"]) {
        await assertCode(() => mount(bootstrap, join(directory, "unsupported-mount"), { transport }), "ENOTSUP")
      }
      const rootGrant = await bootstrap.delegationStatus()
      assert.equal(typeof rootGrant.fence, "string", "fences retain all 64 bits")
      await bootstrap.mkdir("/owned-a")
      await bootstrap.mkdir("/owned-b")
      await bootstrap.checkinScope()
      assert.equal(await bootstrap.delegationStatus(), null)
      await assertCode(
        () => mount(bootstrap, join(directory, "unclaimed-mount"), { transport: "fuse" }),
        process.platform === "linux" ? "EACCES" : "ENOTSUP",
      )
    } finally {
      await bootstrap.shutdown()
    }
    const ownerA = await openDriver({ ...delegated, checkoutPath: "/owned-a", owner: `directory-a-${suffix}` })
    const ownerB = await openDriver({ ...delegated, checkoutPath: "/owned-b", owner: `directory-b-${suffix}` })
    try {
      await ownerA.writeFile("/owned-a/database", Buffer.from("a owns database"))
      await ownerB.writeFile("/owned-b/database", Buffer.from("b owns database"))
      await assertCode(() => ownerB.readFile("/owned-a/database"), "EACCES")
      await assertCode(() => ownerB.writeFile("/owned-a/database", Buffer.from("denied")), "EACCES")
      await assertCode(() => openDriver({ ...delegated, checkoutPath: "/owned-a" }), "ESTALE")
      const handle = await ownerA.open("/owned-a/database", "r")
      await assertCode(() => ownerA.checkinScope(), "EBUSY")
      await handle.close()
      await ownerA.checkinScope()
      await assertCode(() => ownerA.readFile("/owned-a/database"), "EACCES")
      await ownerB.checkinScope()
      const handedOff = await ownerB.checkoutScope("/owned-a")
      assert.equal(typeof handedOff.root, "string")
      assert.equal(Buffer.from(await ownerB.readFile("/owned-a/database")).toString(), "a owns database")
      await ownerB.checkinScope()
    } finally {
      await ownerB.shutdown()
      await ownerA.shutdown()
    }
    console.log("mount-rs N-API delegated SQLite: PASS (disjoint scopes, denied access, clean handoff)")
  }

  // The provider lease is held until shutdown, so a second writer fails
  // immediately. Once the first writer releases it, a replacement succeeds.
  const leaseOptions = {
    metadata: { kind: "sqlite", uri: join(directory, "lease-metadata.sqlite") },
    blocks: { kind: "sqlite", uri: join(directory, "lease-blocks.sqlite") },
  }
  const held = await openDriver({ ...leaseOptions, owner: `lease-held-${suffix}` })
  try {
    await assertCode(
      () => openDriver({ ...leaseOptions, owner: `lease-contender-${suffix}` }),
      "EAGAIN",
    )
  } finally {
    await held.shutdown()
  }
  const afterRelease = await openDriver({ ...leaseOptions, owner: `lease-after-${suffix}` })
  await afterRelease.shutdown()

  const closed = await openDriver()
  await closed.shutdown()
  await assertCode(() => closed.stat("/"), "EBADF")
  await assertCode(() => closed.writeFile("/closed.txt", Buffer.from("closed")), "EBADF")

  await assertCode(
    () =>
      createChunkedDriver({
        metadata: { kind: "unknown" },
        blocks: { kind: "memory" },
        chunkSize: 4096,
      }),
    "EINVAL",
  )
  await assertCode(
    () => createChunkedDriver({ ...memoryStores(), chunkSize: 4096, concurrentWrites: true }),
    "EINVAL",
  )
  const concurrentMetadata = {
    kind: "foundationdb",
    uri: "/missing/fdb.cluster",
    key: "concurrent-validation",
    leaseAuthority: "revision-cas",
  }
  for (const kind of ["memory", "sqlite"]) {
    await assertCode(
      () =>
        createChunkedDriver({
          metadata: concurrentMetadata,
          blocks: { kind, uri: "/missing/local-blocks.sqlite" },
          chunkSize: 4096,
          concurrentWrites: true,
        }),
      "EINVAL",
    )
  }
  await assertCode(
    () => createChunkedDriver({
      metadata: { kind: "sqlite", uri: ":memory:" },
      blocks: { kind: "sqlite", uri: join(directory, "concurrent-blocks.sqlite") },
      chunkSize: 4096,
      concurrentWrites: true,
    }),
    "EINVAL",
  )
  await assertCode(
    () => createChunkedDriver({
      metadata: { kind: "sqlite", uri: join(directory, "concurrent-meta.sqlite") },
      blocks: { kind: "memory" },
      chunkSize: 4096,
      concurrentWrites: true,
    }),
    "EINVAL",
  )
  await assertCode(
    () => createChunkedDriver({
      metadata: { kind: "pglite", uri: "postgres://127.0.0.1:1/missing", key: "metadata" },
      blocks: { kind: "sqlite", uri: join(directory, "concurrent-blocks.sqlite") },
      chunkSize: 4096,
      concurrentWrites: true,
    }),
    "EINVAL",
  )
  await assertCode(
    () => createChunkedDriver({
      metadata: { kind: "rustfs" },
      blocks: { kind: "memory" },
      chunkSize: 4096,
    }),
    "EINVAL",
  )
  await assertCode(
    () => createChunkedDriver({
      metadata: { kind: "memory" },
      blocks: { kind: "rustfs", endpoint: "http://127.0.0.1:9878", bucket: "test", key: "test", accessKeyId: "key", secretAccessKey: "secret" },
      chunkSize: 4096,
    }),
    "EINVAL",
  )
  await assertCode(
    () =>
      createChunkedDriver({
        metadata: concurrentMetadata,
        blocks: { kind: "memory" },
        chunkSize: 4096,
      }),
    "EINVAL",
  )
  await assertCode(
    () =>
      createChunkedDriver({
        metadata: {
          kind: "foundationdb",
          key: "missing-cluster-uri",
          leaseAuthority: "persisted-single-authority",
        },
        blocks: { kind: "memory" },
        chunkSize: 4096,
      }),
    "EINVAL",
  )
  await assertCode(
    () =>
      createChunkedDriver({
        metadata: { kind: "sqlite" },
        blocks: { kind: "memory" },
        chunkSize: 4096,
      }),
    "EINVAL",
  )
  for (const chunkSize of [0, 1.5]) {
    await assertCode(() => createChunkedDriver({ ...memoryStores(), chunkSize }), "ERR_OUT_OF_RANGE")
  }
  await assertCode(
    () => createChunkedDriver({ ...memoryStores(), chunkSize: 4096, ttlMs: 1.5 }),
    "ERR_OUT_OF_RANGE",
  )
  await assertCode(
    () => createChunkedDriver({ ...memoryStores(), chunkSize: 4096, uid: 4_294_967_296 }),
    "ERR_OUT_OF_RANGE",
  )
  await assertCode(
    () => createChunkedDriver({ ...memoryStores(), chunkSize: 4096, owner: "" }),
    "EINVAL",
  )
  await assertCode(
    () =>
      createChunkedDriver({
        metadata: { kind: "memory", durable: false },
        blocks: { kind: "memory" },
        chunkSize: 4096,
      }),
    "EINVAL",
  )

  if (process.env.PGLITE_DATABASE_URL) {
    await exercise(
      {
        metadata: {
          kind: "pglite",
          uri: process.env.PGLITE_DATABASE_URL,
          key: `napi-meta-${suffix}`,
          durable: false,
        },
        blocks: {
          kind: "pglite",
          uri: process.env.PGLITE_DATABASE_URL,
          key: `napi-blocks-${suffix}`,
          durable: false,
        },
      },
      "pglite",
    )

    const concurrentPglite = {
      metadata: {
        kind: "pglite",
        uri: process.env.PGLITE_DATABASE_URL,
        key: `napi-concurrent-meta-${suffix}`,
        durable: false,
      },
      blocks: {
        kind: "pglite",
        uri: process.env.PGLITE_DATABASE_URL,
        key: `napi-concurrent-blocks-${suffix}`,
        durable: false,
      },
      concurrentWrites: true,
    }
    const pgliteA = await openDriver({ ...concurrentPglite, owner: `pglite-a-${suffix}` })
    const pgliteB = await openDriver({ ...concurrentPglite, owner: `pglite-b-${suffix}` })
    try {
      await pgliteA.writeFile("/from-a.txt", Buffer.from("pglite a"))
      await pgliteB.writeFile("/from-b.txt", Buffer.from("pglite b"))
      assert.equal(Buffer.from(await pgliteA.readFile("/from-b.txt")).toString(), "pglite b")
      assert.equal(Buffer.from(await pgliteB.readFile("/from-a.txt")).toString(), "pglite a")
    } finally {
      await pgliteB.shutdown()
      await pgliteA.shutdown()
    }

    // Keep every JS Filesystem alive after shutdown. The explicit close must
    // release both PostgreSQL-wire clients; relying on native object drop or
    // JavaScript GC exhausts the isolated server's four-connection limit after
    // only two retained drivers.
    const retained = []
    for (let index = 0; index < 5; index += 1) {
      const driver = await openDriver({
        metadata: {
          kind: "pglite",
          uri: process.env.PGLITE_DATABASE_URL,
          key: `napi-lifecycle-meta-${suffix}-${index}`,
          durable: false,
        },
        blocks: {
          kind: "pglite",
          uri: process.env.PGLITE_DATABASE_URL,
          key: `napi-lifecycle-blocks-${suffix}-${index}`,
          durable: false,
        },
        owner: `napi-lifecycle-${suffix}-${index}`,
      })
      await driver.writeFile(`/lifecycle-${index}`, Buffer.from(`value-${index}`))
      await driver.shutdown()
      await assertCode(() => driver.stat("/"), "EBADF")
      retained.push(driver)
    }
    assert.equal(retained.length, 5)
  } else {
    console.log("mount-rs N-API chunked PGlite integration: SKIP (PGLITE_DATABASE_URL unset)")
  }
} finally {
  await rm(directory, { recursive: true, force: true })
}

console.log("mount-rs N-API chunked integration: PASS")
