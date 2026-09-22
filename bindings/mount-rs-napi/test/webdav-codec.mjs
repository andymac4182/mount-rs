import assert from "node:assert/strict"
import { createRequire } from "node:module"
import { pathToFileURL } from "node:url"

const source = process.env.MOUNTX_SOURCE
if (!source) {
  console.log("mount-rs N-API WebDAV barrel differential: SKIP (MOUNTX_SOURCE unset)")
  process.exit(0)
}

const require = createRequire(import.meta.url)
const root = require("@mount-rs/core")
const rootKeys = Object.keys(root)
const native = require("@mount-rs/core/webdav")
assert.deepEqual(Object.keys(root), rootKeys, "WebDAV facade must not mutate root exports")
assert.equal(native.createWebdavServer, root.createWebdavServer)
assert.equal(native.WebdavServer, root.WebdavServer)
assert.equal(native.WebdavSession, root.WebdavSession)

const upstream = await import(pathToFileURL(`${source}/src/webdav/constants.ts`).href)
const upstreamProtocol = await import(pathToFileURL(`${source}/src/webdav/protocol.ts`).href)
const upstreamLocks = await import(pathToFileURL(`${source}/src/webdav/locks.ts`).href)
const upstreamServer = await import(pathToFileURL(`${source}/src/webdav/server.ts`).href)
const upstreamSession = await import(pathToFileURL(`${source}/src/webdav/session.ts`).href)
const thrown = (invoke, label) => {
  try {
    invoke()
  } catch (error) {
    return error
  }
  assert.fail(`${label} did not throw`)
}

for (const name of [
  "DAV_NS",
  "DAV_COMPLIANCE",
  "MS_AUTHOR_VIA",
  "ALLOW_HEADER",
  "RESOURCE_CONTENT_TYPE",
  "COLLECTION_CONTENT_TYPE",
  "XML_CONTENT_TYPE",
  "MAX_XML_BYTES",
  "READ_CHUNK_BYTES",
  "LOCK_TOKEN_PREFIX",
  "DEFAULT_LOCK_TIMEOUT_SECONDS",
  "MAX_LOCK_TIMEOUT_SECONDS",
  "MAX_LOCKS",
  "UNKNOWN_STATUS",
]) {
  assert.deepEqual(native[name], upstream[name], name)
}
assert.deepEqual(native.WEBDAV_METHODS, upstream.WEBDAV_METHODS)
assert.deepEqual(native.STATUS_TEXT, upstream.STATUS_TEXT)
assert.deepEqual(native.ERRNO_STATUS, upstream.ERRNO_STATUS)
assert.equal(native.DEFAULT_HOST, upstreamServer.DEFAULT_HOST)
assert.equal(native.DEFAULT_DRAIN_TIMEOUT, upstreamServer.DEFAULT_DRAIN_TIMEOUT)
assert.deepEqual(native.ALLPROP_NAMES, upstreamSession.ALLPROP_NAMES)
assert.deepEqual(native.QUOTA_NAMES, upstreamSession.QUOTA_NAMES)
for (const stats of [
  { dev: 1, ino: 2, size: 0, mtimeMs: 0 },
  { dev: 7, ino: 42, size: 4096, mtimeMs: 1700000000123.5 },
  { dev: -1, ino: "inode", size: "large", mtimeMs: "now" },
]) {
  assert.equal(native.resourceETag(stats), upstreamSession.resourceETag(stats), "resource ETag")
}
for (const host of [
  "localhost",
  "LOCALHOST",
  "127.0.0.1",
  "127.9.9.9",
  "::1",
  "[::1]",
  "::ffff:127.0.0.1",
  "",
  "0.0.0.0",
  "::",
  "[::]",
  "10.0.0.1",
  "127.0.0.256",
  "::1]",
]) {
  assert.equal(native.isLoopbackHost(host), upstreamServer.isLoopbackHost(host), `loopback ${host}`)
}
for (const [host, credentials] of [
  ["0.0.0.0", false],
  ["", false],
  ["192.168.1.10", false],
  ["127.0.0.1", false],
  ["0.0.0.0", true],
]) {
  assert.equal(native.bindRefusal(host, credentials), upstreamServer.bindRefusal(host, credentials), `bind refusal ${host}`)
}
const nativeBindError = new native.WebdavBindError("0.0.0.0", "refused")
const oracleBindError = new upstreamServer.WebdavBindError("0.0.0.0", "refused")
assert.deepEqual(
  { name: nativeBindError.name, code: nativeBindError.code, host: nativeBindError.host, message: nativeBindError.message },
  { name: oracleBindError.name, code: oracleBindError.code, host: oracleBindError.host, message: oracleBindError.message },
  "bind error shape",
)
assert.equal(native.isWebdavBindError(nativeBindError), upstreamServer.isWebdavBindError(oracleBindError))
assert.equal(native.isWebdavBindError(new Error("not a bind error")), upstreamServer.isWebdavBindError(new Error("not a bind error")))
for (const status of [200, 207, 423, 508, 599]) {
  assert.equal(native.statusLine(status), upstream.statusLine(status), `statusLine ${status}`)
}
for (const code of ["ENOENT", "ENOTDIR", "EIO", "ENOTSUP", "E_NOT_REAL", undefined]) {
  assert.equal(native.statusOf(code), upstream.statusOf(code), `statusOf ${code}`)
}

