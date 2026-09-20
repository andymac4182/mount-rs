# Core in-memory parity trace

`check.mjs` is a deterministic, mount-free differential test between the
root-package `mount-rs-core` memory filesystem and the pinned `pithings/mountx`
memory driver through each project’s loopback harness.

Run it with the pinned checkout and a writable temporary Cargo target:

```sh
CARGO_TARGET_DIR=/private/tmp/mount-rs-w01-target \
MOUNTX_SOURCE=/tmp/mountx-source.uWiHfX \
node tests/core_parity/check.mjs
```

The trace covers normalized paths, binary bytes, hard and symbolic links,
explicit `utimes`/`lutimes`, mode/owner/stat/statfs metadata, open-handle
cursor and positional I/O, unlink-surviving handles, sync/datasync/close
lifecycle, special nodes, and stable error codes. It masks only the
engine-clock-dependent `ctime` and `birthtime` values while asserting that
both are present; explicit atime/mtime values remain exact.

This packet intentionally skips persistence and snapshots, concurrent access,
remote/provider drivers, transports and HTTP, native mounts, versioning,
platform-specific behavior, and the wider Node package/export surface. Those
are separate W01 or later gates, not inferred from this trace. If
`MOUNTX_SOURCE` is unset, the command reports an oracle-unavailable skip
instead of claiming parity.
