import assert from "node:assert/strict"
import { pathToFileURL } from "node:url"

import { Filesystem, createS3Server } from "../index.js"

const source = process.env.MOUNTX_SOURCE
if (!source) {
  console.log("mount-rs N-API S3 session/member differential: SKIP (MOUNTX_SOURCE unset)")
  process.exit(0)
}

const [upstreamServer, upstreamMemory] = await Promise.all([
  import(pathToFileURL(`${source}/src/s3/server.ts`).href),
  import(pathToFileURL(`${source}/src/drivers/memory.ts`).href),
])

const options = {
  host: "127.0.0.1",
  port: 0,
  readChunkBytes: 4 * 1024,
  maxXmlBytes: 128 * 1024,
  maxBodyBytes: 1024 * 1024,
  debug: true,
}

const nativeFilesystem = Filesystem.memory()
const oracleDriver = upstreamMemory.createMemoryDriver()
const nativeServer = createS3Server({ buckets: { photos: nativeFilesystem } }, options)
const oracleServer = upstreamServer.createS3Server(
  { buckets: { photos: oracleDriver } },
  options,
)

function memberNames(value) {
  const names = []
  let current = value
  while (current && current !== Object.prototype) {
    names.push(...Object.getOwnPropertyNames(current))
    current = Object.getPrototypeOf(current)
  }
  return [...new Set(names)].sort()
}

function publicSymbolNames(value) {
  const internal = new Set(["mountRsServerLifecycleWrapped", "mountRsS3StreamWrapped"])
  const names = []
  let current = value
  while (current && current !== Object.prototype) {
    for (const symbol of Object.getOwnPropertySymbols(current)) {
      const name = symbol === Symbol.asyncDispose ? "Symbol.asyncDispose" : symbol.description
      if (name && !internal.has(name)) names.push(name)
    }
    current = Object.getPrototypeOf(current)
  }
  return [...new Set(names)].sort()
}

try {
  assert.deepEqual(memberNames(nativeServer), memberNames(oracleServer), "server prototype members")
  assert.deepEqual(
    publicSymbolNames(nativeServer),
    publicSymbolNames(oracleServer),
    "server public symbol members",
  )

  const nativeSessionMembers = memberNames(nativeServer.session)
  const oracleSessionMembers = memberNames(oracleServer.session)
  assert.deepEqual(
    nativeSessionMembers.filter((name) => name !== "handleRequestStream"),
    oracleSessionMembers,
    "session prototype members within supported scope",
  )
  assert.deepEqual(
    publicSymbolNames(nativeServer.session),
    publicSymbolNames(oracleServer.session),
    "session public symbol members",
  )

  assert.deepEqual(nativeServer.buckets, oracleServer.buckets, "server bucket names")
  assert.deepEqual(nativeServer.session.bucketNames, oracleServer.session.bucketNames, "session bucket names")
  assert.deepEqual(Object.keys(nativeServer.session.buckets), ["photos"], "native bucket wrappers")
  assert.deepEqual([...oracleServer.session.buckets.keys()], ["photos"], "oracle bucket map")
  assert.equal(nativeServer.session.options.readChunkBytes, options.readChunkBytes)
  assert.equal(nativeServer.session.options.maxXmlBytes, options.maxXmlBytes)
  assert.equal(nativeServer.session.options.maxBodyBytes, options.maxBodyBytes)
  assert.equal(nativeServer.session.options.debug, options.debug)
  assert.equal(nativeServer.session.options.credentialsConfigured, false)
  assert.equal(typeof nativeServer.session.close, "function", "native session close member")
  assert.equal(typeof oracleServer.session.close, "function", "oracle session close member")

  await nativeServer.session.close()
} finally {
  await nativeServer.close().catch(() => {})
  await oracleServer.close().catch(() => {})
}

console.log("mount-rs N-API S3 session/member differential: PASS (supported facade; close and stream additions scoped)")
