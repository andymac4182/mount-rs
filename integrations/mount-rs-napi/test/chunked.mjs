import assert from "node:assert/strict"
import { mkdtemp, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { createChunkedDriver } from "../index.js"

const suffix = `${process.pid}-${Date.now()}`

async function exercise(options, expected) {
  const driver = await createChunkedDriver({
    ...options,
    owner: `mount-rs-napi-chunked-${suffix}-${expected}`,
    chunkSize: 4096,
    ttlMs: 30_000,
  })
  const payload = Buffer.alloc(4096 * 2 + 17, expected.charCodeAt(0))
  try {
    await driver.writeFile(`/chunked-${expected}`, payload)
    assert.deepEqual(
      Buffer.from(await driver.readFile(`/chunked-${expected}`)),
      payload,
      `${expected} chunked round trip`,
    )
    assert.equal((await driver.stat(`/chunked-${expected}`)).size, payload.length)
    await driver.shutdown()
    // shutdown is deliberately idempotent so cleanup paths can safely call it
    // after the normal close path.
    await driver.shutdown()
  } finally {
    await driver.shutdown()
  }
}

const directory = await mkdtemp(join(tmpdir(), "mount-rs-napi-chunked-"))
try {
  await exercise(
    {
      metadata: { kind: "memory" },
      blocks: { kind: "sqlite", uri: join(directory, "blocks.sqlite") },
    },
    "mixed",
  )

  await assert.rejects(
    () =>
      createChunkedDriver({
        metadata: { kind: "unknown" },
        blocks: { kind: "memory" },
        chunkSize: 4096,
      }),
    /unknown metadata backend/,
  )
  await assert.rejects(
    () =>
      createChunkedDriver({
        metadata: { kind: "sqlite" },
        blocks: { kind: "memory" },
        chunkSize: 4096,
      }),
    /metadata\.uri is required/,
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
  } else {
    console.log("mount-rs N-API chunked PGlite integration: SKIP (PGLITE_DATABASE_URL unset)")
  }
} finally {
  await rm(directory, { recursive: true, force: true })
}

console.log("mount-rs N-API chunked integration: PASS")
