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
