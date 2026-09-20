import type { Filesystem, JsCapabilities } from "@andymac4182/mount-rs";

/**
 * The public async filesystem surface consumed by both adapters.
 *
 * `Filesystem` from @andymac4182/mount-rs satisfies this type. A future
 * remote logical-drive client may implement the same method surface; the
 * virtual-fs package intentionally does not provide an HTTP transport.
 */
export type MountRsBackend = Pick<
  Filesystem,
  | "stat"
  | "lstat"
  | "readdir"
  | "open"
  | "readFile"
  | "writeFile"
  | "mkdir"
  | "rmdir"
  | "unlink"
  | "rename"
  | "link"
  | "symlink"
  | "readlink"
  | "chmod"
  | "utimes"
> & {
  readonly capabilities?: Pick<JsCapabilities, "readOnly">;
  readonly shutdown?: () => Promise<void>;
};

/** Alias documenting the currently supported concrete provider. */
export type NodeRustFsBackend = Filesystem;

/** Metadata returned by the public NodeRustFs stat/lstat methods. */
export type MountRsStats = Awaited<ReturnType<Filesystem["stat"]>>;
