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
task; the server-owned protocol state remains available to other clients. The
rootless wire suite also proves that a v4.1 session can be used again after an
orderly TCP transport reconnect while this server process remains alive, and
that multiple v3 calls can be pipelined on one connection within the configured
in-flight bound. Two independent v4.1 sessions also complete concurrent
distinct-file OPEN/WRITE/READ round trips. This is rootless in-process
userspace concurrency evidence. A restart-boundary test reuses the backend
with a replacement server and confirms that the old v4 session is rejected
with `NFS4ERR_BADSESSION`.

The rootless process-restart gate also starts a real child server over a
`HostFs` root, writes a `FILE_SYNC` NFSv3 payload, force-terminates that child,
and recovers the file through MOUNT/LOOKUP/READ from a replacement server.
The replacement also rejects the pre-crash file handle with `NFS3ERR_STALE`,
making the boundary explicit: backend data is recoverable, while handles remain
process-local. A companion NFSv4.1 child-process case establishes a session,
force-terminates the child, and verifies that a replacement rejects the old
session with `NFS4ERR_BADSESSION`. These are process-crash classification
checks; they do not claim power-loss durability or persistent NFSv4 lease,
replay, or file-handle state.

The shared file-handle table accepts `max_handles` through
`NfsSessionOptions`/`NfsServerOptions` (and `maxHandles` through the N-API
server options). A positive value is a soft LRU cap: the root and the entry
currently being returned are protected, and live NFSv4.1 open state pins its
handle entry so the cap cannot silently break share reservations or locks.
When every candidate is pinned the table may exceed the cap until state is
released; with no value, the table remains uncapped for compatibility.

NFSv4.1 channel and state ceilings are available through
`NfsSessionOptions.nfs4` and the nested N-API `nfs4`/`Nfs4StateKnobs` option:
`idmap`, `leaseSeconds`, `maxSessions`, `maxForeSlots`, `maxOperations`,
`maxRequestSize`, `maxCachedResponseSize`, `maxOpensPerFile`, `seed`,
`maxLocksPerFile`, and `requireReclaimComplete`. The channel-size and count
values are clamped during `CREATE_SESSION`; operation, session, open-state,
and lock-range limits are enforced by the v4 state machine. When enabled,
`requireReclaimComplete` gates both `OPEN` and `LOCK` state establishment
until the client sends `RECLAIM_COMPLETE`. The defaults match the pinned
oracle's 90-second lease, 64 fore slots/operations, 1 MiB request ceiling,
64 KiB replay cache, 256 opens per file, and 1024 lock ranges per file.
Lease expiry is enforced at NFSv4 request boundaries: an expired client loses
its sessions, locks, open states, and pinned backend handles before the next
COMPOUND is dispatched. Rust callers can also invoke
`Nfs4Session::sweep_expired`; `Nfs4Clock` provides deterministic monotonic
time injection for Rust tests. N-API callers can additionally provide the
synchronous `nfs4.now` JavaScript callback; its millisecond results are
anchored to a monotonic Rust `Instant`, and invalid/throwing/Promise results
retain the last observed instant. `idmap` supports a deterministic static map:
Rust callers use `Nfs4IdMap`'s `with_user`/`with_group` builders, while N-API
callers provide `domain`, `users`, and `groups` name-to-id records. Mapped
names are qualified with the configured domain, unmapped ids retain numeric
form, and incoming names from a different domain return `NFS4ERR_BADOWNER`.
Both Rust and N-API callers may also provide synchronous `nameOf`/`idOf`
callbacks; they take precedence over the static entries for that request.

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
lease/lock recovery. Delegations and callback-channel operations, ACL policy,
pNFS/layout/offload operations, persistent lease/reply state across process
restart, and the other RFC operations outside the common filesystem/session
path remain gaps. It does not claim Linux-kernel native mount interoperability
from the rootless wire test. macOS remains v3-only here until its v4
minor-version behavior is verified, and native Linux CI is not wired by this
crate.

UDP transport, portmapper registration, NLM/NSM locking, and persistent
cross-process file-handle recovery remain unimplemented. The two-client v4.1
round-trip qualifies concurrent clients and sessions served by one
`NfsServer` process, but does not qualify native-client ordering. Running
multiple server processes against one backend is outside the supported scope:
there is no cross-process session, lease, replay, or handle arbitration. File
handles and
exclusive-create verifiers are process-local unless the caller supplies a
stable handle verifier. A caller can reconnect to a still-running server and
reuse the tested session, but `NfsConnection` close/wait state does not provide
automatic reconnect, lease recovery, or crash-durable session/reply state;
the restart-boundary test therefore classifies v4 session/lease/replay state as
process-local. Backend crash recovery and durability behavior remains outside
the supported local scope until a separate qualification lane is accepted.

The N-API boundary now also supports the upstream synchronous NFSv4
`idmap.nameOf`, `idmap.idOf`, and `now` callbacks. Callback results must be
immediate strings, uint32-compatible numbers, or millisecond numbers;
Promises and other invalid results are treated as unavailable (numeric owner
fallback, `NFS4ERR_BADOWNER`, or the last clock instant). The callback
references are retained by the server and released on close. Rust callback
panics are isolated. Dedicated Rust callback tests and the live N-API server
exercise `EXCHANGE_ID`/`CREATE_SESSION`, owner `GETATTR`, owner `SETATTR`, and
clock calls; release build, generated typecheck, and the pinned NFS codec
differential remain separate evidence gates.

Rust callers can install `NfsSessionHooks`; the N-API
`NfsServerOptions.onError` callback receives an `Error` plus the decoded
`NfsRpcCall` for XDR/dispatch failures, or `undefined` for ordinary NFS status
failures. Callback-channel operations, native Linux NFSv4.1, hosted lifecycle,
and crash/durability qualification remain explicit production gates.
