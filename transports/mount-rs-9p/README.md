# mount-rs-9p

This crate implements the mountx-compatible 9P2000.L wire codec, per-connection
fid/session state, byte-range locks, and a rootless Tokio TCP server for the
`mount-rs-core` driver contract.

## Verification

From the repository root:

```text
cargo test -p mount-rs-9p
cargo clippy -p mount-rs-9p --all-targets -- -D warnings
```

These tests exercise complete protocol frames, session operations, and a
loopback TCP connection without requiring a kernel 9P client, a mount point, or
elevated privileges. They are therefore portable rootless tests on macOS and
Linux; they do not prove native mount support. The Rust crate also has an
all-target compile guard for `x86_64-pc-windows-gnu`, but there is no Windows
runtime, N-API, or native-mount acceptance gate in this project, so that target
is not a production-supported platform claim.

## Native mount prerequisites

Native verification is Linux-specific. The Linux host must provide the kernel
9P client and TCP transport (normally the `9p`, `9pnet`, and `9pnet_tcp`
modules), and the process mounting the filesystem needs `CAP_SYS_ADMIN`
(normally root). After starting `P9Server`, a typical check is equivalent to:

```text
sudo mount -t 9p -o trans=tcp,version=9p2000.L,port=<PORT> 127.0.0.1 /mnt/mount-rs-9p
```

The exact module packaging and mount policy are distribution- and kernel-
specific. macOS has no built-in 9P client, so this crate's macOS verification
is the rootless wire/TCP suite; an external userspace client would be required
for an actual macOS mount. Windows has no qualified native or hosted client
path in this project; its compile guard is portability evidence only. The crate
does not provide a native mount wrapper for non-Linux platforms.

## Windows platform boundary

The portable Rust surface is compile-checked with
`--target x86_64-pc-windows-gnu --all-targets`. That check does not qualify
Windows runtime behavior, the N-API addon, Unix-domain listeners, or a native
9P mount. Until a hosted Windows runtime gate is added, production support is
limited to Linux native mounts and the tested macOS/Linux rootless wire/TCP
surface.

## Lifecycle and crash boundary

`P9Mount::wait_closed()` waits for the kernel connection and releases the
server resources; it does not implicitly run `umount(8)` on a still-live
mount. Call `P9Mount::unmount()` after a server-close or EOF path when the
kernel mount must also be detached. The transport provides bounded graceful
close, external-unmount observation, and retryable unmount semantics. It does
not promise automatic recovery from a process crash or transparent recovery
of arbitrary kernel reset/half-close behavior; deployments that require those
properties must provide a supervisor and an explicit cleanup/restart policy.

## Deliberate protocol boundaries

The implemented `.L` operations include version/attach/walk, `Tlopen`,
`Tlcreate`, read/write, clunk/remove, getattr/statfs/readdir, fsync/setattr,
mkdir/symlink/mknod/link/readlink, rename/renameat/unlinkat, and lock/getlock.
Authentication (`Tauth`), extended attributes (`Txattrwalk` and
`Txattrcreate`), the legacy `Topen`/`Tcreate`/`Tstat`/`Twstat` family, legacy
error messages, and unknown message types return `ENOTSUP`. `Tstatfs` returns
`ENOSYS` when the driver does not advertise statfs. A driver without mknod
support gets the mountx regular-file fallback; special-node creation remains
`ENOSYS` unless the driver implements it.

Direct dependencies are intentionally limited to `mount-rs-core` and Tokio.
The core crate supplies the filesystem contract; Tokio's `io-util`, `net`,
runtime, synchronization, macros, and timing features support the asynchronous
TCP/session implementation and rootless tests. No backend or platform-specific
filesystem library is pulled into this transport.
