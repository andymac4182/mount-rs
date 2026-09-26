import assert from "node:assert/strict"

// Called before the existing FoundationDB gate's terminal network shutdown.
export async function verifyFoundationdbInodeLayouts(createChunkedDriver, clusterFile, prefix) {
  const fdb = (key) => ({
    kind: "foundationdb", uri: clusterFile, key, durable: true,
    leaseAuthority: "revision-cas",
  })
  const external = process.env.R2_ENDPOINT && process.env.R2_BUCKET &&
    process.env.R2_ACCESS_KEY_ID && process.env.R2_SECRET_ACCESS_KEY
  // Explicit test fixture selection; production still validates/qualifies the chosen provider.
  const externalKind = process.env.MOUNT_RS_NAPI_FOUNDATIONDB_EXTERNAL_BLOCK_PROVIDER || "r2"
  assert.ok(["r2", "rustfs"].includes(externalKind))
  if (external && externalKind === "rustfs") assert.ok(process.env.RUSTFS_REGION)
  const externalLayout = `external-${externalKind}`
  const layouts = ["same-prefix", "split-prefix", ...(external ? [externalLayout] : [])]
  for (const layout of layouts) {
    const metadata = fdb(`${prefix}/inode/${layout}/metadata`)
    const blocks = layout === "same-prefix" ? { ...metadata } : layout === "split-prefix"
      ? fdb(`${prefix}/inode/${layout}/blocks`)
      : {
          kind: externalKind, ...(externalKind === "rustfs" ? { region: process.env.RUSTFS_REGION } : {}), key: `${prefix}/inode/${layout}/blocks`, durable: true,
          endpoint: process.env.R2_ENDPOINT, bucket: process.env.R2_BUCKET,
          accessKeyId: process.env.R2_ACCESS_KEY_ID, secretAccessKey: process.env.R2_SECRET_ACCESS_KEY,
        }
    const options = { metadata, blocks, chunkSize: 4096, concurrentWrites: true, inodeUpdates: true }
    const drivers = []
    try {
      const a = await createChunkedDriver({ ...options, owner: `inode-${layout}-a` })
      drivers.push(a)
      const b = await createChunkedDriver({ ...options, owner: `inode-${layout}-b` })
      drivers.push(b)
      const payload = Buffer.alloc(4096 * 2 + 17, 0x39)
      const replacement = Buffer.alloc(payload.length, 0xc7)
      replacement.writeBigUInt64LE(257n)
      await a.writeFile("/oracle", payload)
      assert.deepEqual(Buffer.from(await b.readFile("/oracle")), payload)
      await b.writeFile("/oracle", replacement)
      assert.deepEqual(Buffer.from(await a.readFile("/oracle")), replacement)
      const patch = Buffer.from("cross-boundary patch")
      const handle = await b.open("/oracle", "r+")
      try {
        assert.equal((await handle.write(patch, 0, patch.length, 4090)).bytesWritten, patch.length)
      } finally {
        await handle.close()
      }
      patch.copy(replacement, 4090)
      assert.deepEqual(Buffer.from(await a.readFile("/oracle")), replacement)
      await b.rename("/oracle", "/renamed")
      assert.deepEqual(Buffer.from(await a.readFile("/renamed")), replacement)
      await b.rename("/renamed", "/oracle")
      await b.writeFile("/sibling", Buffer.from("independent structural publication"))
      assert.deepEqual(Buffer.from(await a.readFile("/sibling")), Buffer.from("independent structural publication"))
      await b.shutdown()
      await a.shutdown()
      const reopened = await createChunkedDriver({ ...options, owner: `inode-${layout}-reopen` })
      drivers.push(reopened)
      assert.deepEqual(Buffer.from(await reopened.readFile("/oracle")), replacement)
      await reopened.unlink("/oracle")
      await reopened.unlink("/sibling")
      assert.deepEqual(await reopened.readdir("/"), [])
      await reopened.shutdown()
      await assert.rejects(
        () => createChunkedDriver({ ...options, blocks: { ...blocks, key: `${blocks.key}/wrong` } }),
        (error) => error.code === "ESTALE",
        "changed block pairing must not rebind metadata or create a returned driver",
      )
      console.log(`FOUNDATIONDB_NAPI_INODE_PASS layout=${layout} bytes=${payload.length} reopen=true cleanup=true`)
    } finally {
      await Promise.all(drivers.map((driver) => driver.shutdown()))
    }
  }
  if (!external) console.log("FOUNDATIONDB_NAPI_INODE_EXTERNAL_SKIP (no configured R2-compatible store)")
}
