import type { JsStats, JsStatsFs, JsCapabilities } from "../index.js"

export interface FileHandleLike {
  readonly fd?: number
  read(buffer: Uint8Array, offset?: number | null, length?: number | null, position?: number | null): Promise<{ bytesRead: number; buffer: Uint8Array }>
  write(buffer: Uint8Array, offset?: number | null, length?: number | null, position?: number | null): Promise<{ bytesWritten: number; buffer: Uint8Array }>
  stat(): Promise<JsStats>
  truncate(length?: number): Promise<void>
  close(): Promise<void>
  sync?(): Promise<void>
  datasync?(): Promise<void>
}

export interface DirentLike {
  name: string
  parentPath?: string
  isFile(): boolean
  isDirectory(): boolean
  isSymbolicLink(): boolean
  isBlockDevice(): boolean
  isCharacterDevice(): boolean
  isFIFO(): boolean
  isSocket(): boolean
}

/** Structural driver accepted by public mount and server factories. */
export interface FsDriver {
  readonly capabilities?: Partial<Omit<JsCapabilities, "extensions">> & { extensions?: readonly string[] }
  readonly mountx?: {
    utimens?(path: string, atimeNs: bigint, mtimeNs: bigint, options?: { followSymlinks?: boolean }): Promise<void>
    mknod?(path: string, mode: number, dev: number): Promise<void>
  }
  syncfs?(): Promise<void>
  stat(path: string): Promise<JsStats>
  readdir(path: string, options: { withFileTypes: true }): Promise<DirentLike[]>
  open(path: string, flags?: string | number, mode?: number): Promise<FileHandleLike>
  lstat?(path: string): Promise<JsStats>
  statfs?(path: string): Promise<Pick<JsStatsFs, "type" | "bsize" | "blocks" | "bfree" | "bavail" | "files" | "ffree">>
  mkdir?(path: string, options?: { recursive?: boolean; mode?: number }): Promise<string | undefined>
  rmdir?(path: string): Promise<void>
  unlink?(path: string): Promise<void>
  rename?(oldPath: string, newPath: string): Promise<void>
  link?(existingPath: string, newPath: string): Promise<void>
  symlink?(target: string, path: string, type?: string | null): Promise<void>
  readlink?(path: string): Promise<string>
  chmod?(path: string, mode: number): Promise<void>
  chown?(path: string, uid: number, gid: number): Promise<void>
  lchown?(path: string, uid: number, gid: number): Promise<void>
  truncate?(path: string, length?: number): Promise<void>
  utimes?(path: string, atime: number | Date, mtime: number | Date): Promise<void>
  lutimes?(path: string, atime: number | Date, mtime: number | Date): Promise<void>
}
