/** Focused low-level constants barrel for the ./webdav entrypoint. */

export * from "../index.js"
import type { JsStats } from "../index.js"

export declare class WebdavBindError extends Error {
  readonly code: "ERR_WEBDAV_BIND"
  readonly host: string
  constructor(host: string, message: string)
}

export declare function isWebdavBindError(error: unknown): error is WebdavBindError
export declare function isLoopbackHost(host: string): boolean
export declare function bindRefusal(host: string, credentials: boolean): string | undefined

export declare const ALLPROP_NAMES: readonly [
  "creationdate",
  "displayname",
  "getcontentlength",
  "getcontenttype",
  "getetag",
  "getlastmodified",
  "resourcetype",
  "supportedlock",
  "lockdiscovery",
]
export declare const QUOTA_NAMES: readonly ["quota-available-bytes", "quota-used-bytes"]
export declare function resourceETag(stats: Pick<JsStats, "dev" | "ino" | "size" | "mtimeMs">): string

export interface DavFaultOptions {
  condition?: string
  hrefs?: readonly string[]
  message?: string
  headers?: Readonly<Record<string, string>>
}

export declare class DavFault extends Error {
  readonly code: "ERR_WEBDAV_FAULT"
  readonly status: number
  readonly condition: string | undefined
  readonly hrefs: readonly string[]
  readonly headers: Readonly<Record<string, string>>
  constructor(status: number, options?: DavFaultOptions)
}

export declare function isDavFault(error: unknown): error is DavFault
export declare function refuse(status: number, options?: DavFaultOptions): DavFault
export type Depth = 0 | 1 | "infinity"
export declare function parseTargetPath(target: string): string
export declare function hrefOf(path: string, collection: boolean): string
export declare function parseDepth(value: string | undefined, fallback: Depth): Depth | undefined
export declare function parseOverwrite(value: string | undefined): boolean | undefined
export declare function parseDestination(value: string | undefined, host: string | undefined): string
export declare function parseTimeout(value: string | undefined): number | "infinite" | undefined
export declare function parseLockToken(value: string | undefined): string | undefined
export declare function formatLockToken(token: string): string
export interface IfCondition {
  negated: boolean
  token?: string
  etag?: string
}
export interface IfList {
  resource: string | undefined
  foreign: boolean
  conditions: IfCondition[]
}
export declare function parseIf(value: string, host: string | undefined): IfList[] | undefined
export declare function submittedTokens(lists: readonly IfList[]): string[]
export type XmlText = string | number | bigint | boolean
export interface XmlNode {
  name: string
  ns?: string
  text?: XmlText
  children?: readonly (XmlNode | undefined)[]
}
export interface Propstat {
  status: number
  props: XmlNode[]
  condition?: string
}
export interface MultistatusEntry {
  href: string
  propstat?: readonly Propstat[]
  status?: number
}
export declare function encodeMultistatus(entries: readonly MultistatusEntry[]): string
export declare function encodeErrorDocument(condition: string, hrefs?: readonly string[]): string
export declare function supportedLockNode(): XmlNode
export type XmlRefusal =
  | "too-large" | "encoding" | "invalid-character" | "malformed" | "doctype"
  | "entity" | "depth" | "too-many-elements" | "unexpected-root"
  | "missing-field" | "duplicate-field" | "invalid-field"
export declare class XmlError extends Error {
  readonly code: "ERR_S3_XML"
  readonly reason: XmlRefusal
  readonly offset: number | undefined
  constructor(reason: XmlRefusal, message: string, offset?: number)
}
export declare function isXmlError(error: unknown): error is XmlError
export interface DavPropertyName {
  ns: string
  name: string
}
export declare function samePropertyName(left: DavPropertyName, right: DavPropertyName): boolean
export declare function davProperty(name: string): DavPropertyName
export type PropfindRequest =
  | { kind: "allprop" }
  | { kind: "propname" }
  | { kind: "prop"; names: DavPropertyName[] }
