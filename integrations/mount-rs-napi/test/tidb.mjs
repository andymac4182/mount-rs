import assert from "node:assert/strict"
import { createChunkedDriver } from "../index.js"

if (process.env.MOUNT_RS_TIDB_NAPI !== "1") {
  console.log(
    "mount-rs N-API TiDB integration: SKIP " +
      "(set MOUNT_RS_TIDB_NAPI=1 with a built N-API addon)",
  )
} else {
  const tidbUrl = process.env.MOUNT_RS_TIDB_URL
  assert.ok(tidbUrl, "MOUNT_RS_TIDB_URL is required for the N-API TiDB gate")

  const suffix = `${process.pid}-${Date.now()}`
  const prefix =
    process.env.MOUNT_RS_TIDB_NODE_PREFIX || `mount-rs-napi/tidb/${suffix}`
  const blocks =
    process.env.R2_ENDPOINT &&
    process.env.R2_BUCKET &&
    process.env.R2_ACCESS_KEY_ID &&
    process.env.R2_SECRET_ACCESS_KEY
      ? {
          kind: "r2",
          key: `${prefix}/blocks`,
          endpoint: process.env.R2_ENDPOINT,
          bucket: process.env.R2_BUCKET,
          accessKeyId: process.env.R2_ACCESS_KEY_ID,
          secretAccessKey: process.env.R2_SECRET_ACCESS_KEY,
          durable: true,
        }
      : { kind: "memory" }
  const store = {
    metadata: {
      kind: "tidb",
      uri: tidbUrl,
      key: `${prefix}/metadata`,
      durable: true,
    },
    blocks,
  }

  const options = (owner) => ({
    ...store,
    chunkSize: 4096,
    owner,
  })
  const first = await createChunkedDriver(options(`napi-tidb-first-${suffix}`))
  const payload = Buffer.alloc(4096 * 3 + 29, 0x5a)
  await first.writeFile("/tidb-node.txt", payload)
  assert.deepEqual(Buffer.from(await first.readFile("/tidb-node.txt")), payload)
  await first.truncate("/tidb-node.txt", 4096 + 11)
  assert.equal((await first.stat("/tidb-node.txt")).size, 4096 + 11)
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
  console.log(`TIDB_NAPI_BOUNDED_READDIR_PASS phase=seed prefix=${prefix}`)
  await first.shutdown()

  const reopened = await createChunkedDriver(options(`napi-tidb-reopen-${suffix}`))
  try {
    assert.equal((await reopened.stat("/tidb-node.txt")).size, 4096 + 11)
    assert.equal(
      (await reopened.readFile("/tidb-node.txt")).length,
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
    console.log(`TIDB_NAPI_BOUNDED_READDIR_PASS phase=reopen prefix=${prefix}`)
  } finally {
    await reopened.shutdown()
  }
  console.log("mount-rs N-API TiDB integration: PASS")
}
