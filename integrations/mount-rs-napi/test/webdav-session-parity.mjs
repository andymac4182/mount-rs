import assert from "node:assert/strict"
import { pathToFileURL } from "node:url"

import { Filesystem, createWebdavServer } from "../index.js"

const source = process.env.MOUNTX_SOURCE
if (!source) {
  console.log("mount-rs N-API WebDAV session/member differential: SKIP (MOUNTX_SOURCE unset)")
  process.exit(0)
}

const [upstreamServer, upstreamSessionModule, upstreamMemory] = await Promise.all([
  import(pathToFileURL(`${source}/src/webdav/server.ts`).href),
  import(pathToFileURL(`${source}/src/webdav/session.ts`).href),
  import(pathToFileURL(`${source}/src/drivers/memory.ts`).href),
])

const configured = {
  host: "127.0.0.1",
  port: 0,
  realm: "member-parity",
  credentials: { username: "parity-user", password: "parity-secret" },
  readChunkBytes: 4 * 1024,
  maxXmlBytes: 128 * 1024,
  maxBodyBytes: 1024 * 1024,
  locks: {
    defaultTimeoutSeconds: 30,
    maxTimeoutSeconds: 60,
    maxLocks: 4,
  },
  debug: true,
}
const authorization = `Basic ${Buffer.from("parity-user:parity-secret").toString("base64")}`

const nativeFilesystem = Filesystem.memory()
const oracleDriver = upstreamMemory.createMemoryDriver()
const nativeServer = createWebdavServer(nativeFilesystem, configured)
const oracleServer = upstreamServer.createWebdavServer(oracleDriver, configured)
const nativeSession = nativeServer.session
const oracleSession = oracleServer.session

function nativeHead(method, target, headers = []) {
  return { method, target, headers: headers.map(([name, value]) => ({ name, value })) }
}

function oracleHead(method, target, headers = []) {
  return {
    method,
    target,
    headers: Object.fromEntries(headers.map(([name, value]) => [name.toLowerCase(), value])),
  }
}

function oracleBody(body) {
  if (body === undefined || body === null) return undefined
  return (async function* () {
    yield body
  })()
}

function withAuthorization(headers) {
  return [["authorization", authorization], ...headers]
}

function normalizedXmlNode(node) {
  if (node === undefined) return undefined
  return {
    name: node.name,
    ns: node.ns ?? "",
    text: node.text ?? "",
    children: (node.children ?? []).map(normalizedXmlNode),
  }
}

function nativeHeaders(response) {
  return Object.fromEntries(response.headers.map(({ name, value }) => [name.toLowerCase(), value]))
}

function oracleHeaders(response) {
  return Object.fromEntries(Object.entries(response.headers).map(([name, value]) => [name.toLowerCase(), value]))
}

async function responseBody(response) {
  if (response.body === undefined || response.body === null) return null
  if (typeof response.body[Symbol.asyncIterator] === "function") {
    const chunks = []
    for await (const chunk of response.body) chunks.push(Buffer.from(chunk))
    return Buffer.concat(chunks)
  }
  return Buffer.from(response.body)
}

function normalizeXmlBody(body, headers) {
  if (!Buffer.isBuffer(body) || !headers["content-type"]?.toLowerCase().includes("xml")) {
    return body
  }
  return Buffer.from(
    body
      .toString("utf8")
      .replace(
        /\b(?:Mon|Tue|Wed|Thu|Fri|Sat|Sun), \d{2} [A-Z][a-z]{2} \d{4} \d{2}:\d{2}:\d{2} GMT\b/g,
        "<dynamic-http-date>",
      )
      .replace(
        /\b\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z\b/g,
        "<dynamic-iso-date>",
      ),
  )
}

async function staticResponse(response, headers) {
  const body = await responseBody(response)
  return {
    status: response.status,
    contentType: headers["content-type"],
    contentLength: headers["content-length"],
    allow: headers.allow,
    dav: headers.dav,
    body: normalizeXmlBody(body, headers),
  }
}

async function pair(label, method, target, headers = [], body) {
  const requestHeaders = withAuthorization(headers)
  const [nativeResponse, oracleResponse] = await Promise.all([
    nativeSession.handleRequest(nativeHead(method, target, requestHeaders), body ?? null),
    oracleSession.handleRequest(oracleHead(method, target, requestHeaders), oracleBody(body)),
  ])
  const nativeSummary = await staticResponse(nativeResponse, nativeHeaders(nativeResponse))
  const oracleSummary = await staticResponse(oracleResponse, oracleHeaders(oracleResponse))
  assert.deepEqual(nativeSummary, oracleSummary, `${label} response`)
  return { nativeResponse, oracleResponse }
}

