import assert from "node:assert/strict"
import sdk from "../index.js"

const { createChunkedDriver, shutdownFoundationdbClientNetwork } = sdk

if (process.env.MOUNT_RS_NAPI_FOUNDATIONDB !== "1") {
  console.log(
    "mount-rs N-API FoundationDB integration: SKIP " +
      "(set MOUNT_RS_NAPI_FOUNDATIONDB=1 with a foundationdb-feature build)",
  )
} else {
  const clusterFile = process.env.MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE
  assert.ok(
    clusterFile,
    "MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE is required for the N-API FoundationDB gate",
  )
  assert.equal(
    typeof shutdownFoundationdbClientNetwork,
    "function",
    "the foundationdb-feature addon must expose terminal client-network shutdown",
  )

  let first
  let reopened
  let concurrentA
  let concurrentB
  try {
    const suffix = `${process.pid}-${Date.now()}`
    const prefix =
      process.env.MOUNT_RS_FOUNDATIONDB_NODE_PREFIX ||
      `mount-rs-napi/foundationdb/${suffix}`
    const sharedProvider = process.env.MOUNT_RS_NAPI_FOUNDATIONDB_SHARED_PROVIDER === "1"
    const authorityPrefix = process.env.MOUNT_RS_FOUNDATIONDB_AUTHORITY_PREFIX
    if (sharedProvider) {
      assert.ok(
        authorityPrefix,
        "MOUNT_RS_FOUNDATIONDB_AUTHORITY_PREFIX is required for shared-provider mode",
      )
    }
    const blocks =
      process.env.R2_ENDPOINT &&
      process.env.R2_BUCKET &&
      process.env.R2_ACCESS_KEY_ID &&
      process.env.R2_SECRET_ACCESS_KEY
      ? {
          kind: "r2",
            key: prefix,
            endpoint: process.env.R2_ENDPOINT,
            bucket: process.env.R2_BUCKET,
            accessKeyId: process.env.R2_ACCESS_KEY_ID,
            secretAccessKey: process.env.R2_SECRET_ACCESS_KEY,
          durable: true,
        }
      : {
          kind: "foundationdb",
          uri: clusterFile,
          key: `${prefix}/blocks`,
          durable: true,
          leaseAuthority: sharedProvider ? "shared-provider" : "persisted-single-authority",
        }

    const store = {
      metadata: {
        kind: "foundationdb",
        uri: clusterFile,
        key: prefix,
        durable: true,
        leaseAuthority: sharedProvider ? "shared-provider" : "persisted-single-authority",
      },
      blocks,
    }
    if (sharedProvider) {
      store.metadata.authorityPrefix = authorityPrefix
      if (store.blocks.kind === "foundationdb") {
        store.blocks.authorityPrefix = authorityPrefix
      }
    }
    const missingAuthorityPrefixMetadata = {
      ...store.metadata,
      leaseAuthority: "shared-provider",
    }
    delete missingAuthorityPrefixMetadata.authorityPrefix
    await assert.rejects(
      () =>
        createChunkedDriver({
          metadata: missingAuthorityPrefixMetadata,
          blocks: { kind: "memory" },
          chunkSize: 4096,
        }),
      /metadata\.authorityPrefix is required/,
    )
    await assert.rejects(
      () =>
        createChunkedDriver({
          metadata: {
            ...store.metadata,
            leaseAuthority: "persisted-single-authority",
            authorityPrefix: "not-valid-for-persisted-mode",
          },
          blocks: { kind: "memory" },
          chunkSize: 4096,
        }),
      /metadata\.authorityPrefix is not valid for this backend/,
    )
    const options = {
      ...store,
      chunkSize: 4096,
      owner: `napi-foundationdb-${suffix}`,
    }
    first = await createChunkedDriver(options)
    assert.throws(
      () => shutdownFoundationdbClientNetwork(),
      (error) => error.code === "EBUSY",
      "a live FoundationDB filesystem must keep the client network running",
    )
    const payload = Buffer.alloc(4096 * 3 + 29, 0x5a)
    await first.writeFile("/foundationdb-node.txt", payload)
    assert.deepEqual(Buffer.from(await first.readFile("/foundationdb-node.txt")), payload)
    await first.truncate("/foundationdb-node.txt", 4096 + 11)
    assert.equal((await first.stat("/foundationdb-node.txt")).size, 4096 + 11)
    await first.mkdir("/bounded-listing")
    await first.writeFile("/bounded-listing/alpha", Buffer.from("alpha"))
    await first.writeFile("/bounded-listing/beta", Buffer.from("beta"))
    assert.deepEqual(
      (await first.readdirBounded("/bounded-listing", 2)).map((entry) => entry.name).sort(),
      ["alpha", "beta"],
    )
    await assert.rejects(
      () => first.readdirBounded("/bounded-listing", 1),
      (error) => error.code === "EOVERFLOW",
    )
    console.log(
      `FOUNDATIONDB_NAPI_BOUNDED_READDIR_PASS phase=seed prefix=${prefix}`,
    )
    await first.shutdown()

    reopened = await createChunkedDriver({
      ...store,
      chunkSize: 4096,
      owner: `napi-foundationdb-reopen-${suffix}`,
    })
    try {
      assert.equal(
        (await reopened.stat("/foundationdb-node.txt")).size,
        4096 + 11,
      )
      assert.equal(
        (await reopened.readFile("/foundationdb-node.txt")).length,
        4096 + 11,
      )
      assert.deepEqual(
        (await reopened.readdirBounded("/bounded-listing", 2))
          .map((entry) => entry.name)
          .sort(),
        ["alpha", "beta"],
      )
      await assert.rejects(
        () => reopened.readdirBounded("/bounded-listing", 1),
        (error) => error.code === "EOVERFLOW",
      )
      await reopened.unlink("/foundationdb-node.txt")
      await reopened.unlink("/bounded-listing/alpha")
      await reopened.unlink("/bounded-listing/beta")
      await reopened.rmdir("/bounded-listing")
      await assert.rejects(
        () => reopened.stat("/foundationdb-node.txt"),
        (error) => error.code === "ENOENT",
      )
      console.log(
        `FOUNDATIONDB_NAPI_BOUNDED_READDIR_PASS phase=reopen prefix=${prefix}`,
      )
    } finally {
      await reopened.shutdown()
    }
    const concurrentStore = {
      metadata: {
        kind: "foundationdb",
        uri: clusterFile,
        key: `${prefix}/concurrent-metadata`,
        durable: true,
        leaseAuthority: "revision-cas",
      },
      blocks: {
        kind: "foundationdb",
        uri: clusterFile,
        key: `${prefix}/concurrent-blocks`,
        durable: true,
        leaseAuthority: "revision-cas",
      },
      chunkSize: 4096,
      concurrentWrites: true,
    }
    concurrentA = await createChunkedDriver({ ...concurrentStore, owner: `napi-concurrent-a-${suffix}` })
    concurrentB = await createChunkedDriver({ ...concurrentStore, owner: `napi-concurrent-b-${suffix}` })
    await concurrentA.writeFile("/from-a.txt", Buffer.from("writer A"))
    assert.equal(Buffer.from(await concurrentB.readFile("/from-a.txt")).toString(), "writer A")
    await concurrentB.writeFile("/from-b.txt", Buffer.from("writer B"))
    assert.equal(Buffer.from(await concurrentA.readFile("/from-b.txt")).toString(), "writer B")
    console.log(`FOUNDATIONDB_NAPI_CONCURRENT_VISIBILITY_PASS prefix=${prefix}`)
    await concurrentB.shutdown()
    await concurrentA.shutdown()
    shutdownFoundationdbClientNetwork()
    shutdownFoundationdbClientNetwork()
    await assert.rejects(
      () => createChunkedDriver(options),
      (error) => error.code === "EIO",
      "terminal network shutdown must reject another FoundationDB connection",
    )
    console.log("mount-rs N-API FoundationDB integration: PASS")
  } finally {
    let cleanupError
    for (const filesystem of [concurrentB, concurrentA, reopened, first]) {
      if (!filesystem) continue
      try {
        await filesystem.shutdown()
      } catch (error) {
        cleanupError ??= error
      }
    }
    try {
      shutdownFoundationdbClientNetwork()
    } catch (error) {
      if (error?.code === "ENOTSUP") {
        cleanupError ??= error
      } else {
        console.error("FoundationDB client network could not stop safely:", error)
        process.abort()
      }
    }
    if (cleanupError) throw cleanupError
  }
}
