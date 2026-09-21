import assert from "node:assert/strict"
import { createChunkedDriver } from "../index.js"

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
      : { kind: "memory" }

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
  const first = await createChunkedDriver(options)
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

  const reopened = await createChunkedDriver({
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
    console.log(
      `FOUNDATIONDB_NAPI_BOUNDED_READDIR_PASS phase=reopen prefix=${prefix}`,
    )
  } finally {
    await reopened.shutdown()
  }
  console.log("mount-rs N-API FoundationDB integration: PASS")
}
