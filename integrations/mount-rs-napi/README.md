# `@mount-rs/core` native structural-driver acceptance

The N-API surface accepts a structural JavaScript `FsDriver` in the server
factories and in `mount()`. `createDriver()` is the explicit adapter when a
caller wants a native `Filesystem` value for other APIs. The native mount
binding retains the plain structural object for the full mount lifecycle; it
does not take ownership of the caller's driver or call its `shutdown()`.

`test/structural-native.mjs` covers both boundaries:

- a portable adapter smoke test performs read/write I/O through
  `createDriver()`;
- malformed structural input is rejected before a host mount is attempted;
- an opt-in macOS/Linux test passes the plain structural object directly to
  `mount()`, reads a seeded file through the kernel mount, verifies callbacks
  were reached, and checks unmount cleanup.

There is one current capability boundary in the implementation:
the native structural path is read-only. The native transports issue decoded
write-only `OpenFlags` for host writes; the N-API structural adapter currently
implements the string `open()` contract but does not provide the corresponding
decoded `open_flags` override. The host reports an I/O or unsupported-operation
error (`EIO`, `ENOSYS`, `ENOTSUP`, or `EOPNOTSUPP`) for a write, while the
portable `createDriver()` smoke test still proves ordinary string open/read/write
behavior. The opt-in test asserts this exact boundary so a
future adapter implementation can change the assertion to full native
read/write acceptance without silently masking the gap.

The host-mount portion is deliberately opt-in because it requires a working
native transport and host mount privileges. The ordinary check is safe and
rootless:

```sh
node integrations/mount-rs-napi/test/structural-native.mjs
```

To exercise the real lifecycle on macOS or Linux:

```sh
MOUNT_RS_NAPI_STRUCTURAL_NATIVE_MOUNT=1 \
  node integrations/mount-rs-napi/test/structural-native.mjs
```

`auto` uses the platform probe. A named transport can be selected when the
host is configured for it:

```sh
MOUNT_RS_NAPI_STRUCTURAL_NATIVE_MOUNT=1 \
MOUNT_RS_NAPI_STRUCTURAL_TRANSPORT=nfs \
  node integrations/mount-rs-napi/test/structural-native.mjs
```

Linux may use `fuse`, `9p`, or `nfs`; macOS currently uses its native NFS
client. The test refuses an explicitly requested transport whose probe says it
is unavailable. If mount or teardown fails, the mountpoint is printed and is
not recursively removed, so an active host mount is never hidden by cleanup.
