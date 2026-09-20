# mount-rs-nfs

This crate provides the mountx NFS transport boundary for NFSv3 and MOUNTv3
over ONC RPC/TCP. `Nfs3Session` is byte-oriented and can be tested without a
kernel mount; `NfsServer` adds TCP record marking and filesystem-backed NFS
operations through `mount-rs-core::FsDriver`. `mount_nfs` is the separate
native macOS/Linux lifecycle bridge around that NFSv3 server.

## Rootless wire tests

The crate's wire tests bind `127.0.0.1:0`, use an in-process `MemoryFs`, and
exercise MOUNT, CREATE, WRITE, and READ over TCP. They run as an ordinary user
on macOS and Linux. Passing them proves RPC/XDR/session behavior only; it is
not native kernel-mount verification.

The server does not start `rpcbind`/portmap and does not register a dynamic
port. It serves MOUNTv3 and NFSv3 on the same explicitly selected TCP address.

## Native macOS/Linux mount lifecycle

`nfs_client_probe` reports host prerequisites and `mount_nfs` starts the
in-process NFSv3 server before invoking the platform's `mount(8)`. Teardown is
idempotent and retryable: it checks the mount table, tries `umount`, and then
uses `umount -f` (plus Linux-only `umount -l`) within the configured deadline.
`unmount_all_nfs` provides process-local cleanup for application shutdown; the
crate does not install signal handlers, so signal policy remains with the host
application.

Native verification is a separate, privileged/manual test. The host must have
an NFS client and a writable empty mount point:

* Linux needs the distribution NFS client tools (`nfs-common` on Debian-family
  systems or `nfs-utils` on Fedora-family systems), plus root or the equivalent
  `CAP_SYS_ADMIN` mount capability. The native helper reports this as a
  prerequisite; a rootless Linux wire test is not a native mount test.
* macOS uses its built-in `/sbin/mount_nfs` client. An ordinary user must own
  the mountpoint, and macOS privacy/security policy may require Full Disk
  Access for the terminal or process running the test.

The native helper uses an ephemeral loopback TCP port and supplies both
`port=` and `mountport=` for NFSv3, so `rpcbind`/portmap is not needed. The
export path is `/` by default. For a manual mount outside the helper, use a
reachable fixed bind address/port and the same explicit TCP options. These
prerequisites do not claim that native mounting has been run or passed here.

The ignored integration harness can be deliberately enabled on a prepared
host; it performs a real kernel mount only when explicitly selected:

```text
MOUNT_RS_NFS_NATIVE_TEST=1 cargo test -p mount-rs-nfs --test native_mount -- --ignored --nocapture
```

The normal crate tests remain rootless and do not invoke `mount(8)`.

## Deliberate scope gaps

NFSv4.1 wire service is not implemented. `NfsVersion::V4_1` and its Linux
option spelling are present only so the native/client boundary is explicit;
`mount_nfs` refuses it before opening a listener. The planned v4.1 work is a
separate server dispatcher for RPC program version 4 with COMPOUND,
EXCHANGE_ID/CREATE_SESSION/SEQUENCE, stateids and leases, OPEN/CLOSE,
READ/WRITE/COMMIT, reclaim, delegations, ACLs, and id mapping, backed by
interoperability tests against a Linux kernel client. macOS remains v3-only in
this crate until its v4 minor-version behavior is verified.

UDP transport, portmapper registration, NLM/NSM locking, and persistent
cross-process file-handle recovery remain unimplemented. File handles and
exclusive-create verifiers are process-local unless the caller supplies a
stable handle verifier.
