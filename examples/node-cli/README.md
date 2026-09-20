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

Run the SDK-backed driver example without a native mount. This is the portable
consumer smoke test used by the integration suite:

```sh
node examples/node-cli/index.mjs \
  --driver memory \
  --sdk-self-test
```

The Node CLI also accepts the same versioned provider configuration shape as
the Rust CLI. This opens the configured providers through the public N-API
SDK, writes and reads a file, shuts down, then recreates the driver and checks
the persisted readback:

```sh
PGLITE_DATABASE_URL='postgresql://postgres:postgres@127.0.0.1:5432/postgres?sslmode=disable' \
node examples/node-cli/index.mjs \
  --config /path/to/pglite-config.json \
  --sdk-self-test --reopen
```

For a live run, use a config whose `driver.storage` has PGlite metadata and
either PGlite or R2 blocks. The provider matrix generates this temporary
config during its PGlite run. Credentials remain environment references in
the JSON; the CLI passes their resolved values only to the public SDK factory.
Use `--check` to validate a config without loading native code or resolving
credentials.

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

The CLI never accepts plaintext provider credentials in its JSON; configured
providers refer to environment variables and runtime use passes those values
directly to the public SDK. Native mounting is an explicit operation;
`--check` remains mount-free and does not resolve credentials, while the SDK
self-test exercises the public driver API without a transport. The integration
test runs both paths.
