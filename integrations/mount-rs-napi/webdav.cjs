"use strict"

const { createHash, randomUUID } = require("node:crypto")

// The ./webdav entry keeps the root server facade and adds the transport's
// low-level constants. The protocol/session classes remain the native server
// boundary; this barrel deliberately does not invent JS codecs for operations
// that are not yet bound by N-API.
const root = require("./index.js")
const binding = { ...root }

const DEFAULT_HOST = "127.0.0.1"

const STATUS_TEXT = Object.freeze({
  200: "OK",
  201: "Created",
  204: "No Content",
  206: "Partial Content",
  207: "Multi-Status",
  400: "Bad Request",
  401: "Unauthorized",
  403: "Forbidden",
  404: "Not Found",
  405: "Method Not Allowed",
  409: "Conflict",
  412: "Precondition Failed",
  413: "Content Too Large",
  414: "URI Too Long",
  415: "Unsupported Media Type",
  416: "Range Not Satisfiable",
  423: "Locked",
  424: "Failed Dependency",
  500: "Internal Server Error",
  501: "Not Implemented",
  502: "Bad Gateway",
  503: "Service Unavailable",
  507: "Insufficient Storage",
  508: "Loop Detected",
})

const ERRNO_STATUS = Object.freeze({
  EPERM: 403,
  ENOENT: 404,
  EINTR: 500,
  EIO: 500,
  ENXIO: 403,
  EBADF: 500,
  EAGAIN: 503,
  ENOMEM: 503,
  EACCES: 403,
  EBUSY: 409,
  EEXIST: 409,
  EXDEV: 502,
  ENODEV: 404,
  ENOTDIR: 409,
  EISDIR: 405,
  EINVAL: 400,
  ENFILE: 503,
  EMFILE: 503,
  EFBIG: 413,
  ENOSPC: 507,
  ESPIPE: 500,
  EROFS: 403,
  EMLINK: 403,
  ERANGE: 500,
  ENAMETOOLONG: 414,
  ENOSYS: 501,
  ENOTEMPTY: 409,
  ELOOP: 508,
  ENODATA: 404,
  EPROTO: 500,
  EOVERFLOW: 500,
  ENOTSUP: 501,
  ESTALE: 404,
  EDQUOT: 507,
})

const WEBDAV_METHODS = Object.freeze([
  "OPTIONS",
  "HEAD",
  "GET",
  "PUT",
  "DELETE",
  "MKCOL",
  "COPY",
  "MOVE",
  "PROPFIND",
  "PROPPATCH",
  "LOCK",
  "UNLOCK",
])

const ALLPROP_NAMES = Object.freeze([
  "creationdate",
  "displayname",
  "getcontentlength",
  "getcontenttype",
  "getetag",
  "getlastmodified",
  "resourcetype",
  "supportedlock",
  "lockdiscovery",
])

const QUOTA_NAMES = Object.freeze(["quota-available-bytes", "quota-used-bytes"])

function resourceETag(stats) {
  return createHash("sha256")
    .update(`${stats.dev}:${stats.ino}:${stats.size}:${stats.mtimeMs}`)
    .digest("hex")
    .slice(0, 32)
}

class WebdavBindError extends Error {
  constructor(host, message) {
    super(message)
    this.name = "WebdavBindError"
    this.code = "ERR_WEBDAV_BIND"
    this.host = host
  }
}

function isWebdavBindError(error) {
  return error instanceof WebdavBindError
}

function isLoopbackHost(host) {
  const lower = host.toLowerCase()
  const bare = /^\[(.*)]$/.exec(lower)?.[1] ?? lower
  if (bare === "localhost" || bare === "::1") return true
  if (bare.startsWith("::ffff:")) return isLoopbackHost(bare.slice(7))
  const octets = /^127\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/.exec(bare)
  return octets !== null && octets.slice(1).every((octet) => Number(octet) <= 255)
}

function bindRefusal(host, credentials) {
  if (credentials || isLoopbackHost(host)) return undefined
  const unspecified = host === "" || host === "0.0.0.0" || host === "::" || host === "[::]"
  return (
    `mountx: refusing to bind a WebDAV server to ${host === "" ? "every interface" : host} ` +
    "with no credentials configured. " +
    (unspecified
      ? "That address binds every interface, so the driver would be readable and writable " +
        "by anything that can reach this machine. "
      : "That address is reachable from outside this machine, and an unauthenticated " +
        "share would serve the driver to whatever finds it. ") +
    "Pass `credentials: { username, password }` to bind it, or leave `host` at " +
    `${DEFAULT_HOST}.`
  )
}