for (const [target, expected] of [
  ["/", "/"],
  ["/a//b/../c?x=1#fragment", "/a/c"],
  ["/space%20name", "/space name"],
  ["/dot/../../clamped", "/clamped"],
]) {
  assert.equal(native.parseTargetPath(target), upstreamProtocol.parseTargetPath(target), target)
  assert.equal(native.parseTargetPath(target), expected, target)
}
for (const target of ["relative", "/bad%2Fseparator", "/bad%zz", "/bad%00nul"]) {
  const nativeError = thrown(() => native.parseTargetPath(target), target)
  const oracleError = thrown(() => upstreamProtocol.parseTargetPath(target), target)
  assert.equal(nativeError.status, oracleError.status, `${target} status`)
}
for (const [path, collection] of [["/a space", false], ["/a/b", true], ["/", true]]) {
  assert.equal(native.hrefOf(path, collection), upstreamProtocol.hrefOf(path, collection), `${path} href`)
}
for (const value of [undefined, "0", "1", "infinity", " INFINITY ", "2"]) {
  assert.equal(native.parseDepth(value, 1), upstreamProtocol.parseDepth(value, 1), `Depth ${value}`)
}
for (const value of [undefined, "T", "F", " t ", "false"]) {
  assert.equal(native.parseOverwrite(value), upstreamProtocol.parseOverwrite(value), `Overwrite ${value}`)
}
for (const value of [undefined, "Second-1", "second-42", "Infinite", "Infinite, Second-1", "bogus"]) {
  assert.equal(native.parseTimeout(value), upstreamProtocol.parseTimeout(value), `Timeout ${value}`)
}
for (const value of [undefined, "<urn:uuid:abc>", " <token> ", "token", "<>", "<a><b>"]) {
  assert.equal(native.parseLockToken(value), upstreamProtocol.parseLockToken(value), `Lock-Token ${value}`)
}
assert.equal(native.formatLockToken("urn:uuid:abc"), upstreamProtocol.formatLockToken("urn:uuid:abc"))
for (const [value, host] of [
  ["/copy%20here", "example.test"],
  ["http://example.test/dest", "example.test"],
]) {
  assert.equal(native.parseDestination(value, host), upstreamProtocol.parseDestination(value, host), `Destination ${value}`)
}
for (const [value, host] of [[undefined, "example.test"], ["http://other.test/dest", "example.test"]]) {
  const nativeError = thrown(() => native.parseDestination(value, host), `Destination ${value}`)
  const oracleError = thrown(() => upstreamProtocol.parseDestination(value, host), `Destination ${value}`)
  assert.equal(nativeError.status, oracleError.status, `Destination ${value} status`)
}
for (const [value, host] of [
  ["(<urn:uuid:a>)", "example.test"],
  ["(<urn:uuid:a> [\"etag\"])", "example.test"],
  ["<http://example.test/tree> (<urn:uuid:a>) (Not <urn:uuid:b>)", "example.test"],
  ["<http://other.test/tree> (<urn:uuid:a>)", "example.test"],
  ["(<urn:uuid:a>) (<urn:uuid:a>)", "example.test"],
  ["(Not [\"etag with ] bracket\"])", "example.test"],
  ["broken", "example.test"],
  ["(", "example.test"],
]) {
  assert.deepEqual(native.parseIf(value, host), upstreamProtocol.parseIf(value, host), `If ${value}`)
}
for (const lists of [
  native.parseIf("(<urn:uuid:a>) (Not <urn:uuid:b>) (<urn:uuid:a>)", "example.test"),
  native.parseIf("(<urn:uuid:a> [\"etag\"])", "example.test"),
]) {
  assert.deepEqual(native.submittedTokens(lists), upstreamProtocol.submittedTokens(lists), "submitted tokens")
}
for (const [condition, hrefs] of [
  ["lock-token-submitted", []],
  ["no-conflicting-lock", ["/a&b", "/tree/<child>"]],
]) {
  assert.equal(
    native.encodeErrorDocument(condition, hrefs),
    upstreamProtocol.encodeErrorDocument(condition, hrefs),
    `error document ${condition}`,
  )
}
for (const entries of [
  [{ href: "/data", status: 200 }],
  [{ href: "/tree/", propstat: [{ status: 200, props: [{ name: "displayname", text: "A&B" }] }] }],
  [{
    href: "/tree/",
    propstat: [{
      status: 207,
      props: [
        { name: "displayname", text: "A&B" },
        { name: "owner", ns: "urn:test", text: "<owner>" },
      ],
      condition: "failed-dependency",
    }],
    status: 424,
  }],
]) {
  assert.equal(native.encodeMultistatus(entries), upstreamProtocol.encodeMultistatus(entries), "multistatus")
}
assert.deepEqual(native.supportedLockNode(), upstreamProtocol.supportedLockNode(), "supported lock node")
for (const body of [
  Buffer.from(""),
  Buffer.from("<propfind><allprop/></propfind>"),
  Buffer.from("<D:propfind xmlns:D=\"DAV:\"><D:propname/></D:propfind>"),
  Buffer.from("<propfind><prop><getetag/><D:displayname xmlns:D=\"DAV:\"/><Z:getetag xmlns:Z=\"urn:test\"/></prop></propfind>"),
]) {
  assert.deepEqual(native.parsePropfind(body), upstreamProtocol.parsePropfind(body), "PROPFIND")
}
for (const body of [
  Buffer.from("<propertyupdate><set><prop><getlastmodified>now</getlastmodified></prop></set></propertyupdate>"),
  Buffer.from("<propertyupdate><remove><prop><getetag/></prop></remove></propertyupdate>"),
  Buffer.from("<propertyupdate><set><prop><owner xmlns:Z=\"urn:test\"><Z:name>A&amp;B</Z:name></owner></prop></set><remove><prop><getetag/></prop></remove></propertyupdate>"),
]) {
  assert.deepEqual(native.parseProppatch(body), upstreamProtocol.parseProppatch(body), "PROPPATCH")
}
for (const body of [
  Buffer.from(""),
  Buffer.from("<lockinfo><lockscope><exclusive/></lockscope><locktype><write/></locktype></lockinfo>"),
  Buffer.from("<D:lockinfo xmlns:D=\"DAV:\"><D:lockscope><D:shared/></D:lockscope><D:locktype><D:write/></D:locktype><D:owner><Z:name xmlns:Z=\"urn:test\">A&amp;B</Z:name></D:owner></D:lockinfo>"),
]) {
  assert.deepEqual(native.parseLockInfo(body), upstreamProtocol.parseLockInfo(body), "LOCK")
}
for (const body of [
  Buffer.from("<!DOCTYPE propfind><propfind/>") ,
  Buffer.from("<propfind>&unknown;</propfind>"),
  Buffer.from("<propfind>\u0000</propfind>"),
  Buffer.from("<other/>") ,
  Buffer.from("<propfind><prop></propfind>"),
]) {
  const nativeError = thrown(() => native.parsePropfind(body), "invalid PROPFIND")
  const oracleError = thrown(() => upstreamProtocol.parsePropfind(body), "invalid PROPFIND")
  assert.equal(nativeError.status, oracleError.status, "PROPFIND refusal status")
}
const lockRequest = {
  path: "/tree",
  collection: true,
  depth: "infinity",
  exclusive: true,
  owner: { name: "owner", ns: "urn:test", text: "A&B", children: [] },
  timeoutSeconds: 5,
}
const runLocks = (LockTable) => {
  let token = 0
  const table = new LockTable({
    defaultTimeoutSeconds: 4,
    maxTimeoutSeconds: 10,
    maxLocks: 2,
    newToken: () => `urn:uuid:test-${++token}`,
  })
  const root = table.create(lockRequest, 1000)
  const sharedConflict = table.create({
    path: "/tree/child", collection: false, depth: 0, exclusive: false, timeoutSeconds: 2,
  }, 1000)
  const outside = table.create({
    path: "/outside", collection: false, depth: 0, exclusive: true, timeoutSeconds: "infinite",
  }, 1000)
  const covered = table.covering("/tree/child", 2000)
  const within = table.within("/tree", 2000)
  const inScope = LockTable.inScope(root.lock, "/tree/child")
  const refreshed = table.refresh(root.lock.token, undefined, 2000)
  const remaining = LockTable.remaining(refreshed, 2000)
  const removed = table.remove(outside.lock.token)
  const expired = table.find(root.lock.token, 7000)
  return { root, sharedConflict, outside, covered, within, inScope, refreshed, remaining, removed, expired }
}
const nativeLocks = runLocks(native.DavLockTable)
const oracleLocks = runLocks(upstreamLocks.DavLockTable)
assert.deepEqual(nativeLocks, oracleLocks, "DavLockTable lifecycle")
assert.equal(nativeLocks.root.kind, "granted")
assert.equal(nativeLocks.sharedConflict.kind, "conflict")
assert.equal(nativeLocks.outside.kind, "granted")
assert.equal(nativeLocks.inScope, true)
assert.equal(nativeLocks.remaining, 4)
assert.equal(nativeLocks.removed, true)
assert.equal(nativeLocks.expired, undefined)
assert.deepEqual(native.activeLockNode(nativeLocks.refreshed, 2000), upstreamProtocol.activeLockNode(oracleLocks.refreshed, 2000), "active lock node")
assert.deepEqual(native.lockDiscoveryNode([nativeLocks.refreshed], 2000), upstreamProtocol.lockDiscoveryNode([oracleLocks.refreshed], 2000), "lock discovery node")
assert.equal(native.encodeLockResponse(nativeLocks.refreshed, 2000), upstreamProtocol.encodeLockResponse(oracleLocks.refreshed, 2000), "lock response")
assert.equal(native.statusOfError(native.refuse(423)), upstreamProtocol.statusOfError(upstreamProtocol.refuse(423)))
assert.equal(native.statusOfError({ code: "ENOENT" }), upstreamProtocol.statusOfError({ code: "ENOENT" }))
assert.equal(native.statusOfError(new Error("unknown")), upstreamProtocol.statusOfError(new Error("unknown")))
const xmlResponse = native.xmlBody(207, "<body>&amp;</body>", { allow: "GET" })
const oracleXmlResponse = upstreamProtocol.xmlBody(207, "<body>&amp;</body>", { allow: "GET" })
assert.deepEqual(xmlResponse, oracleXmlResponse, "XML response")
assert.deepEqual(native.faultResponse(native.refuse(423, { condition: "no-conflicting-lock", hrefs: ["/tree"] })),
  upstreamProtocol.faultResponse(upstreamProtocol.refuse(423, { condition: "no-conflicting-lock", hrefs: ["/tree"] })),
  "fault response")
