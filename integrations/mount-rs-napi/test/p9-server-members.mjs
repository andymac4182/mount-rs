import assert from "node:assert/strict"
import { Duplex } from "node:stream"
import { pathToFileURL } from "node:url"

import root from "../index.js"

const { Filesystem, createP9Server } = root

function publicMembers(value) {
  const members = new Set()
  for (let current = value; current && current !== Object.prototype; current = Object.getPrototypeOf(current)) {
    for (const name of Object.getOwnPropertyNames(current)) {
      if (name !== "constructor") members.add(name)
    }
  }
  return [...members].sort()
}

function semanticMembers(value) {
  return publicMembers(value).filter((name) => !name.startsWith("_"))
}

const expectedServerMembers = [
  "address",
  "attach",
  "clients",
  "close",
  "connections",
  "host",
  "listen",
  "options",
  "path",
  "port",
]

const expectedAttachedMembers = [
  "close",
  "closed",
  "id",
  "isClosed",
  "peer",
  "session",
  "stream",
  "waitClosed",
]

const expectedOracleConnectionMembers = ["close", "closed", "peer", "session", "stream"]

function memoryDuplex() {
  return new Duplex({
    read() {},
    write(_chunk, _encoding, callback) {
      callback()
    },
  })
}

const server = createP9Server(Filesystem.memory())
const stream = memoryDuplex()
const connection = server.attach(stream, { own: false, peer: "member-audit" })

try {
  assert.deepEqual(semanticMembers(server), expectedServerMembers)
  assert.deepEqual(semanticMembers(connection), expectedAttachedMembers)
  assert.equal(typeof connection.waitClosed, "function")

  if (process.env.MOUNTX_SOURCE) {
    const oraclePath = pathToFileURL(`${process.env.MOUNTX_SOURCE}/src/9p/server.ts`).href
    const { createP9Server: createOracleP9Server } = await import(oraclePath)
    const { createMemoryDriver } = await import(
      pathToFileURL(`${process.env.MOUNTX_SOURCE}/src/drivers/memory.ts`).href,
    )
    const oracleServer = createOracleP9Server(createMemoryDriver())
    const oracleStream = memoryDuplex()
    const oracleConnection = oracleServer.attach(oracleStream, { own: false, peer: "member-audit" })
    try {
      assert.deepEqual(semanticMembers(oracleServer), expectedServerMembers)
      // `drop()` is an implementation helper on the oracle connection class,
      // not a member of the exported P9Connection contract.
      assert.deepEqual(
        semanticMembers(oracleConnection).filter((name) => name !== "drop"),
        expectedOracleConnectionMembers,
      )
    } finally {
      await oracleConnection.close()
      await oracleConnection.closed
      await oracleServer.close()
      oracleStream.destroy()
    }
  }
} finally {
  await connection.close()
  await connection.waitClosed()
  await server.close()
  stream.destroy()
}

console.log("mount-rs N-API P9 server and connection members: PASS")