class DavFault extends Error {
  constructor(status, options = {}) {
    super(options.message ?? statusLine(status))
    this.name = "DavFault"
    this.code = "ERR_WEBDAV_FAULT"
    this.status = status
    this.condition = options.condition
    this.hrefs = options.hrefs ?? []
    this.headers = options.headers ?? {}
  }
}

function isDavFault(error) {
  return error instanceof DavFault
}

function refuse(status, options) {
  return new DavFault(status, options)
}

function normalizePath(path) {
  const segments = []
  for (const segment of path.split("/")) {
    if (segment === "" || segment === ".") continue
    if (segment === "..") {
      segments.pop()
    } else {
      segments.push(segment)
    }
  }
  return segments.length === 0 ? "/" : `/${segments.join("/")}`
}

function parseTargetPath(target) {
  const withoutQuery = target.split("?", 1)[0]
  const withoutFragment = withoutQuery.split("#", 1)[0]
  if (!withoutFragment.startsWith("/")) {
    throw refuse(400, { message: `the request target ${target} is not an absolute path` })
  }
  const decoded = []
  for (const raw of withoutFragment.split("/")) {
    if (raw === "") continue
    let segment
    try {
      segment = decodeURIComponent(raw)
    } catch {
      throw refuse(400, { message: `the request target ${target} does not name a resource` })
    }
    if (segment.includes("/") || segment.includes("\0")) {
      throw refuse(400, { message: `the request target ${target} does not name a resource` })
    }
    decoded.push(segment)
  }
  return normalizePath(`/${decoded.join("/")}`)
}

function hrefOf(path, collection) {
  const href = `/${normalizePath(path).split("/").filter(Boolean).map(encodeURIComponent).join("/")}`
  return collection && href !== "/" ? `${href}/` : href
}

function parseDepth(value, fallback) {
  if (value === undefined) return fallback
  const depth = value.trim().toLowerCase()
  if (depth === "0") return 0
  if (depth === "1") return 1
  return depth === "infinity" ? "infinity" : undefined
}

function parseOverwrite(value) {
  if (value === undefined) return true
  const flag = value.trim().toUpperCase()
  if (flag === "T") return true
  return flag === "F" ? false : undefined
}

function parseDestination(value, host) {
  if (value === undefined || value.trim() === "") {
    throw refuse(400, { message: "the Destination header is required" })
  }
  const destination = value.trim()
  if (destination.startsWith("/")) return parseTargetPath(destination)
  let url
  try {
    url = new URL(destination)
  } catch {
    throw refuse(400, { message: `the Destination header ${destination} is not a URI` })
  }
  if (url.host === "" || host === undefined || url.host.toLowerCase() !== host.toLowerCase()) {
    throw refuse(502, { message: `the Destination ${destination} is not on this server` })
  }
  return parseTargetPath(url.pathname)
}

function parseTimeout(value) {
  if (value === undefined) return undefined
  const first = value.split(",", 1)[0].trim()
  if (first.toLowerCase() === "infinite") return "infinite"
  const seconds = /^[Ss]econd-(\d+)$/.exec(first)
  return seconds === null ? undefined : Number(seconds[1])
}

function parseLockToken(value) {
  if (value === undefined) return undefined
  const coded = /^<([^<>]+)>$/.exec(value.trim())
  return coded === null ? undefined : coded[1]
}

function formatLockToken(token) {
  return `<${token}>`
}

function resourceTag(reference, host) {
  const trimmed = reference.trim()
  try {
    if (trimmed.startsWith("/")) {
      return { resource: parseTargetPath(trimmed), foreign: false }
    }
    const url = new URL(trimmed)
    if (url.host === "" || host === undefined || url.host.toLowerCase() !== host.toLowerCase()) {
      return { resource: undefined, foreign: true }
    }
    return { resource: parseTargetPath(url.pathname), foreign: false }
  } catch {
    return { resource: undefined, foreign: true }
  }
}

function entityTagEnd(value, from) {
  let quoted = false
  for (let index = from; index < value.length; index += 1) {
    const character = value[index]
    if (character === '"') {
      quoted = !quoted
    } else if (character === "]" && !quoted) {
      return index
    }
  }
  return -1
}

function parseIfList(value, start) {
  const conditions = []
  let at = start + 1
  let negated = false
  while (at < value.length) {
    const character = value[at]
    if (/\s/.test(character)) {
      at += 1
      continue
    }
    if (character === ")") {
      return conditions.length === 0 ? undefined : { conditions, at: at + 1 }
    }
    if (value.slice(at, at + 3).toLowerCase() === "not") {
      negated = true
      at += 3
      continue
    }
    if (character === "<") {
      const close = value.indexOf(">", at + 1)
      if (close === -1) return undefined
      conditions.push({ negated, token: value.slice(at + 1, close).trim() })
      negated = false
      at = close + 1
      continue
    }
    if (character !== "[") return undefined
    const close = entityTagEnd(value, at + 1)
    if (close === -1) return undefined
    conditions.push({ negated, etag: value.slice(at + 1, close).trim() })
    negated = false
    at = close + 1
  }
  return undefined
}

