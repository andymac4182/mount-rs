# mount-rs CLI

mount-rs is the native command-line front end for this repository. Its
default is intentionally close to the pinned mountx CLI: an in-memory
filesystem with a README, a request watcher, and one native mount chosen by
the auto facade.

The command accepts one positional mountpoint:

    mount-rs [mountpoint] [options]
    mount-rs mount [mountpoint] [options]
    mount-rs probe

--help, --version, and probe do not create a driver or a mount. The probe
output describes the current host's FUSE, 9P, and NFS prerequisites and the
auto preference. Naming a transport skips that probe and does not fall back
after a mount error. Ctrl-C calls the selected mount's actual unmount
operation before the process exits.

The built-in driver choices are:

- --driver memory (default): volatile memory, seeded with README.md unless
  --empty is supplied.
- --driver host --root PATH: a rooted host filesystem; the default root is
  the current directory. Host paths are kept below the configured root by the
  host integration.
- --driver sqlite --database PATH: the durable SQLite snapshot integration.
- --driver splitstore: independent metadata and immutable block stores. With
  no paths it is volatile memory; with both --database METADATA and --blocks
  BLOCKS it uses two durable SQLite databases.

The CLI never uses a native mount in its ordinary parser, help, version, probe,
or watcher tests. Those tests verify the protocol-free portions only; they do
not claim a native kernel mount on either platform.

## Native prerequisites

On Linux, automatic selection prefers FUSE, then 9P, then NFS. Rootless FUSE
needs /dev/fuse and fusermount3 (or fusermount) available on PATH. Native 9P
and Linux NFS mounting normally require CAP_SYS_ADMIN/root and the
corresponding kernel/client support. A named transport is attempted once, so
an unavailable named transport is reported rather than silently replaced.

On macOS, automatic selection prefers NFS. The process must be allowed to use
the native NFS client and an unprivileged user must own the mountpoint.
Privacy/Full Disk Access policy can also reject a mount or unmount; grant the
terminal/process the required permission and retry. macFUSE is a different
protocol and is not treated as this crate's FUSE transport.

The transport probe is an availability explanation, not a native mount test.
Run a real mount explicitly on a disposable, user-owned mountpoint only when
the host prerequisites are installed.