export declare function parsePropfind(body: Uint8Array): PropfindRequest
export interface ProppatchSet {
  name: DavPropertyName
  text: string
}
export interface ProppatchRequest {
  set: ProppatchSet[]
  remove: DavPropertyName[]
}
export declare function parseProppatch(body: Uint8Array): ProppatchRequest
export interface LockInfoRequest {
  exclusive: boolean
  owner: XmlNode | undefined
}
export declare function parseLockInfo(body: Uint8Array): LockInfoRequest | undefined
export type LockDepth = 0 | "infinity"
export interface DavLock {
  readonly token: string
  readonly path: string
  readonly collection: boolean
  readonly depth: LockDepth
  readonly exclusive: boolean
  readonly owner: XmlNode | undefined
  readonly timeoutSeconds: number
  readonly expiresAt: number
}
export interface DavLockRequest {
  path: string
  collection: boolean
  depth: LockDepth
  exclusive: boolean
  owner?: XmlNode
  timeoutSeconds?: number | "infinite"
}
export type DavLockGrant =
  | { kind: "granted"; lock: DavLock }
  | { kind: "conflict"; lock: DavLock }
  | { kind: "full" }
export interface DavLockTableOptions {
  defaultTimeoutSeconds?: number
  maxTimeoutSeconds?: number
  maxLocks?: number
  newToken?: () => string
}
export declare class DavLockTable {
  constructor(options?: DavLockTableOptions)
  size(now: number): number
  all(now: number): DavLock[]
  covering(path: string, now: number): DavLock[]
  within(path: string, now: number): DavLock[]
  find(token: string, now: number): DavLock | undefined
  static inScope(lock: DavLock, path: string): boolean
  conflict(path: string, depth: LockDepth, exclusive: boolean, now: number): DavLock | undefined
  create(request: DavLockRequest, now: number): DavLockGrant
  refresh(token: string, requested: number | "infinite" | undefined, now: number): DavLock | undefined
  remove(token: string): boolean
  static remaining(lock: DavLock, now: number): number
}
export declare function activeLockNode(lock: DavLock, now: number): XmlNode
export declare function lockDiscoveryNode(locks: readonly DavLock[], now: number): XmlNode
export declare function encodeLockResponse(lock: DavLock, now: number): string
export declare const NO_BODY: AsyncIterable<Uint8Array>
export declare function collectBody(body: AsyncIterable<Uint8Array>, limit?: number): Promise<Uint8Array>
export declare function statusOfError(error: unknown): number
export interface WebdavProtocolResponse {
  status: number
  headers: Record<string, string>
  body?: Uint8Array | AsyncIterable<Uint8Array>
}
export declare function xmlBody(status: number, document: string, extra?: Record<string, string>): WebdavProtocolResponse
export declare function faultResponse(error: unknown): WebdavProtocolResponse

export declare const DAV_NS: "DAV:"
export declare const DEFAULT_HOST: "127.0.0.1"
export declare const DEFAULT_DRAIN_TIMEOUT: 5000
export declare const DAV_COMPLIANCE: "1, 2, 3"
export declare const MS_AUTHOR_VIA: "DAV"
export declare const WEBDAV_METHODS: readonly [
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
]
export type WebdavMethod = (typeof WEBDAV_METHODS)[number]
export declare const ALLOW_HEADER: "OPTIONS, HEAD, GET, PUT, DELETE, MKCOL, COPY, MOVE, PROPFIND, PROPPATCH, LOCK, UNLOCK"
export declare const RESOURCE_CONTENT_TYPE: "application/octet-stream"
export declare const COLLECTION_CONTENT_TYPE: "httpd/unix-directory"
export declare const XML_CONTENT_TYPE: 'application/xml; charset="utf-8"'
export declare const MAX_XML_BYTES: 262144
export declare const MAX_XML_DEPTH: 32
export declare const MAX_XML_ELEMENTS: 100000
export declare const DEFAULT_MAX_REQUEST_BYTES: 67108864
export declare const READ_CHUNK_BYTES: 131072
export declare const LOCK_TOKEN_PREFIX: "urn:uuid:"
export declare const DEFAULT_LOCK_TIMEOUT_SECONDS: 600
export declare const MAX_LOCK_TIMEOUT_SECONDS: 3600
export declare const MAX_LOCKS: 4096
export declare const STATUS_TEXT: Readonly<Record<number, string>>
export declare const ERRNO_STATUS: Readonly<Record<string, number>>
export declare const UNKNOWN_STATUS: 500
export declare function statusLine(status: number): string
export declare function statusOf(code: string | undefined): number
