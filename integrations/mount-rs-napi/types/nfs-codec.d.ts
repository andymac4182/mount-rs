/** NFS XDR and ONC-RPC codec surface exposed by the ./nfs subpath. */

export { NfsServer, createNfsServer } from "../index.js"
export type { NfsServerOptions } from "../index.js"

export class XdrError extends Error {
  readonly code: "ERR_NFS_XDR"
  readonly offset: number | undefined
  constructor(message: string, options?: { offset?: number; cause?: unknown })
}

export function isXdrError(error: unknown): error is XdrError

export const XDR_MAX_ITEM: 16777216
export function xdrPad(length: number): number
export function xdrAlign(length: number): number
export function stringByteLength(value: string): number

export class XdrReader {
  readonly bytes: Uint8Array
  constructor(bytes: Uint8Array, offset?: number)
  readonly offset: number
  readonly remaining: number
  readonly atEnd: boolean
  u32(what?: string): number
  i32(what?: string): number
  u64(what?: string): bigint
  i64(what?: string): bigint
  bool(what?: string): boolean
  fixedOpaque(length: number, what?: string): Uint8Array
  varOpaque(max?: number, what?: string): Uint8Array
  string(max?: number, what?: string): string
  optional<T>(read: (reader: XdrReader) => T, what?: string): T | undefined
  array<T>(read: (reader: XdrReader) => T, max?: number, what?: string): T[]
  list<T>(read: (reader: XdrReader) => T, max?: number, what?: string): T[]
  rest(): Uint8Array
  end(what?: string): void
}

export class XdrWriter {
  constructor(capacity?: number)
  readonly length: number
  ensure(count: number): this
  truncate(length: number): this
  u32(value: number): this
  i32(value: number): this
  u64(value: bigint): this
  i64(value: bigint): this
  bool(value: boolean): this
  fixedOpaque(value: Uint8Array, length?: number): this
  varOpaque(value: Uint8Array): this
  string(value: string): this
  optional<T>(value: T | undefined, write: (writer: XdrWriter, value: T) => void): this
  array<T>(values: readonly T[], write: (writer: XdrWriter, value: T) => void): this
  list<T>(values: readonly T[], write: (writer: XdrWriter, value: T) => void): this
  raw(value: Uint8Array): this
  bytes(): Uint8Array
  view(): Uint8Array
}

export function encodeXdr(write: (writer: XdrWriter) => void, capacity?: number): Uint8Array
export function decodeXdr<T>(
  bytes: Uint8Array,
  read: (reader: XdrReader) => T,
  what?: string,
): T

export interface OpaqueAuth {
  flavor: number
  body: Uint8Array
}

export const AUTH_NULL: OpaqueAuth

export interface AuthSysParams {
  stamp: number
  machineName: string
  uid: number
  gid: number
  gids: number[]
}

export function encodeAuthSys(params: AuthSysParams): Uint8Array
export function decodeAuthSys(body: Uint8Array): AuthSysParams
export function authSys(
  uid?: number,
  gid?: number,
  machineName?: string,
): OpaqueAuth

export interface RpcCredentials {
  flavor: number
  uid: number | undefined
  gid: number | undefined
  gids: readonly number[]
}

export function credentialsOf(cred: OpaqueAuth): RpcCredentials

export interface RpcCall {
  xid: number
  rpcVersion: number
  program: number
  version: number
  procedure: number
  cred: OpaqueAuth
  verf: OpaqueAuth
}

export function decodeCall(bytes: Uint8Array): { call: RpcCall; args: XdrReader }

export interface RpcCallOptions {
  xid: number
  program: number
  version: number
  procedure: number
  cred?: OpaqueAuth
  verf?: OpaqueAuth
  args?: Uint8Array
}

export function encodeCall(options: RpcCallOptions): Uint8Array

export interface RpcReply {
  xid: number
  replyStat: number
  acceptStat: number | undefined
  rejectStat: number | undefined
  authStat: number | undefined
  low: number | undefined
  high: number | undefined
  verf: OpaqueAuth | undefined
}

export function decodeReply(bytes: Uint8Array): { reply: RpcReply; results: XdrReader }

export const RPC_VERSION: 2
export const RPC_CALL: 0
export const RPC_REPLY: 1
export const MSG_ACCEPTED: 0
export const MSG_DENIED: 1
export const RPC_SUCCESS: 0
export const RPC_PROG_UNAVAIL: 1
export const RPC_PROG_MISMATCH: 2
export const RPC_PROC_UNAVAIL: 3
export const RPC_GARBAGE_ARGS: 4
export const RPC_SYSTEM_ERR: 5
export const RPC_MISMATCH: 0
export const RPC_AUTH_ERROR: 1
export const AUTH_OK: 0
export const AUTH_BADCRED: 1
export const AUTH_REJECTEDCRED: 2
export const AUTH_BADVERF: 3
export const AUTH_REJECTEDVERF: 4
export const AUTH_TOOWEAK: 5
export const AUTH_INVALIDRESP: 6
export const AUTH_FAILED: 7
export const AUTH_NONE: 0
export const AUTH_SYS: 1
export const AUTH_SHORT: 2
export const RPC_MAX_AUTH_BYTES: 400
export const RM_LAST_FRAGMENT: 2147483648
export const RM_LENGTH_MASK: 2147483647
export const ACCEPTED_REPLY_HEADER_SIZE: 24
export const DEFAULT_RECORD_LIMIT: 8388608

export function writeAcceptedReplyHeader(writer: XdrWriter, xid: number): XdrWriter
export function encodeAcceptedReply(xid: number, results?: Uint8Array): Uint8Array
export function encodeAcceptError(
  xid: number,
  acceptStat: number,
  mismatch?: { low: number; high: number },
): Uint8Array
export function encodeAuthError(xid: number, authStat: number): Uint8Array
export function encodeRpcMismatch(
  xid: number,
  low?: number,
  high?: number,
): Uint8Array

export function recordMark(length: number): Uint8Array
export function frameRecord(message: Uint8Array): Uint8Array
export function frameFragments(message: Uint8Array, size: number): Uint8Array
export function copyBytes(bytes: Uint8Array, start?: number, end?: number): Uint8Array

/** Rust currently exposes the transport's limit-derived fragment bound. */
export class RecordAssembler {
  constructor(limit?: number)
  readonly pending: number
  push(chunk: Uint8Array): Uint8Array[]
}