function parseIf(value, host) {
  const lists = []
  let tag
  let at = 0
  while (at < value.length) {
    const character = value[at]
    if (/\s/.test(character)) {
      at += 1
      continue
    }
    if (character === "<") {
      const close = value.indexOf(">", at + 1)
      if (close === -1) return undefined
      tag = resourceTag(value.slice(at + 1, close), host)
      at = close + 1
      continue
    }
    if (character !== "(") return undefined
    const list = parseIfList(value, at)
    if (list === undefined) return undefined
    lists.push({
      resource: tag?.resource,
      foreign: tag?.foreign ?? false,
      conditions: list.conditions,
    })
    at = list.at
  }
  return lists.length === 0 ? undefined : lists
}

function submittedTokens(lists) {
  const tokens = []
  for (const list of lists) {
    for (const condition of list.conditions) {
      if (condition.token !== undefined && !condition.negated && !tokens.includes(condition.token)) {
        tokens.push(condition.token)
      }
    }
  }
  return tokens
}

const XML_DECLARATION = '<?xml version="1.0" encoding="UTF-8"?>'
const XML_REPLACEMENT = String.fromCodePoint(0xfffd)
const NO_BODY = {
  [Symbol.asyncIterator]: () => ({
    next: async () => ({ done: true, value: undefined }),
  }),
}

function isXmlChar(code) {
  if (code < 0x20) return code === 0x09 || code === 0x0a || code === 0x0d
  if (code < 0x7f) return true
  if (code <= 0x9f) return false
  if (code >= 0xd800 && code <= 0xdfff) return false
  return (code & 0xfffe) !== 0xfffe
}

function escapeXmlText(value) {
  let escaped = ""
  for (const character of value) {
    if (character === "&") {
      escaped += "&amp;"
    } else if (character === "<") {
      escaped += "&lt;"
    } else if (character === ">") {
      escaped += "&gt;"
    } else if (character === '"') {
      escaped += "&quot;"
    } else if (character === "'") {
      escaped += "&apos;"
    } else if (character === "\r") {
      escaped += "&#13;"
    } else {
      escaped += isXmlChar(character.codePointAt(0) ?? 0) ? character : XML_REPLACEMENT
    }
  }
  return escaped
}

function renderXmlText(value) {
  return typeof value === "string" ? value : String(value)
}

function renderXmlElement(node, inherited) {
  const declaring = node.ns !== undefined && node.ns !== inherited
  const ns = declaring ? node.ns : inherited
  let inner = node.text === undefined ? "" : escapeXmlText(renderXmlText(node.text))
  for (const child of node.children ?? []) {
    if (child !== undefined) inner += renderXmlElement(child, ns)
  }
  const attributes = declaring ? ` xmlns="${escapeXmlText(ns)}"` : ""
  return `<${node.name}${attributes}>${inner}</${node.name}>`
}

function xmlDocument(root, options = {}) {
  const node = options.xmlns === undefined ? root : { ...root, ns: options.xmlns }
  return XML_DECLARATION + renderXmlElement(node, "")
}

function encodeMultistatus(entries) {
  return xmlDocument(
    {
      name: "multistatus",
      children: entries.map((entry) => ({
        name: "response",
        children: [
          { name: "href", text: entry.href },
          ...(entry.propstat ?? []).map((propstat) => ({
            name: "propstat",
            children: [
              { name: "prop", children: propstat.props },
              { name: "status", text: statusLine(propstat.status) },
              propstat.condition === undefined
                ? undefined
                : { name: "error", children: [{ name: propstat.condition }] },
            ],
          })),
          entry.status === undefined ? undefined : { name: "status", text: statusLine(entry.status) },
        ],
      })),
    },
    { xmlns: "DAV:" },
  )
}

function encodeErrorDocument(condition, hrefs = []) {
  return xmlDocument(
    {
      name: "error",
      children: [{ name: condition, children: hrefs.map((href) => ({ name: "href", text: href })) }],
    },
    { xmlns: "DAV:" },
  )
}

async function collectBody(body, limit = 256 * 1024) {
  const chunks = []
  let total = 0
  for await (const chunk of body) {
    total += chunk.byteLength
    if (total > limit) throw refuse(413, { message: `the request body is over the ${limit}-byte budget` })
    chunks.push(Uint8Array.prototype.slice.call(chunk))
  }
  const buffer = new Uint8Array(total)
  let at = 0
  for (const chunk of chunks) {
    buffer.set(chunk, at)
    at += chunk.byteLength
  }
  return buffer
}