assert.deepEqual(native.faultResponse(native.refuse(404)), upstreamProtocol.faultResponse(upstreamProtocol.refuse(404)), "empty fault response")
const chunks = async function* () {
  yield new Uint8Array([1, 2])
  yield Buffer.from([3, 4])
}
assert.deepEqual(await native.collectBody(chunks()), await upstreamProtocol.collectBody(chunks()), "body collection")
const nativeBodyError = await (async () => {
  try { await native.collectBody(chunks(), 3) } catch (error) { return error }
  assert.fail("native collectBody did not refuse an oversized body")
})()
const oracleBodyError = await (async () => {
  try { await upstreamProtocol.collectBody(chunks(), 3) } catch (error) { return error }
  assert.fail("oracle collectBody did not refuse an oversized body")
})()
assert.equal(nativeBodyError.status, oracleBodyError.status, "body collection limit")
const nativeNoBody = []
for await (const chunk of native.NO_BODY) nativeNoBody.push(chunk)
const oracleNoBody = []
for await (const chunk of upstreamProtocol.NO_BODY) oracleNoBody.push(chunk)
assert.deepEqual(nativeNoBody, oracleNoBody, "empty body iterator")
assert.equal(native.isDavFault(native.refuse(400)), true)

console.log("mount-rs N-API WebDAV barrel differential: PASS (complete pure protocol primitives)")
