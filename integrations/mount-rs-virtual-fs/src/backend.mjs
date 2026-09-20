/**
 * The operations consumed by the in-process adapters.
 *
 * A NodeRustFs Filesystem satisfies this structural contract. Keeping the
 * boundary structural also leaves room for a future authenticated HTTP
 * transport which forwards the same logical-drive operations; this package
 * does not implement that transport.
 */
export const MOUNT_RS_BACKEND_METHODS = Object.freeze([
  "stat",
  "lstat",
  "readdir",
  "open",
  "readFile",
  "writeFile",
  "mkdir",
  "rmdir",
  "unlink",
  "rename",
  "link",
  "symlink",
  "readlink",
  "chmod",
  "utimes",
]);

export function assertMountRsBackend(backend) {
  if (!backend || typeof backend !== "object") {
    throw new TypeError("an initialized mount-rs filesystem backend is required");
  }
  for (const method of MOUNT_RS_BACKEND_METHODS) {
    if (typeof backend[method] !== "function") {
      throw new TypeError(`mount-rs filesystem backend is missing ${method}()`);
    }
  }
  return backend;
}