try {
  assert.equal(nativeServer.host, oracleServer.host, "server host")
  assert.equal(nativeServer.port, oracleServer.port, "server port before listen")
  assert.equal(nativeServer.url, oracleServer.url, "server URL before listen")
  assert.equal(nativeServer.connections, oracleServer.connections, "server connections before listen")
  assert.equal(typeof nativeServer.listen, "function", "native listen member")
  assert.equal(typeof nativeServer.close, "function", "native close member")
  assert.equal(typeof nativeServer[Symbol.asyncDispose], "function", "native asyncDispose member")
  assert.equal(typeof nativeSession.handleRequest, "function", "native buffered session member")
  assert.equal(typeof nativeSession.handleRequestStream, "function", "native streamed session member")
  assert.equal(typeof oracleServer.listen, "function", "oracle listen member")
  assert.equal(typeof oracleServer.close, "function", "oracle close member")
  assert.equal(typeof oracleServer[Symbol.asyncDispose], "function", "oracle asyncDispose member")
  assert.equal(typeof oracleSession.handleRequest, "function", "oracle buffered session member")

  for (const key of ["driver", "options", "stats", "assertions", "locks"]) {
    assert.ok(key in nativeSession, `native session member ${key}`)
    assert.ok(key in oracleSession, `oracle session member ${key}`)
  }
  assert.ok("lockCount" in nativeSession, "native session member lockCount")

  for (const key of ["realm", "readChunkBytes", "maxXmlBytes", "maxBodyBytes", "debug"]) {
    assert.equal(nativeSession.options[key], oracleSession.options[key], `session option ${key}`)
  }
  assert.deepEqual(nativeSession.options.locks, oracleSession.options.locks, "session lock options")
  assert.deepEqual(nativeSession.options.credentials, oracleSession.options.credentials, "session credentials")
  assert.ok(nativeSession.driver instanceof Filesystem, "native session driver view")
  assert.equal(typeof oracleSession.driver.stat, "function", "oracle session driver view")
  assert.deepEqual(nativeSession.assertions, oracleSession.assertions, "initial assertions")
  assert.equal(nativeSession.stats.methods instanceof Map, true, "native method stats Map")
  assert.equal(oracleSession.stats.methods instanceof Map, true, "oracle method stats Map")

  await pair("OPTIONS", "OPTIONS", "/")
  await pair("PUT", "PUT", "/member-parity.txt", [], Buffer.from("member differential"))
  const get = await pair("GET", "GET", "/member-parity.txt")
  assert.deepEqual(await responseBody(get.nativeResponse), Buffer.from("member differential"), "GET bytes")
  const head = await pair("HEAD", "HEAD", "/member-parity.txt")
  assert.equal(await responseBody(head.nativeResponse), null, "native HEAD body")
  assert.equal(await responseBody(head.oracleResponse), null, "oracle HEAD body")

  await pair("MKCOL", "MKCOL", "/method-parity")
  await pair(
    "method PUT",
    "PUT",
    "/method-parity/source.txt",
    [],
    Buffer.from("method differential"),
  )
  await pair(
    "PROPFIND",
    "PROPFIND",
    "/method-parity",
    [["depth", "1"]],
    Buffer.from('<D:propfind xmlns:D="DAV:"><D:allprop/></D:propfind>'),
  )
  await pair(
    "PROPPATCH",
    "PROPPATCH",
    "/method-parity/source.txt",
    [],
    Buffer.from(
      '<D:propertyupdate xmlns:D="DAV:"><D:set><D:prop>' +
        "<D:getlastmodified>2026-09-21T00:00:00.000Z</D:getlastmodified>" +
        "</D:prop></D:set></D:propertyupdate>",
    ),
  )
  await pair(
    "COPY",
    "COPY",
    "/method-parity/source.txt",
    [["destination", "/method-parity/copy.txt"]],
  )
  const copied = await pair("COPY destination", "GET", "/method-parity/copy.txt")
  assert.deepEqual(await responseBody(copied.nativeResponse), Buffer.from("method differential"))
  await pair(
    "MOVE",
    "MOVE",
    "/method-parity/copy.txt",
    [["destination", "/method-parity/moved.txt"]],
  )
  const moved = await pair("MOVE destination", "GET", "/method-parity/moved.txt")
  assert.deepEqual(await responseBody(moved.nativeResponse), Buffer.from("method differential"))
  await pair("DELETE", "DELETE", "/method-parity/moved.txt")
  const deleted = await pair("DELETE destination", "GET", "/method-parity/moved.txt")
  assert.equal(deleted.nativeResponse.status, 404)

  const lockBody = Buffer.from(
    '<D:lockinfo xmlns:D="DAV:"><D:lockscope><D:exclusive/></D:lockscope>' +
      '<D:locktype><D:write/></D:locktype><D:owner><Z:name xmlns:Z="urn:test">A&amp;B</Z:name>' +
      "</D:owner></D:lockinfo>",
  )
  const [nativeLockResponse, oracleLockResponse] = await Promise.all([
    nativeSession.handleRequest(
      nativeHead("LOCK", "/member-parity.txt", withAuthorization([["depth", "0"]])),
      lockBody,
    ),
    oracleSession.handleRequest(
      oracleHead("LOCK", "/member-parity.txt", withAuthorization([["depth", "0"]])),
      oracleBody(lockBody),
    ),
  ])
  assert.equal(
    nativeHeaders(nativeLockResponse)["content-type"],
    oracleHeaders(oracleLockResponse)["content-type"],
    "LOCK content type",
  )
  const lock = { nativeResponse: nativeLockResponse, oracleResponse: oracleLockResponse }
  assert.equal(lock.nativeResponse.status, 200, "native LOCK status")
  assert.equal(lock.oracleResponse.status, 200, "oracle LOCK status")
  const nativeLockToken = nativeHeaders(lock.nativeResponse)["lock-token"]
  const oracleLockToken = oracleHeaders(lock.oracleResponse)["lock-token"]
  assert.match(nativeLockToken ?? "", /^<urn:uuid:/, "native lock token")
  assert.match(oracleLockToken ?? "", /^<urn:uuid:/, "oracle lock token")
  assert.equal(nativeSession.lockCount, oracleSession.locks.size(Date.now()), "lock count")
  assert.equal(nativeSession.locks.length, 1, "native lock snapshot count")
  const nativeLock = nativeSession.locks[0]
  const oracleLock = oracleSession.locks.all(Date.now())[0]
  assert.deepEqual(
    {
      path: nativeLock.path,
      collection: nativeLock.collection,
      depth: nativeLock.depth,
      exclusive: nativeLock.exclusive,
      timeoutSeconds: nativeLock.timeoutSeconds,
      owner: nativeLock.owner,
    },
    {
      path: oracleLock.path,
      collection: oracleLock.collection,
      depth: String(oracleLock.depth),
      exclusive: oracleLock.exclusive,
      timeoutSeconds: oracleLock.timeoutSeconds,
      owner: normalizedXmlNode(oracleLock.owner),
    },
    "active lock member view",
  )

  const [nativeUnlock, oracleUnlock] = await Promise.all([
    nativeSession.handleRequest(
      nativeHead(
        "UNLOCK",
        "/member-parity.txt",
        withAuthorization([["lock-token", nativeLockToken]]),
      ),
      null,
    ),
    oracleSession.handleRequest(
      oracleHead(
        "UNLOCK",
        "/member-parity.txt",
        withAuthorization([["lock-token", oracleLockToken]]),
      ),
      undefined,
    ),
  ])
  assert.equal(nativeUnlock.status, 204, "native UNLOCK status")
  assert.equal(oracleUnlock.status, 204, "oracle UNLOCK status")
  assert.equal(nativeSession.lockCount, 0, "native lock count after UNLOCK")
  assert.equal(oracleSession.locks.size(Date.now()), 0, "oracle lock count after UNLOCK")

  const unsupported = await pair("PATCH", "PATCH", "/member-parity.txt")
  assert.equal(unsupported.nativeResponse.status, 405, "native unsupported method")
  assert.equal(unsupported.oracleResponse.status, 405, "oracle unsupported method")

  const nativeStats = nativeSession.stats
  const oracleStats = oracleSession.stats
  assert.equal(nativeStats.requests, oracleStats.requests, "request count")
  assert.equal(nativeStats.replies, oracleStats.replies, "reply count")
  assert.equal(nativeStats.errors, oracleStats.errors, "error count")
  assert.deepEqual(
    [...nativeStats.methods.entries()].sort(([left], [right]) => left.localeCompare(right)),
    [...oracleStats.methods.entries()].sort(([left], [right]) => left.localeCompare(right)),
    "method counters",
  )
  assert.equal(nativeStats.assertions, oracleStats.assertions, "assertion count")
  assert.deepEqual(nativeSession.assertions, oracleSession.assertions, "final assertions")
} finally {
  await nativeServer.close().catch(() => {})
  await nativeFilesystem.shutdown()
  await oracleServer.close().catch(() => {})
  await oracleDriver.shutdown?.()
}

console.log("mount-rs N-API WebDAV session/member differential: PASS (supported session/server surface)")
