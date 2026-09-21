import type { ReactNode } from 'react'
import { Link } from '@tanstack/react-router'
import { CodeBlock } from './code-block'
import { MaturityBadge, type ProviderMaturity } from './provider-doc'

export type TransportSpec = {
  slug: string
  name: string
  eyebrow: string
  maturity: ProviderMaturity
  maturityNote: string
  summary: ReactNode
  platform: ReactNode
  access: ReactNode
  surface: ReactNode
  verifyLabel: string
  verifyCode: string
  safeUse: ReactNode
  limitations: ReactNode
  evidence: ReactNode
  sources: Array<{ label: string; href: string }>
}

export const transportSpecs = {
  fuse: {
    slug: 'fuse',
    name: 'FUSE',
    eyebrow: 'Transport / kernel-facing Unix mount',
    maturity: 'Preview',
    maturityNote: 'Mount-free FUSE session and codec evidence plus a revision-specific hosted Linux mount checkpoint exist; callback, native lifecycle, FSKit, cancellation, crash/restart, concurrency, and durability gates remain external.',
    summary: (
      <>
        FUSE is the kernel-facing route for a host that can provide the FUSE
        device and mount capability. The repository also keeps a mount-free
        loopback/codec path so applications can use the same contract without
        a kernel mount.
      </>
    ),
    platform: (
      <>
        Linux needs <code>/dev/fuse</code>, a FUSE userspace helper where the
        selected lifecycle uses one, and the required mount capability. The
        CLI probes these prerequisites. The repository's FUSE path is not the
        same thing as macFUSE, and macOS FSKit is documented separately.
      </>
    ),
    access: (
      <>
        Mount-free tests exercise codecs, loopback drivers, and userspace
        sessions. A native mount is a separate privileged/platform test; do
        not infer it from a decoded FUSE frame or a successful Rust driver
        operation.
      </>
    ),
    surface: (
      <>
        The codec represents a broad protocol-7.41 wire surface, while the
        current native session dispatch intentionally supports a smaller set.
        Unsupported native operations return explicit errors rather than being
        silently claimed as complete parity.
      </>
    ),
    verifyLabel: 'Check the mount-free and native boundaries separately',
    verifyCode: `cargo test -p mount-rs-fuse --locked
cargo run --locked -p mount-rs-cli -- probe

# Only on a prepared Linux host, when explicitly enabled:
MOUNT_RS_CLI_NATIVE_FUSE=1 \
  cargo test -p mount-rs-cli --test native_lifecycle \
  -- --ignored --nocapture`,
    safeUse: (
      <>
        Keep mountpoints empty and owned by the test process, unmount on every
        exit path, and treat elevated mount capability as a host prerequisite,
        not as a reason to broaden the test's filesystem scope.
      </>
    ),
    limitations: (
      <>
        The codec is not a complete native session. The public Node barrel now
        covers typed <code>READ</code>/<code>WRITE</code>,
        <code>GETATTR</code>/<code>SETATTR</code>,
        <code>OPEN</code>/<code>OPENDIR</code>, <code>CREATE</code>,
        <code>LOOKUP</code>, <code>READLINK</code>, <code>STATFS</code>,
        <code>SYMLINK</code>, <code>MKNOD</code>, <code>MKDIR</code>,
        <code>UNLINK</code>, <code>RMDIR</code>, <code>RENAME</code>,
        <code>LINK</code>, <code>ACCESS</code>,
        <code>BATCH_FORGET</code>, <code>INTERRUPT</code>, <code>POLL</code>,
        <code>FALLOCATE</code>, <code>RENAME2</code>, <code>LSEEK</code>,
        <code>GETLK</code>/<code>SETLK</code>/<code>SETLKW</code>, and
        <code>COPY_FILE_RANGE</code>, <code>RELEASE</code>/<code>RELEASEDIR</code>,
        <code>FLUSH</code>, and <code>FSYNC</code>/<code>FSYNCDIR</code> bodies,
        plus <code>SETXATTR</code>/<code>GETXATTR</code>/<code>LISTXATTR</code>/
        <code>REMOVEXATTR</code> request/reply codecs and
        <code>READDIR</code>/<code>READDIRPLUS</code> directory codecs. These
        are focused mount-free boundaries: malformed or trailing advanced
        requests fail closed and valid unsupported operations still return
        <code>ENOSYS</code>. Full request/reply, init negotiation, session, and
        native-mount surfaces remain open. The current-tree Rust session packet
        adds 16 frame-level cases for <code>SYMLINK</code>, <code>MKNOD</code>,
        <code>MKDIR</code>, <code>UNLINK</code>, <code>RMDIR</code>,
        <code>RENAME</code>, <code>LINK</code>, and <code>ACCESS</code>, including
        error/state cleanup, regular-file <code>MKNOD</code> fallback, and the
        POSIX 255-byte name limit. Advanced <code>FALLOCATE</code>/<code>LSEEK</code>
        semantics and the native device/mount remain open. Plain-flag
        <code>RENAME2</code> now participates in the Rust session path;
        unsupported flag bits return <code>ENOSYS</code> without mutating the
        namespace, while <code>COPY_FILE_RANGE</code> remains unsupported.
        FUSE evidence does not qualify NFS, 9P, or FSKit.
      </>
    ),
    evidence: (
      <>
        The Rust-backed <code>./fuse</code> barrel now exposes oracle-shaped
        typed <code>GETATTR</code>/<code>SETATTR</code>,
        <code>READ</code>/<code>WRITE</code>,
        <code>OPEN</code>/<code>OPENDIR</code>, <code>CREATE</code>,
        <code>LOOKUP</code>, <code>READLINK</code>, and <code>STATFS</code>
        codecs alongside <code>BATCH_FORGET</code>/<code>INTERRUPT</code>,
        <code>POLL</code>, <code>FALLOCATE</code>, <code>RENAME2</code>,
        <code>LSEEK</code>, and <code>COPY_FILE_RANGE</code>. The advanced
        operation packet validates framing and keeps unsupported valid requests
        at <code>ENOSYS</code> without mutating the session. The N-API xattr
        packet adds <code>SETXATTR</code>, <code>GETXATTR</code>,
        <code>LISTXATTR</code>, and <code>REMOVEXATTR</code> codecs with
        pinned-oracle coverage across protocol contexts, malformed,
        truncated, trailing, and declared-size checks. The existing
        raw-layout <code>IOCTL</code> request/reply and typed
        <code>BMAP</code> request/reply bodies are now covered by pinned-oracle
        differentials as well; these remain focused codecs rather than a full
        native session. The latest packet also adds typed
        <code>GETLK</code>/<code>SETLK</code>/<code>SETLKW</code> request codecs
        and the typed <code>GETLK</code> reply. Its pinned-oracle differential
        covers the 48-byte request, 24-byte reply, truncation, trailing-byte,
        and empty status-reply boundaries; generated bindings, declarations,
        typecheck, release build, and the full N-API suite passed. Native FUSE
        lock/session semantics remain open. The latest packet also adds typed
        <code>SYMLINK</code>, <code>MKNOD</code>, <code>MKDIR</code>,
        <code>UNLINK</code>, <code>RMDIR</code>, <code>RENAME</code>,
        <code>RENAME2</code>, <code>LINK</code>, <code>ACCESS</code>,
        <code>FALLOCATE</code>, and <code>LSEEK</code> bodies with generated
        declarations and explicit CommonJS/ESM exports. Pinned byte/decode
        differentials and artifact aggregation passed; this remains mount-free
        codec evidence. Other existing
        <code>RELEASE</code>/<code>RELEASEDIR</code>, <code>FLUSH</code>,
        <code>FSYNC</code>/<code>FSYNCDIR</code>,
        <code>packDirents</code>/<code>unpackDirents</code>, and
        <code>packDirentsPlus</code>/<code>unpackDirentsPlus</code>. Pinned
        7.8/7.39/7.41 differential tests cover protocol bytes, legacy layouts,
        truncation, trailing data, UTF-8 names, 8-byte alignment, bounded
        packing, integer coercion, malformed input, embedded-NUL rejection, and
        inode parity. Current-tree Rust session coverage adds 16 frame-level
        cases for the simple namespace operations above, including rollback and
        inode-path cleanup, error/state preservation, regular-file
        <code>MKNOD</code> fallback, symlink access checks, and the
        <code>NAME_MAX</code> boundary. Existing session tests cover focused
        <code>ACCESS</code>, <code>BATCH_FORGET</code>, and fail-closed
        <code>INTERRUPT</code> validation; six INIT tests cover negotiated
        <code>FUSE_INIT_EXT</code>
        and <code>flags2</code> handling. A revision-specific hosted Linux FUSE
        job passed on <code>35ffbfa</code>, but the current W01-FUSE tracker
        keeps hosted <code>/dev/fuse</code>, callback-event, FSKit,
        cancellation/concurrency, crash/restart, and durability qualification
        as external gates. The current packet also adds fail-closed native
        mount-option validation plus <code>FuseMountHooks</code> and
        <code>FuseTransportError</code> exactly-once terminal reporting; this is
        current-tree and mount-free evidence, not a fresh hosted native pass.
        The mount-free lock session now covers <code>GETLK</code>,
        <code>SETLK</code>, same-owner replacement/unlock, and
        <code>RELEASE</code>/<code>DESTROY</code> cleanup in 19 focused cases;
        serialized <code>SETLKW</code> returns explicit <code>EAGAIN</code>, so
        native POSIX locking is not advertised yet. The automatic mount bridge
        now carries one owned <code>onTransportError</code> callback through the
        FUSE, 9P, and NFS paths, and the native request loop isolates backend
        panic and stop-aware cancellation into bounded session cleanup. Local
        macOS and Linux-target checks pass; hosted Linux callback, fault,
        crash/restart, and concurrent native execution remain external.
        The mount-free N-API session keeps INIT defaults conservative rather
        than advertising asynchronous or parallel capabilities that the
        serialized path cannot provide. Native FUSE now has up to 16
        positional read workers with a serialized reply writer and targeted
        interrupt handling; the eight-client native harness compiles and
        remains awaiting hosted <code>/dev/fuse</code> execution.
        The current W01 packet also adds public <code>FuseSession</code>
        options, negotiated state, inode views, request/reply/error counters,
        assertion and transport-error callbacks, notification encoders, and
        destroy-state readback. On the actual Darwin 27 arm64 host,
        <code>/dev/fuse</code> is absent and the non-Linux mount path returns
        <code>UnsupportedPlatform</code> without touching the requested path;
        the supported native macOS path is NFS, with no macFUSE or FSKit FUSE
        parity claim. Hosted Linux callback, concurrency, close-race,
        crash/restart, and durability execution remain open.
        Plain-flag <code>RENAME2</code> is now supported at session dispatch;
        unsupported flags remain explicit <code>ENOSYS</code> with no mutation.
        The no-reply <code>FORGET</code> path follows the pinned session
        behavior: a malformed body is ignored without releasing the inode,
        while valid <code>FORGET</code> releases its lookup count. Malformed
        <code>BATCH_FORGET</code> frames remain validated before any inode
        release. This is a session-boundary behavior, not native mount proof.
        <code>FALLOCATE</code>, <code>LSEEK</code>, and
        <code>COPY_FILE_RANGE</code> remain unsupported boundaries.
      </>
    ),
    sources: [
      { label: 'FUSE transport boundary', href: 'https://github.com/andymac4182/mount-rs/blob/main/transports/mount-rs-fuse/README.md' },
      { label: 'FUSE body codec tests', href: 'https://github.com/andymac4182/mount-rs/blob/main/integrations/mount-rs-napi/test/fuse-codec.mjs' },
      { label: 'N-API FUSE parity ledger', href: 'https://github.com/andymac4182/mount-rs/blob/main/docs/public-api-parity.md' },
      { label: 'W01 FUSE progress tracker', href: 'https://github.com/andymac4182/mount-rs/blob/main/docs/W01_FUSE_PROGRESS.md' },
      { label: 'CLI native prerequisites', href: 'https://github.com/andymac4182/mount-rs/blob/main/crates/mount-rs-cli/README.md#native-prerequisites' },
      { label: 'Historical hosted Linux transport CI', href: 'https://github.com/andymac4182/mount-rs/actions/runs/35575442663' },
    ],
  },
  nfs: {
    slug: 'nfs',
    name: 'NFS',
    eyebrow: 'Transport / network filesystem protocol',
    maturity: 'Preview',
    maturityNote: 'Native macOS NFSv3 and Linux checkpoints plus shared v3/v4 session and connection views exist; rootless reconnect, bounded pipelining, and v4 channel/state knobs pass, while the current workstream remains NO-GO pending native NFSv4.1, hosted lifecycle, and crash/durability qualification.',
    summary: (
      <>
        NFS is the current native macOS path and a Linux option when the host
        client and mount permissions are available. Its server and wire
        sessions can also be tested over loopback without mounting anything.
      </>
    ),
    platform: (
      <>
        macOS uses <code>/sbin/mount_nfs</code> and can run the owned temporary
        mountpoint rootlessly under the documented privacy policy. Linux needs
        the distribution NFS client tools and normally <code>CAP_SYS_ADMIN</code>
        or root for a kernel mount.
      </>
    ),
    access: (
      <>
        Rootless TCP/XDR tests exercise NFSv3, MOUNTv3, and the implemented
        NFSv4.1 session service. <code>mount_nfs</code> is a separate lifecycle
        bridge; the native test must be explicitly enabled and hard-fails when
        prerequisites are missing.
      </>
    ),
    surface: (
      <>
        The common filesystem/session path includes handles, namespace I/O,
        stateids, and selected byte-range locks. Held backend handles retain
        object identity across unlink/rename. The N-API views now expose shared
        v3/v4 session state, sorted BigInt handle snapshots, active connection
        counts, and stable live client peer/session objects with close/wait
        lifecycle. The server does not start <code>rpcbind</code>; it uses an
        explicitly selected loopback TCP port.
      </>
    ),
    verifyLabel: 'Run the platform-specific native test only when prepared',
    verifyCode: `# macOS or Linux, NFSv3:
MOUNT_RS_NFS_NATIVE_TEST=1 \
  cargo test -p mount-rs-nfs --test native_mount \
  -- --ignored --exact native_loopback_mount_round_trip --nocapture

# Linux only, NFSv4.1:
MOUNT_RS_NFS_NATIVE_V4_TEST=1 \
  cargo test -p mount-rs-nfs --test native_mount \
  -- --ignored --exact native_loopback_mount_v4_1_round_trip --nocapture`,
    safeUse: (
      <>
        Use an ephemeral loopback port and an empty temporary mountpoint. For
        SQLite's single-host profile, opt in explicitly and prefer DELETE
        journaling; the profile is not cross-host locking or power-loss proof.
      </>
    ),
    limitations: (
      <>
        NFSv4.1 is not full RFC parity, macOS remains v3-only in the current
        path, and lock state is process-local/advisory. UDP, portmapper/NLM,
        persistent cross-process recovery, distributed SQLite locking, and
        universal WAL support are not claimed.
      </>
    ),
    evidence: (
      <>
        Current-tree Rust and N-API checks retain backend handles across NFSv3
        unlink and NFSv4 rename, expose shared v3/v4 state, and exercise live
        connection/client close and wait behavior. The focused package packet
        passed 35 unit, rootless wire 1, transport concurrency 1,
        transport-error 4, v4 barrier 1, and v4 wire 6 cases; the release
        addon, generated typecheck, and live N-API server integration also
        pass. Rootless NFSv4.1 survives an orderly TCP reconnect while the
        server remains alive, and eight pipelined NFSv3 MOUNT NULL calls pass
        with <code>max_in_flight=4</code>. A restart-boundary test confirms
        the old v4 session is rejected with <code>NFS4ERR_BADSESSION</code> by
        a replacement server, proving that session/lease state is process-local
        rather than crash-durable. The v4 state packet now covers lease,
        session, fore-slot, COMPOUND, replay-cache, open/lock, and reclaim
        ceilings, including <code>NFS4ERR_TOOSMALL</code>,
        <code>NFS4ERR_NOSPC</code>, refused-session replay, and per-file lock
        enforcement. Static NFSv4 identity maps now qualify mapped user and
        group names with numeric fallback and reject other domains with
        <code>NFS4ERR_BADOWNER</code>; reclaim ordering returns
        <code>NFS4ERR_GRACE</code> until <code>RECLAIM_COMPLETE</code>, and
        deterministic lease expiry releases sessions, locks, open state, and
        pinned handles. The refreshed opt-in macOS native NFSv3 loopback mount
        passed 1/1 in 0.11s with filesystem round trips and bounded cleanup.
        The pinned oracle passes 266 NFSv3/MOUNT and NFSv4.1 TCP cases with 18
        capability/root skips; bounded <code>maxHandles</code> LRU and NFSv4
        open-state pinning are covered. Native Linux NFSv4.1, the full
        stateful/member matrix, hosted lifecycle, and
        crash/concurrency/durability remain external gates.
      </>
    ),
    sources: [
      { label: 'NFS transport README', href: 'https://github.com/andymac4182/mount-rs/blob/main/transports/mount-rs-nfs/README.md' },
      { label: 'W01 NFS progress tracker', href: 'https://github.com/andymac4182/mount-rs/blob/main/docs/W01_NFS_PROGRESS.md' },
      { label: 'Public API parity ledger', href: 'https://github.com/andymac4182/mount-rs/blob/main/docs/public-api-parity.md' },
      { label: 'SQLite-over-NFS boundary', href: 'https://github.com/andymac4182/mount-rs/blob/main/README.md#node-split-store-api' },
      { label: 'Historical hosted Linux transport CI', href: 'https://github.com/andymac4182/mount-rs/actions/runs/35575442663' },
    ],
  },
  '9p': {
    slug: '9p',
    name: '9P2000.L',
    eyebrow: 'Transport / lightweight TCP filesystem protocol',
    maturity: 'Experimental',
    maturityNote: 'Rootless protocol/server, attached Node Duplex, and a hosted Linux lifecycle qualification now pass for the supported scope; public parity remains partial, and crash/reset/half-close recovery is supervisor-owned rather than a library guarantee.',
    summary: (
      <>
        9P is a mount-free-friendly transport: the server and per-connection
        fid state run over Tokio TCP, while native Linux mounting is a separate
        kernel capability. macOS has no built-in 9P client in this project.
      </>
    ),
    platform: (
      <>
        Rootless TCP tests run on macOS and Linux. Native Linux needs the
        <code>9p</code>, <code>9pnet</code>, and <code>9pnet_tcp</code> kernel
        modules plus mount capability, normally <code>CAP_SYS_ADMIN</code>.
        macOS would require an external userspace client.
      </>
    ),
    access: (
      <>
        The standard path is mount-free: start <code>P9Server</code> and test
        frames or a loopback client. A Linux mount is an explicit host test,
        not something the Rust transport wrapper silently provides.
      </>
    ),
    surface: (
      <>
        The implementation covers 9P2000.L attach/walk, open/create, I/O,
        metadata, links, rename/unlink, locks, and session state. The N-API
        facade exposes direct <code>P9Session.handleCall</code>/<code>destroy</code>,
        attached stream identity/peer/closed state, ownership and duplicate
        attach handling, bounded backpressure, and write-fault reporting.
        Native Tokio connections deliberately expose no Node stream; callers
        needing a Node <code>Duplex</code> use <code>server.attach</code>.
        Authentication, xattr messages, legacy message families, and several
        unsupported driver capabilities return explicit unsupported errors.
      </>
    ),
    verifyLabel: 'Exercise the portable wire surface first',
    verifyCode: `cargo test -p mount-rs-9p --locked
cargo clippy -p mount-rs-9p --all-targets --locked -- -D warnings

# Prepared Linux host only:
sudo mount -t 9p -o trans=tcp,version=9p2000.L,port=<PORT> \
  127.0.0.1 /mnt/mount-rs-9p`,
    safeUse: (
      <>
        Bind the server to the intended address, choose a unique test
        mountpoint, and unmount explicitly. Keep any authentication or service
        credentials in the host environment; the transport tests themselves
        do not create credentials.
      </>
    ),
    limitations: (
      <>
        There is no native mount wrapper and no built-in macOS client. The
        protocol does not claim legacy 9P families, xattrs, authentication, or
        full upstream parity; rootless success is not a kernel mount result.
      </>
    ),
    evidence: (
      <>
        The current attached-stream/session packet passed focused Rust/N-API
        lifecycle and fault tests, strict Clippy, generated typechecks, and
        the pinned 44-case 9P codec differential. Dedicated hosted run
        <code>35628187344</code> at the exact published revision passed
        <code>9p</code>/<code>9pnet_fd</code> probing and all four ignored native
        tests: eight-worker concurrent mounted I/O, server-close/kernel-
        connection release, ordinary mount/unmount, and external umount. This
        closes the supported Linux lifecycle scope; automatic recovery after a
        process crash, arbitrary kernel reset, or half-close remains outside
        the library contract and needs supervisor-level evidence. The N-API
        surface now also exposes scalar options, <code>userFor</code>, and a
        transport-backed live lock client, while upstream driver/fid/debug,
        full fid-graph, option-injection, and property-shaped client parity
        remain open. Follow-on parity checks cover all 124 upstream 9P
        constants and <code>messageName</code> results plus live session fids,
        cursor/open state, hardlink identity, clunk snapshots, and retained
        open-handle enumeration; focused Rust tests report 30 passes. Overall
        production status remains NO-GO.
      </>
    ),
    sources: [
      { label: '9P transport README', href: 'https://github.com/andymac4182/mount-rs/blob/main/transports/mount-rs-9p/README.md' },
      { label: 'W01 9P progress tracker', href: 'https://github.com/andymac4182/mount-rs/blob/main/docs/W01_9P_PROGRESS.md' },
      { label: 'Porting status', href: 'https://github.com/andymac4182/mount-rs/blob/main/PORTING_STATUS.md' },
      { label: 'Hosted Linux 9P lifecycle CI', href: 'https://github.com/andymac4182/mount-rs/actions/runs/35616832528' },
      { label: 'Current hosted Native 9P qualification', href: 'https://github.com/andymac4182/mount-rs/actions/runs/35628187344' },
    ],
  },
  fskit: {
    slug: 'fskit',
    name: 'macOS FSKit',
    eyebrow: 'Transport / Apple extension boundary',
    maturity: 'Planned',
    maturityNote: 'Unsigned SDK/worker checkpoint with a 12-test Rust bridge suite and arm64 build evidence; signing, installation, activation, and real mounts remain pending.',
    summary: (
      <>
        FSKit is the native macOS extension path for a future mounted volume.
        The current work proves the Swift/Rust worker seam and bundle layout,
        not an installed or activated product.
      </>
    ),
    platform: (
      <>
        The extension targets the installed FSKit V1 SDK with a macOS 15.4
        deployment floor; path-backed resources use the macOS 26+ API described
        by the integration. A containing app, provisioning profile, Apple
        entitlement, signing identity, and user approval are required for a
        real activated volume.
      </>
    ),
    access: (
      <>
        The current bridge is mount-free: in-process worker/XPC tests call the
        Rust driver and reopen data through a second worker. The unsigned host
        embeds the appex but is not registered, launched by launchd, or enabled
        in System Settings.
      </>
    ),
    surface: (
      <>
        Swift translates FSKit operations to a little-endian bounded Rust
        worker frame with a 1 MiB body limit and 128 KiB data chunks; Rust owns
        filesystem, provider, locking, and lifecycle behavior. The bridge
        supports memory, rooted host, SQLite, and split SQLite backends in its
        current test configuration.
      </>
    ),
    verifyLabel: 'Verify the unsigned seam without calling it a mount',
    verifyCode: `cargo test --manifest-path integrations/mount-rs-fskit/Cargo.toml --locked

xcodebuild -project integrations/mount-rs-fskit/MountRsFSKit.xcodeproj \
  -scheme MountRsHost -configuration Debug -sdk macosx26.5 \
  CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO build`,
    safeUse: (
      <>
        Do not attempt installation with an unsigned artifact. Keep backend
        paths and chunk sizes in the host environment, and treat the containing
        app as a build artifact until signing and user-approved activation are
        available.
      </>
    ),
    limitations: (
      <>
        No valid Apple signing identity, provisioning profile, launchd
        registration, or FSKit activation test is currently recorded. The
        local Rust toolchain can build arm64 targets and the extension for
        x86_64, but cannot link the x86_64 XPC service because that Rust target
        is not installed. Optional xattrs, offloaded I/O, extent/preallocation,
        and special-node surfaces also remain separate gaps. FUSE/NFS fallback
        does not close this stream.
      </>
    ),
    evidence: (
      <>
        The Rust bridge suite has 12 passing tests covering bounded frames,
        malformed input, provider errno propagation, handle shutdown, rooted
        host round trips, and durable split-SQLite reopen. Swift frame tests,
        in-process XPC lifecycle tests, and unsigned arm64/Xcode builds also
        pass on the recorded macOS 26.5.1 arm64 SDK/Xcode 26.6 environment.
        The activation script now has an explicit required mode that only
        passes after signing, installation, user approval, and a real read/write
        mount check; without those prerequisites it reports a diagnostic block.
        All of this remains transport/worker evidence rather than a mounted-
        volume result.
      </>
    ),
    sources: [
      { label: 'FSKit integration README', href: 'https://github.com/andymac4182/mount-rs/blob/main/integrations/mount-rs-fskit/README.md' },
      { label: 'FSKit workstream', href: 'https://github.com/andymac4182/mount-rs/blob/main/WORK_TRACKER.md#-w13--fskit' },
    ],
  },
  http: {
    slug: 'http',
    name: 'HTTP multi-drive API',
    eyebrow: 'Transport / unmounted service',
    maturity: 'Preview',
    maturityNote: 'Loopback service with bearer isolation, ranges, streaming, reopen, and bounded health/readiness probes; a hosted all-features HTTP/OTLP gate now passes, while deployment hardening remains the caller’s job.',
    summary: (
      <>
        The HTTP service exposes named mount-rs drives without creating a
        kernel mount. It is the cleanest boundary for containers, remote
        clients, and environments where FUSE/NFS/FSKit are unavailable.
      </>
    ),
    platform: (
      <>
        Any platform that can run the CLI/service and an HTTP client can use
        the mount-free API. The default listener is loopback with bounded
        requests and an ephemeral port; deployments binding beyond loopback
        need an authenticated TLS boundary.
      </>
    ),
    access: (
      <>
        There is no FUSE, NFS, 9P, or FSKit mount. Each named drive retains its
        own <code>FsDriver</code>, stable ID, and bearer token; the API maps
        HTTP verbs to filesystem operations and one-byte-range reads.
      </>
    ),
    surface: (
      <>
        <code>/v1/drives</code> lists authorized drives; <code>fs</code> routes
        read/write/remove paths; <code>entries</code> lists directories; and
        bounded JSON operations cover mkdir, rename, truncate, and sync.
        Unknown drives and invalid tokens fail closed.
      </>
    ),
    verifyLabel: 'Run the local unmounted service with owned tokens',
    verifyCode: `MOUNT_RS_MEMORY_TOKEN=demo-memory \
  MOUNT_RS_SQLITE_TOKEN=demo-sqlite \
  cargo run --locked -p mount-rs-cli -- serve-http \
  --config crates/mount-rs-cli/examples/config-http.json

curl -H 'Authorization: Bearer demo-memory' \
  http://127.0.0.1:PORT/v1/drives/memory/entries/`,
    safeUse: (
      <>
        Keep bearer tokens in environment variables or a secret manager, never
        JSON or source. Put a TLS/authenticated reverse proxy in front of a
        non-loopback listener and bound request/chunk sizes to the deployment.
      </>
    ),
    limitations: (
      <>
        <code>PUT</code> is a bounded streaming write, not an atomic publish;
        a disconnect can leave a partial file. Distributed caching and
        cross-process version coordination are outside this slice. The local
        demo does not prove internet-facing deployment security.
      </>
    ),
    evidence: (
      <>
        Local HTTP/CLI checks cover bearer isolation, streamed reads/writes,
        ranges, truncate, concurrent writes, restart/reopen, and cleanup. The
        optional <code>mount-rs-observability</code> seam can attach a
        caller-owned telemetry handle through
        <code>HttpServerOptions::with_telemetry</code>; the OTLP variant accepts
        only W3C trace propagation and emits bounded operation metadata without
        paths, tokens, file contents, block IDs, or arbitrary headers. The
        macOS arm64 W30.5 loopback collector test now receives non-empty
        <code>/v1/traces</code>, <code>/v1/metrics</code>, and
        <code>/v1/logs</code> payloads without raw path bytes, and the HTTP
        observability integration gate recorded 7 passes. Hosted CI run
        <code>35599817215</code>, source <code>b26819e</code>, also passed the
        all-features HTTP health/readiness, telemetry/OTLP, and strict-Clippy
        job <code>106333141914</code>. This is hosted exporter-path and
        contract evidence only: deployed collector reachability, provider-
        aware readiness, dashboards, paging, SLOs, and production hardening
        remain open.
      </>
    ),
    sources: [
      { label: 'HTTP transport README', href: 'https://github.com/andymac4182/mount-rs/blob/main/crates/mount-rs-http/README.md' },
      { label: 'HTTP observability boundary', href: 'https://github.com/andymac4182/mount-rs/blob/main/docs/observability.md' },
      { label: 'Observability platform qualification', href: 'https://github.com/andymac4182/mount-rs/blob/main/docs/w30.5-platform-qualification.md' },
      { label: 'HTTP server options and telemetry seam', href: 'https://github.com/andymac4182/mount-rs/blob/main/crates/mount-rs-http/src/server.rs' },
      { label: 'Hosted HTTP observability CI', href: 'https://github.com/andymac4182/mount-rs/actions/runs/35599817215' },
      { label: 'CLI HTTP example', href: 'https://github.com/andymac4182/mount-rs/blob/main/crates/mount-rs-cli/README.md#quick-local-demo' },
    ],
  },
  webdav: {
    slug: 'webdav',
    name: 'WebDAV',
    eyebrow: 'Transport / HTTP filesystem protocol',
    maturity: 'Preview',
    maturityNote: 'Pinned pure protocol differential, direct N-API streaming, active-lock, method, peer-fault, same-session concurrency, and a local macOS native round trip pass within scope; member parity, restart/durability, provider, and hosted gates remain open.',
    summary: (
      <>
        WebDAV makes the filesystem contract available through standard HTTP
        resource methods. The portable server is mount-free; native mounting
        is a separate davfs/macOS host-client test.
      </>
    ),
    platform: (
      <>
        Rootless HTTP tests run on macOS and Linux. Linux native mounting needs
        <code>mount.davfs</code>, FUSE, <code>/dev/fuse</code>, and mount
        privileges; macOS uses <code>/sbin/mount_webdav</code> and host
        authorization policy.
      </>
    ),
    access: (
      <>
        The portable path is an HTTP server over an <code>FsDriver</code> and
        does not mount anything. The ignored native harness is only selected
        when its platform prerequisites are present and explicitly enabled.
      </>
    ),
    surface: (
      <>
        The server implements class-1 OPTIONS/GET/HEAD/PUT/MKCOL/DELETE/COPY/
        MOVE/PROPFIND and bounded class-2 locking/property behavior. Reads are
        streamed with positional operations; uploads are bounded and incremental.
      </>
    ),
    verifyLabel: 'Keep protocol and native checks separate',
    verifyCode: `cargo test -p mount-rs-webdav --locked

# Prepared host only, Linux or macOS:
MOUNT_RS_WEBDAV_NATIVE_TEST=1 \
  cargo test -p mount-rs-webdav --test native_mount \
  -- --ignored --nocapture`,
    safeUse: (
      <>
        Bind the service to loopback during tests, bound XML/request bodies,
        and use an owned empty mountpoint for native checks. Add TLS and
        authentication before exposing WebDAV beyond a trusted local boundary.
      </>
    ),
    limitations: (
      <>
        Multi-range requests are intentionally unsupported, unknown properties
        are protected, and the native harness is not part of ordinary tests.
        A passing HTTP protocol test is not universal client or mount
        interoperability evidence.
      </>
    ),
    evidence: (
      <>
        The current tracker records 13 Rust WebDAV tests, a passing isolated
        locked N-API check, release addon/declaration generation, and a direct
        buffered session/options/driver/auth slice. The pinned pure protocol
        differential now passes at the recorded oracle revision.
        The direct streaming facade passes a three-chunk PUT, multi-chunk GET,
        early response-iterator return, and deliberate request-body failure
        mapping for async iterables and Web ReadableStreams. Active
        <code>WebdavLockView</code> state exposes direct LOCK/UNLOCK records;
        an unsubmitted token receives <code>423</code>, expiry returns the
        session view to zero, and the direct method matrix covers OPTIONS,
        MKCOL, PUT, HEAD, GET, PROPFIND, PROPPATCH, COPY, MOVE, LOCK, UNLOCK,
        DELETE, and explicit PATCH refusal. A peer reset produces exactly one
        typed transport callback with the accepted loopback peer. A malformed
        HTTP request produces one typed callback and clean socket/server
        teardown. Same-driver recreation preserves file bytes while resetting
        session locks, and eight parallel direct-session PUTs followed by GETs
        all return matching bodies. The N-API listener remains blocked in this
        sandbox by its loopback bind prerequisite. The current postlude also
        normalizes method counters to the oracle's <code>Map&lt;string,
        number&gt;</code> shape and exposes a request-level
        <code>onError(error, head)</code> callback. The explicit macOS native
        harness passed 1/1 on Darwin 27 arm64 using
        <code>/sbin/mount_webdav</code> and <code>/sbin/umount</code>; hosted
        macOS/Linux lifecycle and provider rows remain separate, and current
        hosted workflow snapshots are canceled or pending rather than a
        WebDAV PASS. No local protocol or native pass is promoted to a
        production mount claim.
      </>
    ),
    sources: [
      { label: 'WebDAV transport README', href: 'https://github.com/andymac4182/mount-rs/blob/main/transports/mount-rs-webdav/README.md' },
      { label: 'W01 WebDAV progress tracker', href: 'https://github.com/andymac4182/mount-rs/blob/main/docs/W01_WEBDAV_PROGRESS.md' },
      { label: 'Transport evidence', href: 'https://github.com/andymac4182/mount-rs/blob/main/PORTING_STATUS.md' },
    ],
  },
} as const satisfies Record<string, TransportSpec>

