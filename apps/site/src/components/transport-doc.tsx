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
    maturityNote: 'Linux native mount and SQLite-hosting checkpoints exist; focused current-tree codec packets now cover additional FUSE operations and xattrs, while the latest recorded hosted Linux run passed the structural FUSE lifecycle at 37e9ba1 and current-tree requalification remains open.',
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
        <code>BATCH_FORGET</code>, <code>INTERRUPT</code>, <code>POLL</code>,
        <code>FALLOCATE</code>, <code>RENAME2</code>, <code>LSEEK</code>, and
        <code>COPY_FILE_RANGE</code>, <code>RELEASE</code>/<code>RELEASEDIR</code>,
        <code>FLUSH</code>, and <code>FSYNC</code>/<code>FSYNCDIR</code> bodies,
        plus <code>SETXATTR</code>/<code>GETXATTR</code>/<code>LISTXATTR</code>/
        <code>REMOVEXATTR</code> request/reply codecs and
        <code>READDIR</code>/<code>READDIRPLUS</code> directory codecs. These
        are focused mount-free boundaries: malformed or trailing advanced
        requests fail closed and valid unsupported operations still return
        <code>ENOSYS</code>. Full request/reply, init negotiation, session, and
        native-mount surfaces remain open.
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
        <code>RELEASE</code>/<code>RELEASEDIR</code>, <code>FLUSH</code>,
        <code>FSYNC</code>/<code>FSYNCDIR</code>,
        <code>packDirents</code>/<code>unpackDirents</code>, and
        <code>packDirentsPlus</code>/<code>unpackDirentsPlus</code>. Pinned
        7.8/7.39/7.41 differential tests cover protocol bytes, legacy layouts,
        truncation, trailing data, UTF-8 names, 8-byte alignment, bounded
        packing, integer coercion, malformed input, embedded-NUL rejection, and
        inode parity. Rust session tests cover focused <code>ACCESS</code>,
        <code>BATCH_FORGET</code>, and fail-closed <code>INTERRUPT</code>
        validation; six INIT tests cover negotiated <code>FUSE_INIT_EXT</code>
        and <code>flags2</code> handling. Hosted CI run 35499717435 passed Linux
        native FUSE/NFS/9P/WebDAV at 37e9ba1; newer current-tree CI is queued,
        so full request/reply, session, and native-mount surfaces stay open.
      </>
    ),
    sources: [
      { label: 'FUSE transport boundary', href: 'https://github.com/andymac4182/mount-rs/blob/main/transports/mount-rs-fuse/README.md' },
      { label: 'FUSE body codec tests', href: 'https://github.com/andymac4182/mount-rs/blob/main/integrations/mount-rs-napi/test/fuse-codec.mjs' },
      { label: 'N-API FUSE parity ledger', href: 'https://github.com/andymac4182/mount-rs/blob/main/docs/public-api-parity.md' },
      { label: 'CLI native prerequisites', href: 'https://github.com/andymac4182/mount-rs/blob/main/crates/mount-rs-cli/README.md#native-prerequisites' },
    ],
  },
  nfs: {
    slug: 'nfs',
    name: 'NFS',
    eyebrow: 'Transport / network filesystem protocol',
    maturity: 'Preview',
    maturityNote: 'Native macOS NFSv3 and Linux checkpoints exist; held-handle retention across unlink/rename is tested, while NFSv4.1 parity and distributed locking remain limited.',
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
        object identity across unlink/rename. The server does not start
        <code>rpcbind</code>; it uses an explicitly selected loopback TCP port.
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
        Current-tree Rust NFS tests retain backend handles across NFSv3 unlink
        and NFSv4 rename. Focused coverage passed 30 unit, 8 integration, and
        266 oracle cases with 18 capability-gated skips. The opt-in macOS Node
        CLI path also mounted HostFs through native NFS, verified read/write,
        unmount, persistence, and clean backing-directory teardown. The
        TypeScript control, Linux FUSE, and privileged cross-platform
        qualification remain separate.
      </>
    ),
    sources: [
      { label: 'NFS transport README', href: 'https://github.com/andymac4182/mount-rs/blob/main/transports/mount-rs-nfs/README.md' },
      { label: 'Public API parity ledger', href: 'https://github.com/andymac4182/mount-rs/blob/main/docs/public-api-parity.md' },
      { label: 'SQLite-over-NFS boundary', href: 'https://github.com/andymac4182/mount-rs/blob/main/README.md#node-split-store-api' },
    ],
  },
  '9p': {
    slug: '9p',
    name: '9P2000.L',
    eyebrow: 'Transport / lightweight TCP filesystem protocol',
    maturity: 'Experimental',
    maturityNote: 'Rootless protocol/server coverage with a Linux-native path; no built-in macOS client or full mount claim.',
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
        metadata, links, rename/unlink, locks, and session state. Authentication,
        xattr messages, legacy message families, and several unsupported driver
        capabilities return explicit unsupported errors.
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
        Complete protocol frames, session operations, and loopback TCP tests
        are covered; native Linux evidence is narrower and revision-specific.
      </>
    ),
    sources: [
      { label: '9P transport README', href: 'https://github.com/andymac4182/mount-rs/blob/main/transports/mount-rs-9p/README.md' },
      { label: 'Porting status', href: 'https://github.com/andymac4182/mount-rs/blob/main/PORTING_STATUS.md' },
    ],
  },
  fskit: {
    slug: 'fskit',
    name: 'macOS FSKit',
    eyebrow: 'Transport / Apple extension boundary',
    maturity: 'Planned',
    maturityNote: 'Unsigned SDK/worker checkpoint; signing, installation, activation, and real mounts remain pending.',
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
        Swift translates FSKit operations to the bounded Rust worker frame;
        Rust owns filesystem, provider, locking, and lifecycle behavior. The
        bridge supports memory, rooted host, SQLite, and split SQLite backends
        in its current test configuration.
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
        registration, or FSKit activation test is currently recorded. Optional
        xattrs, offloaded I/O, extent/preallocation, and special-node surfaces
        also remain separate gaps. FUSE/NFS fallback does not close this stream.
      </>
    ),
    evidence: (
      <>
        Rust bridge tests, Swift frame tests, XPC lifecycle tests, and unsigned
        arm64/Xcode builds are current evidence. They are deliberately labeled
        as transport/worker evidence rather than a mounted-volume result.
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
    maturityNote: 'Loopback service, bearer isolation, ranges, streaming, and reopen checks; deployment hardening remains the caller’s job.',
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
        paths, tokens, file contents, block IDs, or arbitrary headers. Collector
        reachability and broader multi-drive and hosted coverage remain open.
      </>
    ),
    sources: [
      { label: 'HTTP transport README', href: 'https://github.com/andymac4182/mount-rs/blob/main/crates/mount-rs-http/README.md' },
      { label: 'HTTP observability boundary', href: 'https://github.com/andymac4182/mount-rs/blob/main/docs/observability.md' },
      { label: 'HTTP server options and telemetry seam', href: 'https://github.com/andymac4182/mount-rs/blob/main/crates/mount-rs-http/src/server.rs' },
      { label: 'CLI HTTP example', href: 'https://github.com/andymac4182/mount-rs/blob/main/crates/mount-rs-cli/README.md#quick-local-demo' },
    ],
  },
  webdav: {
    slug: 'webdav',
    name: 'WebDAV',
    eyebrow: 'Transport / HTTP filesystem protocol',
    maturity: 'Preview',
    maturityNote: 'Protocol and native Linux/macOS checkpoints exist; class-2 and client-specific limits remain visible.',
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
        The repository records portable protocol coverage and revision-specific
        native Linux/macOS read/write/unmount checks. Remaining client,
        platform, and full upstream parity limits stay explicit.
      </>
    ),
    sources: [
      { label: 'WebDAV transport README', href: 'https://github.com/andymac4182/mount-rs/blob/main/transports/mount-rs-webdav/README.md' },
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
