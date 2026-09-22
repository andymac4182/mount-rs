/// <reference types="node" />

import type {
  CopyOptions,
  FileContent,
  FileEntry,
  FileStat,
  FilesystemInfo,
  ListOptions,
  ReadOptions,
  RemoveOptions,
  WorkspaceFilesystem,
  WriteOptions,
  ProviderStatus,
} from "@mastra/core/workspace";

import type { MountRsBackend, MountRsStats } from "./backend.js";

export interface MastraAdapterOptions {
  id?: string;
  name?: string;
  displayName?: string;
  description?: string;
  /** Reject writes while preserving reads. Defaults to the backend policy. */
  readOnly?: boolean;
  /** Call backend.shutdown() when destroy completes. */
  closeOnDestroy?: boolean;
}

export declare class MountRsMastraFilesystem implements WorkspaceFilesystem {
  readonly id: string;
  readonly name: string;
  readonly provider: string;
  readonly readOnly: boolean;
  readonly displayName?: string;
  readonly description?: string;
  status: ProviderStatus;
  error?: string;

  init(): Promise<void>;
  destroy(): Promise<void>;
  isReady(): boolean;
  getInfo(): FilesystemInfo;
  getInstructions(): string;

  readFile(path: string, options?: ReadOptions): Promise<string | Buffer>;
  writeFile(path: string, content: FileContent, options?: WriteOptions): Promise<void>;
  appendFile(path: string, content: FileContent): Promise<void>;
  deleteFile(path: string, options?: RemoveOptions): Promise<void>;
  copyFile(source: string, destination: string, options?: CopyOptions): Promise<void>;
  moveFile(source: string, destination: string, options?: CopyOptions): Promise<void>;
  mkdir(path: string, options?: { recursive?: boolean }): Promise<void>;
  rmdir(path: string, options?: RemoveOptions): Promise<void>;
  readdir(path: string, options?: ListOptions): Promise<FileEntry[]>;
  exists(path: string): Promise<boolean>;
  stat(path: string): Promise<FileStat>;
  realpath(path: string): Promise<string>;

  /** Mount-rs extensions used to preserve symlink metadata. */
  lstat(path: string): Promise<MountRsStats>;
  symlink(target: string, path: string): Promise<void>;
  readlink(path: string): Promise<string>;
}

export declare function createMastraFilesystem(
  filesystem: MountRsBackend,
  options?: MastraAdapterOptions,
): MountRsMastraFilesystem;