export function TransportIndex() {
  return (
    <article className="doc-article transport-index">
      <p className="eyebrow">Documentation / transports</p>
      <h1>Put the filesystem at the edge you actually have.</h1>
      <p className="doc-lede">
        A transport changes how a client reaches the same filesystem contract;
        it does not erase provider limits. These pages separate mount-free
        protocol evidence from native host mounting.
      </p>
      <div className="transport-card-grid">
        {Object.values(transportSpecs).map((transport) => (
          <Link className="transport-card" key={transport.slug} to={`/docs/transports/${transport.slug}`}>
            <div className="provider-card-top">
              <span className="eyebrow">{transport.eyebrow}</span>
              <MaturityBadge maturity={transport.maturity} />
            </div>
            <h2>{transport.name}</h2>
            <p>{transport.maturityNote}</p>
            <span className="card-arrow" aria-hidden="true">→</span>
          </Link>
        ))}
      </div>
      <div className="callout callout-amber">
        <strong>Mount-free is a first-class path.</strong>
        <p>
          Loopback, HTTP, and userspace protocol tests can be valuable without
          kernel privileges. They must still be labeled as protocol evidence,
          not silently promoted to native mount acceptance.
        </p>
      </div>
      <div className="source-note">
        <span className="source-note-mark" aria-hidden="true">↗</span>
        <p>
          Read the <a href="https://github.com/andymac4182/mount-rs/blob/main/PORTING_STATUS.md">porting status</a>
          {' '}and <a href="https://github.com/andymac4182/mount-rs/blob/main/WORK_TRACKER.md">work tracker</a>
          {' '}for revision-specific platform evidence.
        </p>
      </div>
    </article>
  )
}

