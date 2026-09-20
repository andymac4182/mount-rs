# mount-rs-nfs

This crate provides the mountx NFS transport boundary for NFSv3, MOUNTv3, and
the implemented NFSv4.1 COMPOUND/session service over ONC RPC/TCP.
`Nfs3Session` and `Nfs4Session` are byte-oriented and can be tested without a
kernel mount; `NfsServer` adds TCP record marking and filesystem-backed NFS
operations through `mount-rs-core::FsDriver`. `mount_nfs` is the separate
native macOS/Linux lifecycle bridge around that server.

## Rootless wire tests

The crate's wire tests bind `127.0.0.1:0`, use an in-process `MemoryFs`, and
exercise MOUNT, CREATE, WRITE, and READ over TCP. They run as an ordinary user
on macOS and Linux. Passing them proves RPC/XDR/session behavior only; it is
not native kernel-mount verification.

The server does not start `rpcbind`/portmap and does not register a dynamic
port. It serves MOUNTv3/NFSv3 and NFSv4.1 on the same explicitly selected TCP
address. The NFSv4.1 service uses AUTH_NONE/AUTH_SYS, session slots, replay
cache, stateids, common namespace operations, OPEN/CLOSE, READ/WRITE, and the
filesystem-backed attributes exposed by `FsDriver`.

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
`port=` and `mountport=` for NFSv3, so `rpcbind`/portmap is not needed. Linux
NFSv4.1 uses `vers=4.1,proto=tcp,port=...` and does not add the v3
`mountport=`/lock controls. The export path is `/` by default. For a manual
mount outside the helper, use a reachable fixed bind address/port and the same
explicit TCP options. These prerequisites do not claim that native mounting
has been run or passed here.

The ignored integration harness can be deliberately enabled on a prepared
host; it performs a real kernel mount only when explicitly selected:

```text
MOUNT_RS_NFS_NATIVE_TEST=1 cargo test -p mount-rs-nfs --test native_mount -- --ignored --nocapture
```

The normal crate tests remain rootless and do not invoke `mount(8)`.

## Deliberate scope gaps

NFSv4.1 is not full upstream parity yet. Byte-range LOCK/LOCKT/LOCKU now have
real process-local state, conflict/denial replies, range release, and
stateid lifecycle handling, but they are advisory to this in-process service:
there is no blocking wait, backend/kernel lock integration, or persistent
lease/lock recovery. Delegations and callbacks, ACL/id-mapping policy,
pNFS/layout/offload operations, persistent lease/reply state across process
restart, and the other RFC operations outside the common filesystem/session
path remain gaps. It does not claim Linux-kernel native mount interoperability
from the rootless wire test. macOS remains v3-only here until its v4
minor-version behavior is verified, and native Linux CI is not wired by this
crate.

UDP transport, portmapper registration, NLM/NSM locking, and persistent
cross-process file-handle recovery remain unimplemented. File handles and
exclusive-create verifiers are process-local unless the caller supplies a
stable handle verifier.
