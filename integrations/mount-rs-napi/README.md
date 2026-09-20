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
  `mount()`, reads a seeded file through the kernel mount, writes and reads it
  back, verifies callbacks were reached, and checks unmount cleanup.

The structural adapter bridges both string open modes and decoded numeric
`open_flags`, so native host writes are part of this acceptance check rather
than a portable-only capability. The test also verifies that the bytes written
through the mount are visible through the original backing driver after
unmount.

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

On Linux, the CI acceptance path installs `fuse3`, verifies `/dev/fuse` and
`fusermount3`, then forces the probed `fuse` transport. The equivalent local
command is:

```sh
MOUNT_RS_NAPI_STRUCTURAL_NATIVE_MOUNT=1 \
MOUNT_RS_NAPI_STRUCTURAL_TRANSPORT=fuse \
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
is unavailable and includes the probe's prerequisite reason in the failure.
If mount or teardown fails, the mountpoint is printed and is not recursively
removed, so an active host mount is never hidden by cleanup.

## Optional observability

The native crate has an opt-in `observability` feature that decorates every
native, structural-JavaScript, memory-factory, and unstorage filesystem driver
at the Node application boundary. The feature is disabled by default and does
not change the exported API or add exporter setup to the default build.

```sh
MOUNT_RS_TELEMETRY=1 node-gyp-build integrations/mount-rs-napi
```

When the feature is enabled, set `MOUNT_RS_TELEMETRY=1` before loading the
module to activate the bounded operation spans and local metrics. A Rust host
embedding this crate may install an application-owned telemetry handle before
constructing a filesystem. Exporter/provider lifecycle remains owned by that
host; Node operation failures are never made dependent on telemetry export.
