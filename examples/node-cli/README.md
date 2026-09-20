# mount-rs Node SDK CLI example

This example uses the public `@mount-rs/core` Node SDK to select a Rust-backed
filesystem driver, mount it through the Rust transport facade, and access the
mounted path with ordinary Node `fs/promises` calls.

Validate arguments without loading native code or mounting:

```sh
node examples/node-cli/index.mjs \
  --driver memory \
  --transport nfs \
  --mountpoint /tmp/mount-rs-node-cli \
  --check
```

Run the bounded self-test on a host with a usable native transport:

```sh
node examples/node-cli/index.mjs \
  --driver memory \
  --transport nfs \
  --mountpoint /tmp/mount-rs-node-cli \
  --self-test
```

Use `--driver host --root PATH` for a rooted host-backed view. `--transport`
accepts `auto`, `fuse`, `9p`, or `nfs`; a named transport is attempted once and
does not silently fall back. The repository checkout resolves the local N-API
addon at `integrations/mount-rs-napi/index.js`. A published install can resolve
`@mount-rs/core`, or override resolution with `MOUNT_RS_NAPI_PACKAGE`.

The CLI never accepts or reads provider credentials. Native mounting is an
explicit operation; `--check` is mount-free and is the default path used by the
Node CLI integration test.
