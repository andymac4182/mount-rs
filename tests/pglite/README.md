# Local PGlite backend tests

This server is a credential-free PostgreSQL-wire endpoint for the Rust
PGlite integration tests. It is not a native filesystem mount test.

Install the pinned local server dependencies once:

```sh
pnpm --dir tests/pglite install --frozen-lockfile
```

To exercise the Unix-domain-socket path on macOS or Linux, use a directory
whose socket name is the PostgreSQL convention expected by `tokio-postgres`:

```sh
socket_dir="/tmp/mount-rs-pglite-local-$$"
mkdir "$socket_dir"
echo "Copy this socket directory for the test shell: $socket_dir"
PGLITE_MAX_CONNECTIONS=4 node tests/pglite/server.mjs \
  "$socket_dir/.s.PGSQL.5432"
```

For a restart-persistent local database, set `PGLITE_DATA_DIR` to a separate
directory when starting the server. With no `PGLITE_DATA_DIR`, the server is
deliberately in-memory and only tests reconnects while that server process is
alive.

In another shell, replace the example directory with the value printed above
and point the test at the socket directory (not the socket file):

```sh
socket_dir="/tmp/mount-rs-pglite-local-REPLACE_ME"
MOUNT_RS_REQUIRE_PGLITE=1 \
PGLITE_DATABASE_URL="postgresql://postgres:postgres@/postgres?host=$socket_dir&sslmode=disable" \
cargo test --test backend_parity pglite_matches_the_same_contract_when_a_socket_is_configured -- --nocapture
```

The server allows several sessions so the test can open independent instances
and reconnect while checking durable state. The repository acceptance script uses
the same server over a loopback TCP port; both paths exercise the PostgreSQL
wire protocol and backend persistence, not kernel mount support.

## Native two-CLI PGlite metadata with RustFS blocks

On macOS, with Docker running and the pinned PGlite dependencies installed,
run the disposable RustFS harness with the native pairing runner:

```sh
CARGO_TARGET_DIR=/private/tmp/mount-rs-cargo-target \
RUSTFS_COMBO_NAME=pglite-concurrent-rustfs \
RUSTFS_COMBO_TIMEOUT_SECONDS=900 \
RUSTFS_COMBO_COMMAND='python3 ./scripts/test-native-pglite-rustfs-two-process.py' \
./scripts/test-rustfs.sh
```

The runner checks the harness ownership marker and loopback RustFS endpoint.
The ignored native test starts its own persistent PGlite socket server, mounts
the same volume through two CLI processes, and verifies bidirectional writes,
160 acknowledged file lifecycle calls, disjoint writes to one file, rename,
unlink, clean unmount, and fresh PGlite server/CLI reopen with RustFS block
readback. The harness owns its Docker container and bucket; the native test
owns its PGlite server, NFS mountpoints, and temporary files.
The runner uses a private temporary root and preserves it if an interrupted
test leaves an owned NFS mount attached or a child process cannot be stopped.
Cargo, the native test, CLI, and Node inherit the disposable RustFS combo
process group, so the outer watchdog can stop them even if this runner is
force-killed. The runner signals only its exact Cargo leader and preserves
the private root if descendants remain for the outer watchdog to stop. Dry
exited-leader and outer force-kill probes run without Docker or native mounts:

```sh
python3 tests/pglite/native_runner_cleanup_probe.py
```

Observed on macOS on 2026-09-23: the native case passed 1/1; its 160-call load
took 5.660 seconds. The runner reported exact NFS and temporary-file cleanup,
the RustFS combo and full harness exited successfully, and the run-owned
service and cleanup containers were absent on readback. The test uses one
PGlite engine with two PostgreSQL-wire clients and a loopback RustFS service;
it does not qualify a listener or storage path across physical hosts.
