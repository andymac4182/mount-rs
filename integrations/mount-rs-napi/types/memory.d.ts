import type { Filesystem } from "../index.js"

/** Options for the Rust-backed in-memory filesystem. */
export interface MemoryDriverOptions {
  /** Owner of every node created by the driver; defaults to the current process. */
  uid?: number
  /** Group owner of every node created by the driver; defaults to the current process. */
  gid?: number
  /** Permission bits cleared from newly created files, directories, and nodes. */
  umask?: number
  /** Permission bits of the initially empty root directory. Defaults to `0o755`. */
  rootMode?: number
}

/** The N-API filesystem returned by `createMemoryDriver`. */
export type MemoryDriver = Filesystem

export declare function createMemoryDriver(
  options?: MemoryDriverOptions | null,
): MemoryDriver
