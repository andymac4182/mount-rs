import { Bash } from "just-bash";

import { MountRsFilesystemBase } from "./common.mjs";

/**
 * An actual just-bash IFileSystem backed by one NodeRustFs Filesystem.
 *
 * The adapter deliberately does not expose a host path. All operations stay
 * in the logical filesystem passed to the constructor.
 */
export class MountRsJustBashFs extends MountRsFilesystemBase {
  async destroy() {
    await this.close();
  }
}

/**
 * Construct a just-bash filesystem and prime its synchronous glob path index.
 * The index contains paths, not file contents; call refreshPaths() after
 * out-of-band mutations when shell glob expansion must see them.
 */
export async function createJustBashFilesystem(filesystem, options = {}) {
  const adapter = new MountRsJustBashFs(filesystem, options);
  if (options.refreshPaths !== false) await adapter.refreshPaths();
  return adapter;
}

export const createJustBashFs = createJustBashFilesystem;

/**
 * Construct the real just-bash Bash runtime over a NodeRustFs-backed IFileSystem.
 */
export async function createBash(filesystem, options = {}) {
  const adapter = await createJustBashFilesystem(filesystem, options);
  const { refreshPaths: _refreshPaths, closeOnDestroy: _closeOnDestroy, readOnly: _readOnly, ...bashOptions } = options;
  return new Bash({ ...bashOptions, fs: adapter });
}