export function TransportPage({ transport }: { transport: TransportSpec }) {
  return (
    <article className="doc-article transport-page">
      <div className="doc-kicker-row">
        <p className="eyebrow">{transport.eyebrow}</p>
        <MaturityBadge maturity={transport.maturity} />
      </div>
      <h1>{transport.name}</h1>
      <p className="doc-lede">{transport.summary}</p>

      <div className="provider-facts">
        <div className="provider-fact">
          <span className="fact-label">Platform requirements</span>
          <p>{transport.platform}</p>
        </div>
        <div className="provider-fact">
          <span className="fact-label">Mount vs mount-free</span>
          <p>{transport.access}</p>
        </div>
      </div>

      <div className="callout callout-blue">
        <strong>{transport.maturity} — scoped evidence</strong>
        <p>{transport.maturityNote} {transport.evidence}</p>
      </div>

      <h2>What the transport exposes</h2>
      <p>{transport.surface}</p>

      <h2>{transport.verifyLabel}</h2>
      <CodeBlock label="Evidence-oriented check">{transport.verifyCode}</CodeBlock>

      <h2>Safe use and lifecycle</h2>
      <p>{transport.safeUse}</p>

      <h2>Current limitations</h2>
      <p>{transport.limitations}</p>

      <div className="source-note">
        <span className="source-note-mark" aria-hidden="true">↗</span>
        <p>
          Sources:{' '}
          {transport.sources.map((source, index) => (
            <span key={source.href}>
              {index > 0 ? ' · ' : ''}<a href={source.href}>{source.label}</a>
            </span>
          ))}
        </p>
      </div>
      <p className="inline-source">
        <Link to="/docs/transports">← Back to transports</Link>
      </p>
    </article>
  )
}
