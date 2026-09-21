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
  const prefix = `mount-rs-napi/foundationdb/${suffix}`
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
      leaseAuthority: "persisted-single-authority",
    },
    blocks,
  }
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
  } finally {
    await reopened.shutdown()
  }
  console.log("mount-rs N-API FoundationDB integration: PASS")
}