function statusOfError(error) {
  if (isDavFault(error)) return error.status
  if (typeof error === "object" && error !== null) {
    const code = error.code
    return statusOf(typeof code === "string" ? code : undefined)
  }
  return statusOf(undefined)
}

function xmlBody(status, document, extra = {}) {
  const body = Buffer.from(document, "utf8")
  return {
    status,
    headers: {
      ...extra,
      "content-type": 'application/xml; charset="utf-8"',
      "content-length": String(body.byteLength),
    },
    body,
  }
}

function faultResponse(error) {
  const status = statusOfError(error)
  const condition = isDavFault(error) ? error.condition : undefined
  const extra = isDavFault(error) ? error.headers : {}
  if (condition === undefined) {
    return { status, headers: { ...extra, "content-length": "0" } }
  }
  const hrefs = isDavFault(error) ? error.hrefs : []
  return xmlBody(status, encodeErrorDocument(condition, hrefs), extra)
}

function supportedLockNode() {
  return {
    name: "supportedlock",
    children: [
      {
        name: "lockentry",
        children: [
          { name: "lockscope", children: [{ name: "exclusive" }] },
          { name: "locktype", children: [{ name: "write" }] },
        ],
      },
      {
        name: "lockentry",
        children: [
          { name: "lockscope", children: [{ name: "shared" }] },
          { name: "locktype", children: [{ name: "write" }] },
        ],
      },
    ],
  }
}

const XML_MAX_BYTES = 4 * 1024 * 1024
const XML_MAX_DEPTH = 32
const XML_MAX_DEPTH_CEILING = 256
const XML_MAX_ELEMENTS = 100_000
const XML_PREFIX_NS = "http://www.w3.org/XML/1998/namespace"

class XmlError extends Error {
  constructor(reason, message, offset) {
    super(message)
    this.name = "XmlError"
    this.code = "ERR_S3_XML"
    this.reason = reason
    this.offset = offset
  }
}

function isXmlError(error) {
  return error instanceof XmlError
}

function findInvalidXmlChar(text) {
  for (let index = 0; index < text.length; index += 1) {
    const code = text.codePointAt(index)
    if (!isXmlChar(code)) return index
    if (code > 0xffff) index += 1
  }
  return -1
}

function xmlBodyText(input, maxBytes) {
  const size = typeof input === "string" ? input.length : input.byteLength
  if (size > maxBytes) {
    throw new XmlError("too-large", `body is ${size} bytes, over the ${maxBytes}-byte budget`)
  }
  let text
  if (typeof input === "string") {
    text = input
  } else {
    try {
      text = new TextDecoder("utf-8", { fatal: true }).decode(input)
    } catch {
      throw new XmlError("encoding", "body is not valid UTF-8")
    }
  }
  if (text.charCodeAt(0) === 0xfeff) text = text.slice(1)
  const invalid = findInvalidXmlChar(text)
  if (invalid !== -1) {
    throw new XmlError("invalid-character", "body contains a character XML cannot carry", invalid)
  }
  return text
}

function xmlCap(asked, fallback, ceiling = Number.MAX_SAFE_INTEGER) {
  return Math.min(Number.isFinite(asked) ? asked : fallback, ceiling)
}

const XML_SPACE = new Set([0x20, 0x09, 0x0a, 0x0d])

function isXmlNameStart(code) {
  return (code >= 0x41 && code <= 0x5a) || (code >= 0x61 && code <= 0x7a) || code === 0x5f || code === 0x3a || code >= 0x80
}

function isXmlNameChar(code) {
  return isXmlNameStart(code) || (code >= 0x30 && code <= 0x39) || code === 0x2d || code === 0x2e
}

function xmlScope(inherited, declarations) {
  if (declarations === undefined) return inherited
  const scope = new Map(inherited)
  for (const { prefix, uri } of declarations) {
    if (prefix === "xmlns" || prefix === "xml") continue
    if (uri === "") scope.delete(prefix)
    else scope.set(prefix, uri)
  }
  return scope
}

function xmlNamespaceOf(qname, scope) {
  const colon = qname.lastIndexOf(":")
  return scope.get(colon === -1 ? "" : qname.slice(0, colon)) ?? ""
}

function xmlLocalName(qname) {
  const colon = qname.lastIndexOf(":")
  return colon === -1 ? qname : qname.slice(colon + 1)
}

class XmlParser {
  constructor(text, limits) {
    this.text = text
    this.maxDepth = xmlCap(limits.maxDepth, XML_MAX_DEPTH, XML_MAX_DEPTH_CEILING)
    this.maxElements = xmlCap(limits.maxElements, XML_MAX_ELEMENTS)
    this.at = 0
    this.elements = 0
  }

  fail(reason, message) {
    return new XmlError(reason, message, this.at)
  }

