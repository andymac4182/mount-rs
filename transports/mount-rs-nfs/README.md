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
filesystem-backed attributes exposed by `FsDriver`. `NfsServer::clients()`
returns live accepted `NfsConnection` objects in arrival order; each exposes a
stable id, peer, shared v3/v4 sessions, and bounded `close`/`wait_closed`
lifecycle operations. Closing one connection tears down only its TCP serving
task; the server-owned protocol state remains available to other clients.

## Native macOS/Linux mount lifecycle

`nfs_client_probe` reports host prerequisites and `mount_nfs` starts the
in-process NFSv3 or Linux NFSv4.1 server selected by `NfsMountOptions` before
invoking the platform's `mount(8)`. Teardown is idempotent and retryable: it
checks the mount table, tries `umount`, and then uses `umount -f` (plus
Linux-only `umount -l`) within the configured deadline. `unmount_all_nfs`
provides process-local cleanup for application shutdown; the crate does not
install signal handlers, so signal policy remains with the host application.

Native verification is a separate manual host-client test. Linux requires
privilege; macOS can run it rootlessly when the process owns the writable empty
mount point. In both cases the host must have an NFS client:

* Linux needs the distribution NFS client tools (`nfs-common` on Debian-family
  systems or `nfs-utils` on Fedora-family systems), plus root or the equivalent
  `CAP_SYS_ADMIN` mount capability. The native helper reports this as a
  prerequisite; a rootless Linux wire test is not a native mount test.
* macOS uses its built-in `/sbin/mount_nfs` client; Homebrew NFS packages are
  not required. Run the harness as the ordinary user who owns its temporary
  mountpoint (root is also accepted, but is not needed), and grant Full Disk
  Access to the terminal or process if macOS privacy/security policy requires
  it.

The native helper uses an ephemeral loopback TCP port and supplies both
`port=` and `mountport=` for NFSv3, so `rpcbind`/portmap is not needed. Linux
NFSv4.1 uses `vers=4.1,proto=tcp,port=...` and does not add the v3
`mountport=`/lock controls. The export path is `/` by default. For a manual
mount outside the helper, use a reachable fixed bind address/port and the same
explicit TCP options. These prerequisites do not claim that native mounting
has been run or passed here.

The ignored integration harness can be deliberately enabled on a prepared
host; it performs a real kernel mount only when explicitly selected. The v3
case runs on both supported platforms and covers directory rename, `stat`,
`mkdir`, `readdir`, file rename, truncate, and hard-link/readback. The v4.1
case is compiled and run only on Linux and covers the same operations plus
nested directories, offset read/write with `sync_all`, cross-directory rename,
and a 256-entry directory listing:

```text
# macOS or Linux: NFSv3
MOUNT_RS_NFS_NATIVE_TEST=1 ./scripts/cargo-shared test -p mount-rs-nfs --test native_mount -- --ignored --exact native_loopback_mount_round_trip --nocapture

# Linux only: NFSv4.1
MOUNT_RS_NFS_NATIVE_V4_TEST=1 ./scripts/cargo-shared test -p mount-rs-nfs --test native_mount -- --ignored --exact native_loopback_mount_v4_1_round_trip --nocapture
```

On macOS the first command is the native verification command: run it without
`sudo` when the test creates its own temporary directory, or run the same
command as root only when the CI job intentionally provides a root test. The
helper emits the platform-correct v3 options `vers=3,proto=tcp,port=...,`
`mountport=...,nolocks,soft,timeo=...,retrans=...,nobrowse`; it does not use
Linux's `nolock`, and it does not require `rpcbind`. A failure from
`mount_nfs`, a privacy refusal, or teardown is a real native-test failure, not
an implicit pass. This NFS validation does not establish SQLite hosting or
other storage support on macOS.

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
stable handle verifier. `NfsConnection` close/wait state is process-local and
does not provide reconnect, lease recovery, or crash-durable session/reply
state; cross-process crash and durability behavior remains outside the
supported local scope until a separate qualification lane is accepted.
