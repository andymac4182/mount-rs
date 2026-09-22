import type { FsDriver, FileHandleLike } from "./driver.js"
import type { Filesystem, FileHandle } from "../index.js"

/** Every declaration decided; method presence is only the fallback. */
export type ResolvedCapabilities = Required<Omit<NonNullable<FsDriver["capabilities"]>, "mknod" | "extensions">> & {
  extensions: readonly (keyof NonNullable<FsDriver["mountx"]>)[]
}

export interface Loopback<D extends FsDriver | Filesystem = FsDriver> extends Required<Omit<FsDriver, "capabilities" | "mountx" | "open">> {
  readonly driver: D
  readonly capabilities: ResolvedCapabilities
  readonly mountx: FsDriver["mountx"]
  /** Native handles retain their native fd shape; structural handles are unchanged. */
  open(path: string, flags?: string | number, mode?: number): Promise<D extends Filesystem ? FileHandle : FileHandleLike>
  readFile(path: string): Promise<Uint8Array>
  writeFile(path: string, data: string | Uint8Array): Promise<void>
}

export declare function createLoopback<D extends FsDriver | Filesystem>(driver: D): Loopback<D>
export declare function resolveCapabilities(driver: FsDriver | Filesystem): ResolvedCapabilities
