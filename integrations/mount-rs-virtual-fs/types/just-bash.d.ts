import type {
  Bash,
  BashOptions,
  BufferEncoding,
  ByteString,
  CpOptions,
  FileContent,
  FsStat,
  IFileSystem,
  MkdirOptions,
  RmOptions,
} from "just-bash";

import type { MountRsBackend } from "./backend.js";

type ReadFileOptions = Parameters<IFileSystem["readFile"]>[1];
type WriteFileOptions = Parameters<IFileSystem["writeFile"]>[2];
type DirentEntry = NonNullable<
  IFileSystem["readdirWithFileTypes"]
> extends (path: string) => Promise<infer Entries>
  ? Entries[number]
  : never;

export interface JustBashAdapterOptions {
  /** Reject writes while preserving reads. Defaults to the backend policy. */
  readOnly?: boolean;
  /** Call backend.shutdown() when close/destroy completes. */
  closeOnDestroy?: boolean;
  /** Prime the synchronous glob index during construction. Defaults to true. */
  refreshPaths?: boolean;
}

export type CreateBashOptions = Omit<BashOptions, "fs"> & JustBashAdapterOptions;

export declare class MountRsJustBashFs implements IFileSystem {
  readonly filesystem: MountRsBackend;
  readonly readOnly: boolean;
  readonly closeOnDestroy: boolean;
  readonly closed: boolean;

  refreshPaths(): Promise<string[]>;
  getAllPaths(): string[];
  resolvePath(base: string, value: string): string;
  close(): Promise<void>;
  destroy(): Promise<void>;

  readFile(path: string, options?: ReadFileOptions | BufferEncoding): Promise<string>;
  readFileBytes(path: string): Promise<ByteString>;
  readFileBuffer(path: string): Promise<Uint8Array>;
  writeFile(path: string, content: FileContent, options?: WriteFileOptions | BufferEncoding): Promise<void>;
  appendFile(path: string, content: FileContent, options?: WriteFileOptions | BufferEncoding): Promise<void>;
  exists(path: string): Promise<boolean>;
  stat(path: string): Promise<FsStat>;
  lstat(path: string): Promise<FsStat>;
  mkdir(path: string, options?: MkdirOptions): Promise<void>;
  readdir(path: string): Promise<string[]>;
  readdirWithFileTypes(path: string): Promise<DirentEntry[]>;
  rm(path: string, options?: RmOptions): Promise<void>;
  rmdir(path: string, options?: RmOptions): Promise<void>;
  cp(path: string, destination: string, options?: CpOptions): Promise<void>;
  mv(path: string, destination: string): Promise<void>;
  chmod(path: string, mode: number): Promise<void>;
  symlink(target: string, path: string): Promise<void>;
  link(existingPath: string, newPath: string): Promise<void>;
  readlink(path: string): Promise<string>;
  realpath(path: string): Promise<string>;
  utimes(path: string, atime: Date, mtime: Date): Promise<void>;
}

export declare function createJustBashFilesystem(
  filesystem: MountRsBackend,
  options?: JustBashAdapterOptions,
): Promise<MountRsJustBashFs>;

export declare const createJustBashFs: typeof createJustBashFilesystem;

export declare function createBash(
  filesystem: MountRsBackend,
  options?: CreateBashOptions,
): Promise<Bash>;