  starts(prefix) {
    return this.text.startsWith(prefix, this.at)
  }

  skipSpace() {
    while (XML_SPACE.has(this.text.charCodeAt(this.at))) this.at += 1
  }

  skipMisc() {
    for (;;) {
      this.skipSpace()
      if (this.starts("<!--")) {
        this.skipComment()
        continue
      }
      if (this.starts("<?")) {
        this.skipProcessingInstruction()
        continue
      }
      if (this.starts("<!")) throw this.markupRefusal()
      return
    }
  }

  skipComment() {
    const end = this.text.indexOf("-->", this.at + 4)
    if (end === -1) throw this.fail("malformed", "unterminated comment")
    this.at = end + 3
  }

  skipProcessingInstruction() {
    const end = this.text.indexOf("?>", this.at + 2)
    if (end === -1) throw this.fail("malformed", "unterminated processing instruction")
    this.at = end + 2
  }

  markupRefusal() {
    return this.text.slice(this.at, this.at + 9).toUpperCase() === "<!DOCTYPE"
      ? this.fail("doctype", "a DOCTYPE declaration is never processed")
      : this.fail("malformed", "unsupported markup declaration")
  }

  qname() {
    const start = this.at
    if (!isXmlNameStart(this.text.charCodeAt(this.at))) {
      throw this.fail("malformed", "expected an element or attribute name")
    }
    this.at += 1
    while (isXmlNameChar(this.text.charCodeAt(this.at))) this.at += 1
    return this.text.slice(start, this.at)
  }

  attribute() {
    const qname = this.qname()
    const declaring = qname === "xmlns" || qname.startsWith("xmlns:")
    this.skipSpace()
    if (this.text.charCodeAt(this.at) !== 0x3d) throw this.fail("malformed", "expected = after an attribute name")
    this.at += 1
    this.skipSpace()
    const quote = this.text.charCodeAt(this.at)
    if (quote !== 0x22 && quote !== 0x27) throw this.fail("malformed", "attribute value is not quoted")
    this.at += 1
    let uri = ""
    for (;;) {
      const start = this.at
      for (;;) {
        const code = this.text.charCodeAt(this.at)
        if (Number.isNaN(code)) throw this.fail("malformed", "unterminated attribute value")
        if (code === quote || code === 0x26) break
        if (code === 0x3c) throw this.fail("malformed", "< in an attribute value")
        this.at += 1
      }
      if (declaring) uri += this.text.slice(start, this.at)
      if (this.text.charCodeAt(this.at) === quote) {
        this.at += 1
        return declaring ? { prefix: qname === "xmlns" ? "" : qname.slice(6), uri } : undefined
      }
      const resolved = this.entity()
      if (declaring) uri += resolved
    }
  }

  entity() {
    const semicolon = this.text.indexOf(";", this.at + 1)
    if (semicolon === -1 || semicolon - this.at > 13) throw this.fail("entity", "unterminated entity reference")
    const body = this.text.slice(this.at + 1, semicolon)
    this.at = semicolon + 1
    const predefined = { amp: "&", lt: "<", gt: ">", quot: '"', apos: "'" }
    if (predefined[body] !== undefined) return predefined[body]
    if (!body.startsWith("#")) throw this.fail("entity", `&${body}; is not one of the five predefined entities`)
    const hex = body.startsWith("#x") || body.startsWith("#X")
    const digits = body.slice(hex ? 2 : 1)
    const shaped = hex ? /^[\da-f]{1,10}$/i.test(digits) : /^\d{1,10}$/.test(digits)
    if (!shaped) throw this.fail("entity", `&${body}; is not a character reference`)
    const code = Number.parseInt(digits, hex ? 16 : 10)
    if (code > 0x10ffff || !isXmlChar(code)) throw this.fail("entity", `&${body}; is not a character this codec carries`)
    return String.fromCodePoint(code)
  }

  textRun() {
    let value = ""
    for (;;) {
      const start = this.at
      while (this.at < this.text.length) {
        const code = this.text.charCodeAt(this.at)
        if (code === 0x3c || code === 0x26) break
        this.at += 1
      }
      value += this.text.slice(start, this.at)
      if (this.text.charCodeAt(this.at) !== 0x26) return value
      value += this.entity()
    }
  }

