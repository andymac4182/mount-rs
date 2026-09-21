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
        <code>SYMLINK</code>, <code>MKNOD</code>, <code>MKDIR</code>,
        <code>UNLINK</code>, <code>RMDIR</code>, <code>RENAME</code>,
        <code>LINK</code>, <code>ACCESS</code>,
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
        raw-layout <code>IOCTL</code> request/reply and typed
        <code>BMAP</code> request/reply bodies are now covered by pinned-oracle
        differentials as well; these remain focused codecs rather than a full
        native session. The latest packet also adds typed
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
