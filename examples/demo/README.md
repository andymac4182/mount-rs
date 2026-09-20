# Native Rust + Node.js mount demo

Run this from the repository root:

```sh
./scripts/demo-end-to-end.sh
```

The script creates one private temporary directory containing an owned empty
mountpoint, a durable host-backed directory, and a mode-0600 JSON config. It
builds the real `mount-rs` CLI, starts it as the supervised mount owner, and
waits for both the CLI's `mounted <transport>` line and the host mount table to
show the exact mountpoint. A separate Rust process then writes and reads one
file, a separate Node.js process uses `fs/promises` to write and read another,
and the Rust process finally verifies the JavaScript bytes while the CLI mount
is still live. The script sends SIGINT to the CLI, waits for its normal
unmount report, verifies that the exact path is no longer mounted, removes the
empty mountpoint, and verifies the JavaScript bytes in the durable backing
directory before deleting the private temporary directory.

All lifecycle and process waits are bounded: 60 seconds for native mount
readiness, 20 seconds for the CLI stop/unmount lifecycle, 15 seconds for each
Rust/Node filesystem client and exact-path fallback unmount command, plus a
one-second escalation window before a supervised child is force-stopped. If
cleanup cannot prove that the mount is gone, the script preserves the
temporary directory and reports the exact path instead of claiming success.
The demo uses only the local host driver; it does not read, print, or require
cloud/provider credentials.

## Prerequisites and transport policy

All hosts need the repository's pinned Rust toolchain/Cargo lockfile, Node.js
with `fs/promises`, and a writable temporary directory owned by the current
user. The demo is intentionally limited to macOS and Linux.

On macOS, the script requires the native `/sbin/mount_nfs` client and the
process's normal permission to mount and unmount an owned temporary directory.
Terminal or process Privacy/Full Disk Access policy can still refuse a native
network-volume mount; grant the required permission and retry. macFUSE is not
used by this demo.

On Linux, the script prefers native NFS when the CLI probe reports it usable.
That requires a `mount.nfs` helper, kernel NFS support, and root or equivalent
mount capability. If NFS is unavailable, it selects the repository's native
FUSE transport when the probe reports FUSE usable; that requires `/dev/fuse`,
`fusermount3` or `fusermount` on `PATH`, and the host's FUSE permission. The
Linux 9P transport is not silently substituted. If neither NFS nor FUSE is
usable, the script hard-fails with the full `mount-rs probe` reasons.

This is a real native-mount demo, not a wire-only or simulated test. A probe
result is only a prerequisite explanation; success requires an actual kernel
mount, cross-process filesystem I/O, final byte verification, normal CLI
unmount, and cleanup evidence.