  element(depth, inherited) {
    if (depth > this.maxDepth) throw this.fail("depth", `nested deeper than ${this.maxDepth} elements`)
    this.elements += 1
    if (this.elements > this.maxElements) throw this.fail("too-many-elements", `body has more than ${this.maxElements} elements`)
    this.at += 1
    const qname = this.qname()
    let declarations
    let empty = false
    for (;;) {
      const spaced = XML_SPACE.has(this.text.charCodeAt(this.at))
      this.skipSpace()
      const code = this.text.charCodeAt(this.at)
      if (code === 0x3e) {
        this.at += 1
        break
      }
      if (code === 0x2f) {
        if (this.text.charCodeAt(this.at + 1) !== 0x3e) throw this.fail("malformed", "expected /> to close an empty element")
        this.at += 2
        empty = true
        break
      }
      if (Number.isNaN(code)) throw this.fail("malformed", `unterminated <${qname}> start tag`)
      if (!spaced) throw this.fail("malformed", "expected whitespace before an attribute")
      const declaration = this.attribute()
      if (declaration !== undefined) (declarations ??= []).push(declaration)
    }
    const scope = xmlScope(inherited, declarations)
    const element = { name: xmlLocalName(qname), ns: xmlNamespaceOf(qname, scope), text: "", children: [] }
    if (!empty) this.content(element, qname, depth, scope)
    return element
  }

  content(element, qname, depth, scope) {
    for (;;) {
      const before = this.at
      if (this.at >= this.text.length) throw this.fail("malformed", `unterminated <${qname}>`)
      if (this.starts("</")) {
        this.at += 2
        const closing = this.qname()
        this.skipSpace()
        if (this.text.charCodeAt(this.at) !== 0x3e) throw this.fail("malformed", `unterminated </${closing}>`)
        this.at += 1
        if (closing !== qname) throw this.fail("malformed", `</${closing}> closes <${qname}>`)
        return
      } else if (this.starts("<!--")) {
        this.skipComment()
      } else if (this.starts("<![CDATA[")) {
        const end = this.text.indexOf("]]>", this.at + 9)
        if (end === -1) throw this.fail("malformed", "unterminated CDATA section")
        element.text += this.text.slice(this.at + 9, end)
        this.at = end + 3
      } else if (this.starts("<!")) {
        throw this.markupRefusal()
      } else if (this.starts("<?")) {
        this.skipProcessingInstruction()
      } else if (this.text.charCodeAt(this.at) === 0x3c) {
        element.children.push(this.element(depth + 1, scope))
      } else {
        element.text += this.textRun()
      }
      if (this.at === before) throw this.fail("malformed", "the parser made no progress")
    }
  }

  parse() {
    this.skipMisc()
    if (this.text.charCodeAt(this.at) !== 0x3c) throw this.fail("malformed", "no root element")
    const root = this.element(1, new Map([["xml", XML_PREFIX_NS]]))
    this.skipMisc()
    if (this.at < this.text.length) throw this.fail("malformed", "trailing content after the root element")
    return root
  }
}

function parseXml(input, limits = {}) {
  return new XmlParser(xmlBodyText(input, xmlCap(limits.maxBytes, XML_MAX_BYTES)), limits).parse()
}

function toXmlNode(element) {
  return {
    name: element.name,
    ns: element.ns,
    text: element.text === "" ? undefined : element.text,
    children: element.children.map((child) => toXmlNode(child)),
  }
}

function parseDocument(body, rootName) {
  let parsed
  try {
    parsed = parseXml(body, { maxBytes: 256 * 1024 })
  } catch (error) {
    if (isXmlError(error)) throw refuse(400, { message: `the request body is not usable XML: ${error.message}` })
    throw error
  }
  if (parsed.name !== rootName) throw refuse(400, { message: `expected a ${rootName} document, got ${parsed.name}` })
  return parsed
}

function samePropertyName(left, right) {
  return left.ns === right.ns && left.name === right.name
}

function davProperty(name) {
  return { ns: "DAV:", name }
}

function parsePropfind(body) {
  if (body.byteLength === 0) return { kind: "allprop" }
  const root = parseDocument(body, "propfind")
  const names = []
  let sawProp = false
  for (const child of root.children) {
    if (child.name === "propname") return { kind: "propname" }
    if (child.name === "allprop") return { kind: "allprop" }
    if (child.name === "prop") {
      sawProp = true
      for (const property of child.children) {
        const asked = { ns: property.ns, name: property.name }
        if (!names.some((seen) => samePropertyName(seen, asked))) names.push(asked)
      }
    }
  }
  if (!sawProp) throw refuse(400, { message: "a propfind body must hold propname, allprop or prop" })
  return { kind: "prop", names }
}

function parseProppatch(body) {
  const root = parseDocument(body, "propertyupdate")
  const set = []
  const remove = []
  for (const child of root.children) {
    const setting = child.name === "set"
    if (!setting && child.name !== "remove") continue
    for (const prop of child.children) {
      if (prop.name !== "prop") continue
      for (const property of prop.children) {
        const named = { ns: property.ns, name: property.name }
        if (setting) set.push({ name: named, text: property.text })
        else remove.push(named)
      }
    }
  }
  if (set.length === 0 && remove.length === 0) {
    throw refuse(400, { message: "a propertyupdate body must name at least one property" })
  }
  return { set, remove }
}

