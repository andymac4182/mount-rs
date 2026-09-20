export type ErrnoCode =
  | "EPERM"
  | "ENOENT"
  | "EINTR"
  | "EIO"
  | "ENXIO"
  | "EBADF"
  | "EAGAIN"
  | "ENOMEM"
  | "EACCES"
  | "EBUSY"
  | "EEXIST"
  | "EXDEV"
  | "ENODEV"
  | "ENOTDIR"
  | "EISDIR"
  | "EINVAL"
  | "ENFILE"
  | "EMFILE"
  | "EFBIG"
  | "ENOSPC"
  | "ESPIPE"
  | "EROFS"
  | "EMLINK"
  | "ERANGE"
  | "ENAMETOOLONG"
  | "ENOSYS"
  | "ENOTEMPTY"
  | "ELOOP"
  | "ENODATA"
  | "EPROTO"
  | "EOVERFLOW"
  | "ENOTSUP"
  | "ESTALE"
  | "EDQUOT"

export interface FsError extends Error {
  code: string
  errno: number
  syscall?: string
  path?: string
  dest?: string
}

export interface FsErrorOptions {
  message?: string
  syscall?: string
  path?: string
  dest?: string
  cause?: unknown
}

export declare const ERRNO_CODES: Readonly<Record<ErrnoCode, number>>

export declare function fsError(code: ErrnoCode, options?: FsErrorOptions): FsError

export declare function rangeError(name: string, expected: string, value: number): RangeError & {
  code: "ERR_OUT_OF_RANGE"
}

export declare function isFsError(error: unknown, code?: ErrnoCode): error is FsError

export declare function errnoOf(error: unknown): number

export declare function isNormalizedPath(path: string): boolean
export declare function splitPath(path: string): string[]
export declare function normalizePath(path: string): string

export interface ResolvedPath {
  readonly path: string
  readonly segments: string[]
}

export declare function resolvePath(path: string): ResolvedPath
export declare function joinPath(...parts: string[]): string
export declare function dirname(path: string): string
export declare function basename(path: string): string
export declare function isPathInside(path: string, parent: string): boolean

export declare const S_IFMT: 0o170000
export declare const S_IFREG: 0o100000
export declare const S_IFDIR: 0o040000
export declare const S_IFLNK: 0o120000
export declare const S_IFBLK: 0o060000
export declare const S_IFCHR: 0o020000
export declare const S_IFIFO: 0o010000
export declare const S_IFSOCK: 0o140000
export declare const S_ISGID: 0o2000
export declare const S_IXGRP: 0o0010

export declare function fileTypeMode(mode: number): number
export declare function isSpecialMode(mode: number): boolean

/** Native lock/order boundary; callback values and errors remain JavaScript-owned. */
export declare class PathLock {
  constructor()
  read<T>(callback: () => Promise<T>): Promise<T>
  write<T>(callback: () => Promise<T>): Promise<T>
}