function parseLockInfo(body) {
  if (body.byteLength === 0) return undefined
  const root = parseDocument(body, "lockinfo")
  let exclusive
  let write = false
  let owner
  for (const child of root.children) {
    if (child.name === "lockscope") {
      exclusive = child.children.some((scope) => scope.name === "exclusive")
        ? true
        : child.children.some((scope) => scope.name === "shared")
          ? false
          : undefined
    } else if (child.name === "locktype") {
      write = child.children.some((type) => type.name === "write")
    } else if (child.name === "owner") {
      owner = toXmlNode(child)
    }
  }
  if (exclusive === undefined) throw refuse(400, { message: "a lockinfo body needs a lockscope of exclusive or shared" })
  if (!write) throw refuse(400, { message: "write is the only lock type this server has (RFC 4918 §7)" })
  return { exclusive, owner }
}

function pathInside(path, parent) {
  const normalizedPath = normalizePath(path)
  const normalizedParent = normalizePath(parent)
  return normalizedPath === normalizedParent ||
    (normalizedParent === "/" && normalizedPath.startsWith("/")) ||
    normalizedPath.startsWith(`${normalizedParent}/`)
}

class DavLockTable {
  constructor(options = {}) {
    this.locks = new Map()
    this.defaultTimeout = options.defaultTimeoutSeconds ?? 600
    this.maxTimeout = options.maxTimeoutSeconds ?? 3600
    this.maxLocks = options.maxLocks ?? 4096
    this.newToken = options.newToken ?? (() => `urn:uuid:${randomUUID()}`)
  }

  size(now) {
    this.sweep(now)
    return this.locks.size
  }

  all(now) {
    this.sweep(now)
    return [...this.locks.values()]
  }

  covering(path, now) {
    this.sweep(now)
    return [...this.locks.values()].filter((lock) =>
      lock.path === path || (lock.depth === "infinity" && pathInside(path, lock.path)))
  }

  within(path, now) {
    this.sweep(now)
    return [...this.locks.values()].filter((lock) => pathInside(lock.path, path))
  }

  find(token, now) {
    this.sweep(now)
    return this.locks.get(token)
  }

  static inScope(lock, path) {
    return lock.path === path || (lock.depth === "infinity" && pathInside(path, lock.path))
  }

  conflict(path, depth, exclusive, now) {
    const candidates = depth === "infinity"
      ? [...this.covering(path, now), ...this.within(path, now)]
      : this.covering(path, now)
    return candidates.find((lock) => exclusive || lock.exclusive)
  }

  create(request, now) {
    const conflict = this.conflict(request.path, request.depth, request.exclusive, now)
    if (conflict !== undefined) return { kind: "conflict", lock: conflict }
    if (this.size(now) >= this.maxLocks) return { kind: "full" }
    const timeoutSeconds = this.grantedTimeout(request.timeoutSeconds)
    const lock = {
      token: this.newToken(),
      path: request.path,
      collection: request.collection,
      depth: request.depth,
      exclusive: request.exclusive,
      owner: request.owner,
      timeoutSeconds,
      expiresAt: now + timeoutSeconds * 1000,
    }
    this.locks.set(lock.token, lock)
    return { kind: "granted", lock }
  }

  refresh(token, requested, now) {
    const existing = this.find(token, now)
    if (existing === undefined) return undefined
    const timeoutSeconds = this.grantedTimeout(requested)
    const refreshed = { ...existing, timeoutSeconds, expiresAt: now + timeoutSeconds * 1000 }
    this.locks.set(token, refreshed)
    return refreshed
  }

  remove(token) {
    return this.locks.delete(token)
  }

  static remaining(lock, now) {
    return Math.max(0, Math.floor((lock.expiresAt - now) / 1000))
  }

  grantedTimeout(requested) {
    if (requested === undefined) return this.defaultTimeout
    if (requested === "infinite") return this.maxTimeout
    return Math.max(1, Math.min(Math.trunc(requested), this.maxTimeout))
  }

  sweep(now) {
    for (const [token, lock] of this.locks) {
      if (lock.expiresAt <= now) this.locks.delete(token)
    }
  }
}

function activeLockNode(lock, now) {
  return {
    name: "activelock",
    children: [
      { name: "lockscope", children: [{ name: lock.exclusive ? "exclusive" : "shared" }] },
      { name: "locktype", children: [{ name: "write" }] },
      { name: "depth", text: String(lock.depth) },
      lock.owner,
      { name: "timeout", text: `Second-${DavLockTable.remaining(lock, now)}` },
      { name: "locktoken", children: [{ name: "href", text: lock.token }] },
      { name: "lockroot", children: [{ name: "href", text: hrefOf(lock.path, lock.collection) }] },
    ],
  }
}

function lockDiscoveryNode(locks, now) {
  return { name: "lockdiscovery", children: locks.map((lock) => activeLockNode(lock, now)) }
}

function encodeLockResponse(lock, now) {
  return xmlDocument(
    { name: "prop", children: [lockDiscoveryNode([lock], now)] },
    { xmlns: "DAV:" },
  )
}

function statusLine(status) {
  const phrase = STATUS_TEXT[status]
  return phrase === undefined ? `HTTP/1.1 ${status}` : `HTTP/1.1 ${status} ${phrase}`
}

function statusOf(code) {
  return code !== undefined && Object.hasOwn(ERRNO_STATUS, code)
    ? ERRNO_STATUS[code]
    : 500
}

module.exports = binding

// Keep every public alias as a direct assignment for Node's CommonJS-to-ESM
// named-export detector. Root identities are copied above so a consumer can
// mix root and ./webdav imports safely.
module.exports.WebdavServer = root.WebdavServer
module.exports.WebdavSession = root.WebdavSession
module.exports.createWebdavServer = root.createWebdavServer
module.exports.WebdavBindError = WebdavBindError
module.exports.isWebdavBindError = isWebdavBindError
module.exports.isLoopbackHost = isLoopbackHost
module.exports.bindRefusal = bindRefusal
module.exports.ALLPROP_NAMES = ALLPROP_NAMES
module.exports.QUOTA_NAMES = QUOTA_NAMES
module.exports.resourceETag = resourceETag
module.exports.DavFault = DavFault
module.exports.isDavFault = isDavFault
module.exports.refuse = refuse
module.exports.parseTargetPath = parseTargetPath
module.exports.hrefOf = hrefOf
module.exports.parseDepth = parseDepth
module.exports.parseOverwrite = parseOverwrite
module.exports.parseDestination = parseDestination
module.exports.parseTimeout = parseTimeout
module.exports.parseLockToken = parseLockToken
module.exports.formatLockToken = formatLockToken
module.exports.parseIf = parseIf
module.exports.submittedTokens = submittedTokens
module.exports.encodeMultistatus = encodeMultistatus
module.exports.encodeErrorDocument = encodeErrorDocument
module.exports.supportedLockNode = supportedLockNode
module.exports.XmlError = XmlError
module.exports.isXmlError = isXmlError
module.exports.samePropertyName = samePropertyName
module.exports.davProperty = davProperty
module.exports.parsePropfind = parsePropfind
module.exports.parseProppatch = parseProppatch
module.exports.parseLockInfo = parseLockInfo
module.exports.DavLockTable = DavLockTable
module.exports.activeLockNode = activeLockNode
module.exports.lockDiscoveryNode = lockDiscoveryNode
module.exports.encodeLockResponse = encodeLockResponse
module.exports.NO_BODY = NO_BODY
module.exports.collectBody = collectBody
module.exports.statusOfError = statusOfError
module.exports.faultResponse = faultResponse
module.exports.xmlBody = xmlBody
module.exports.DAV_NS = "DAV:"
module.exports.DEFAULT_HOST = DEFAULT_HOST
module.exports.DEFAULT_DRAIN_TIMEOUT = 5000
module.exports.DAV_COMPLIANCE = "1, 2, 3"
module.exports.MS_AUTHOR_VIA = "DAV"
module.exports.WEBDAV_METHODS = WEBDAV_METHODS
module.exports.ALLOW_HEADER = WEBDAV_METHODS.join(", ")
module.exports.RESOURCE_CONTENT_TYPE = "application/octet-stream"
module.exports.COLLECTION_CONTENT_TYPE = "httpd/unix-directory"
module.exports.XML_CONTENT_TYPE = 'application/xml; charset="utf-8"'
module.exports.MAX_XML_BYTES = 256 * 1024
module.exports.MAX_XML_DEPTH = 32
module.exports.MAX_XML_ELEMENTS = 100_000
module.exports.DEFAULT_MAX_REQUEST_BYTES = 64 * 1024 * 1024
module.exports.READ_CHUNK_BYTES = 128 * 1024
module.exports.LOCK_TOKEN_PREFIX = "urn:uuid:"
module.exports.DEFAULT_LOCK_TIMEOUT_SECONDS = 600
module.exports.MAX_LOCK_TIMEOUT_SECONDS = 3600
module.exports.MAX_LOCKS = 4096
module.exports.STATUS_TEXT = STATUS_TEXT
module.exports.ERRNO_STATUS = ERRNO_STATUS
module.exports.UNKNOWN_STATUS = 500
module.exports.statusLine = statusLine
module.exports.statusOf = statusOf
