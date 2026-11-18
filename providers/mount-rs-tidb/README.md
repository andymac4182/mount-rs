# mount-rs-tidb

TiDB-backed implementations of the core `MetadataStore` and `BlockStore`
contracts. The providers are independent: metadata and immutable bytes may be
connected with separate `TidbStorageOptions` values and composed with other
mount-rs providers.

## Transaction contract

- Metadata lease acquisition, renewal, release, and exceptional publication
  classification use TiDB's pessimistic mode in explicit transactions, with a
  volume-row `SELECT ... FOR UPDATE` where classification requires it. The
  provider's private pool configures and verifies each newly created session
  once, and disables connection-reset round trips because no caller can share
  the pool. TiDB Cloud services that expose `tidb_txn_mode` as read-only must
  retain pessimistic mode. The provider rejects optimistic, missing, or unknown
  modes rather than assuming a deployment default; the transaction guard still
  rolls back cancellation and early-return paths before connection reuse.
- Every provider session explicitly enables and verifies `autocommit=1`.
  A server's global autocommit setting cannot turn a reported publication or
  block acknowledgement into an uncommitted client transaction.
- TiDB's provider clock is read inside the transaction. A lease is valid only
  when owner, fence, exact expiry, and provider-clock expiry all match.
- Publication uses an atomic conditional update with the lease and expected
  revision in its predicate. The exceptional path locks the row to classify
  a failed predicate.
  Revision mismatch is `EAGAIN`; stale or expired fencing is `ESTALE`.
- Lock/deadlock conflicts known before commit map to `EAGAIN` and the caller
  may retry the complete operation. There is no automatic transaction replay.
  A single-statement publication is its own commit, so an error from it is a
  known non-commit only for TiDB error codes 9007 (write conflict), 1205
  (lock wait timeout) and 1213 (deadlock). Live tests provoke each of the
  three for an autocommit statement and check from the rows that the
  statement did not commit. Any other server error from such a statement,
  including 9006 (GC life time) and any other code whose message says
  "write conflict" or "try again later", is an unknown outcome.
  A lost publication acknowledgement or `COMMIT` error is returned as an I/O
  error with an unknown outcome and is
  never retried, because a lost connection cannot distinguish rollback from a
  committed transaction.
- Dropping a canceled operation returns its connection to `mysql_async`; the
  pool cleans dirty transactions with rollback before reuse. The provider
  never reports a successful durability barrier after a failed round trip.
- Block IDs are SHA-256 content identities. Duplicate puts never replace
  bytes; the existing bytes are read back and compared before the ID is
  returned. `delete` is explicit and never runs as a close side effect.

## Statement timeout

Every round trip to TiDB is bounded: each statement, each `COMMIT`, and each
connection attempt. The default is 120 seconds, chosen to sit well above
TiDB's own bounded waits so that it fires for a server that has stopped
answering rather than one that is slow. Set it with
`TidbStorageOptions::with_statement_timeout`, or with the
`statement_timeout_ms` parameter of the connection URL, which is how the SDK,
the CLI and the Node bindings set it:

```text
mysql://user:password@tidb.example:4000/mount?statement_timeout_ms=30000
```

The option wins over the URL. A `TidbStorePool` takes the value from its URL;
a store borrowed from it may shorten its own client deadline with the option.

Two bounds work together:

- The session sets `max_execution_time`, so TiDB interrupts a statement
  itself and the connection stays usable. TiDB applies it to `SELECT`
  (including `SELECT ... FOR UPDATE`) only; it does not interrupt `UPDATE`,
  `INSERT`, `DELETE` or `COMMIT`. Checked against TiDB v8.5.7 by
  `tests/statement_timeout.rs`.
- The client stops waiting one second after that limit. This is the only
  bound for writes, commits, connection attempts and a server that accepts
  bytes and never answers. The abandoned connection is closed, not reused.

A timeout is never retried. When it ends a `COMMIT` or a single-statement
publication, TiDB may already have committed, so the error says the commit
outcome is unknown, exactly as for a lost connection. When the configured
timeout is at or below the session's `innodb_lock_wait_timeout`, the provider
lowers the lock wait to one second less, so a contended row is still reported
by TiDB as a retryable conflict instead of as a timeout.

A timeout fails the operation closed. Choose a value above the slowest
statement the deployment is expected to serve.

`durable` is caller-declared. TiDB/TiKV replication, sync-log, backup, and
power-loss guarantees are deployment properties and cannot be inferred from a
connection URL. The provider's `flush` is an acknowledged TiDB round-trip
barrier; it does not change the cluster's durability configuration.

## Concurrent writable mounts

Opt-in `splitstore` + `concurrent_writes` uses revision CAS without a writer
lease. Each client connects to the same authoritative TiDB metadata scope and
shared immutable blocks. TiDB blocks, RustFS, R2, AWS S3 and PGlite blocks can
be selected independently; process-local memory and host-local SQLite blocks
cannot serve independent hosts using TiDB metadata.

A fresh empty metadata scope is bound to one stable block-authority ID in
MRC2 mode. Each publication checks the mode, backing ID and expected revision
atomically. Conflicting known noncommits return `EAGAIN`; an unknown commit
acknowledgement fails closed. Establishing MRC2 exhausts the legacy writer
fence so older lease clients cannot acquire or publish. Reopening established
metadata verifies the existing block marker without creating a replacement.
Additive schema setup upgrades the existing binary-key table with nullable
mode/binding columns; it does not convert a populated legacy namespace to
concurrent mode. Existing MRC1 volumes require the explicit offline
`migrate-concurrent-backing` command while all writers are stopped.

The [CLI example](../../apps/mount-rs-cli/examples/config-concurrent-tidb.json)
uses TiDB for both sides:

```sh
mount-rs mount --config /absolute/path/to/config-concurrent-tidb.json \
  --mountpoint /absolute/path/to/view-a \
  --also-mountpoint /absolute/path/to/view-b
```

For independent CLIs, start one command per mountpoint using the same metadata
and block scope. For TiDB metadata plus RustFS blocks, enable
`concurrent_writes` in the existing TiDB/RustFS example and use the same signed
RustFS bucket/prefix in every client. Both views accept writes. Conflicting
filesystem mutations refresh and replay against the latest namespace;
application-level distributed locking is not provided by the NFS views.

Concurrent online GC remains disabled until distributed open-handle pins and
a reclamation policy exist. Whole-namespace publication is still subject to
the configured 4 MiB limit; conflict blocks and tombstones can accumulate.

## TiDB limits and service requirements

The defaults cap blocks and serialized namespace JSON at 4 MiB. This keeps
normal writes below TiDB's documented default entry and packet limits. A
larger configured limit requires matching TiDB `txn-entry-size-limit`, TiKV
`raft-entry-max-size`, `max_allowed_packet`, and transaction-size settings;
the backend may reject rows or transactions that exceed the deployment's
actual limits. Volume keys and lease owners allow 255 Unicode characters and
are stored as up to 1020 raw UTF-8 bytes (`VARBINARY`), not character columns
whose collation could merge distinct identifiers. Trailing spaces remain
significant. Block IDs are 65 lowercase ASCII bytes. This unpublished schema
requires fresh provider tables; earlier draft character-column schemas are
not automatically migrated.

The opt-in integration test requires a real TiDB endpoint and a database user
with permission to create/alter the provider tables and read/write rows:

```text
MOUNT_RS_TIDB_URL='mysql://user:password@127.0.0.1:4000/test' \
  cargo test -p mount-rs-tidb --test tidb -- --ignored --nocapture
```

The test uses a process/time-unique volume key and leaves no provider-wide
cleanup operation. It exercises lease contention and expiry, fencing, CAS,
reconnect, immutable block reads/deletes, and flush barriers. A MySQL server or
mock-compatible substitute is not an acceptance result. A TLS-required
endpoint uses the crate's optional `rustls` feature:

```text
cargo test -p mount-rs-tidb --features rustls --test tidb -- --ignored --nocapture
```

With `require_ssl=true` the server certificate and its host name are
verified against the built-in public roots. A TiDB whose certificate comes
from a private authority adds that authority with the provider's
`tls_ca_file` URL parameter, which names a PEM file:

```text
mysql://user:password@tidb.internal:4000/mount?require_ssl=true&tls_ca_file=/etc/tidb/ca.pem
```

The parameter extends the trusted roots and leaves certificate and host name
verification on. It is rejected without `require_ssl=true`, and a path with
characters that are special in a URL query must be percent-encoded.

`scripts/test-tidb-tls-local.sh` performs the handshake for real with no
credentials: it creates a throwaway authority, starts a single-node TiDB
serving a certificate issued for `localhost`, runs the provider contract over
the verified session, and checks that the provider refuses an unrelated
authority, the public roots alone, the same server reached as `127.0.0.1`,
and, once `require_secure_transport` is on, a plaintext session. It ran
against TiDB v8.5.7 (TLS 1.3, `TLS_AES_128_GCM_SHA256`). It does not cover a
managed endpoint, certificate rotation or client certificates.

The explicit ambiguous-commit lane uses a small plaintext MySQL-wire proxy to
forward a real TiDB session and drop the successful publication response after
the server commits the conditional UPDATE or explicit transaction. It verifies
that the provider returns an unknown outcome
and does not replay the publication. Run it only against an actual TiDB URL:

```text
MOUNT_RS_TIDB_URL='mysql://user:password@127.0.0.1:4000/test' \
  cargo test -p mount-rs-tidb --test ambiguous_commit -- --ignored --nocapture
```

This failure-injection test requires an unencrypted `mysql://` endpoint because
the proxy must inspect the MySQL command stream. It is evidence for commit
outcome handling, not a claim that a lost response can reveal whether TiDB
committed; callers must reconcile the row before deciding whether to retry.

CI should start a pinned TiDB service (TiDB plus its required storage/PD
components, not MySQL), wait for port and SQL readiness, create a least-
privilege test database/user, set `MOUNT_RS_TIDB_URL` without printing it, and
run the ignored test. A separate failure-injection lane should interrupt the
client connection around statements and commit, then verify reconnect/CAS
reconciliation; such a lane is required before claiming ambiguous-commit and
crash-recovery acceptance. macOS and Linux compile lanes can use the same
remote/local TiDB endpoint; the native client is the MySQL protocol driver and
does not require a TiDB SDK.

With `MOUNT_RS_TIDB_TRACE=1` and `MOUNTX_SOURCE` naming the pinned mountx
checkout, `scripts/test-tidb.sh` also runs the seeded differential traces of
`scripts/check-trace-parity.mjs` against the `chunked-tidb` backend: TiDB
metadata and TiDB blocks under the chunked filesystem, compared operation by
operation with the mountx memory oracle for each seed. To run only that
comparison against an endpoint you own:

```text
MOUNT_RS_TIDB_URL='mysql://user:password@127.0.0.1:4000/test' \
MOUNTX_SOURCE=/absolute/path/to/mountx \
MOUNT_RS_TRACE_BACKENDS=chunked-tidb \
  node scripts/check-trace-parity.mjs
```

Each trace writes under its own volume key and leaves its rows behind.

The standard `scripts/test-tidb.sh` gate also runs the concurrent contracts
and four-coordinator load before and after component restarts. Its owned
cluster enables a global-autocommit regression, restores the original setting
before interpreting the result, and verifies publication/block bytes through
an independent reader. Direct external-endpoint tests skip that global-setting
case unless `MOUNT_RS_TIDB_GLOBAL_AUTOCOMMIT_TEST=1` is explicitly set for a
disposable server. Native macOS cases additionally require
`MOUNT_RS_CLI_NATIVE_NFS=1` and `MOUNT_RS_CLI_NATIVE_TIDB_TWO_PROCESS=1`;
TiDB/RustFS mounts also require
`MOUNT_RS_CLI_NATIVE_RUSTFS_DISPOSABLE=1` and the owned RustFS credentials.

`MOUNT_RS_TIDB_GC_LIFE_TIME_ONLY=1` runs only the test that provokes error
9006 for an autocommit statement and checks that the statement did not
commit and that the provider reports an unknown outcome. It lowers the
cluster's GLOBAL garbage-collection settings to their minimum while it runs
and takes about 22 minutes, because TiDB accepts no GC life time below ten
minutes, so it needs a TiDB nothing else is using. The `tidb-gc-life-time`
CI job runs it that way on a single-node cluster and prints
`TIDB_GC_LIFE_TIME_PASS`; the test fails, rather than skips, if the
statement ends any other way. (`MOUNT_RS_TIDB_GC_LIFE_TIME_TEST=1` adds it to
a full run instead.) The provider still reports 9006 as an unknown outcome:
the test shows one way TiDB raises it, before the statement commits, not
that it can never be raised later.

### Crash tests

`MOUNT_RS_TIDB_CRASH=1` adds a whole-cluster process crash to
`scripts/test-tidb.sh`. A writer
(`tests/crash_restart.rs`) takes the lease and publishes one file per
publication as fast as it can; the harness sends SIGKILL to every PD, TiKV
and TiDB container in one `docker kill`, starts them all again, and a second
process checks the restarted cluster: every acknowledged publication is
present, the one in flight is present exactly as written or absent (never
partly), the writer got an error that is not retryable and did not replay,
every block the metadata names reads back with its bytes, and the dead
writer's lease is refused once a new writer has taken over. In the durable
topology this runs after the ordered restarts and before the persisted phase,
so the persisted checks also run after it. A pass prints
`TIDB_CRASH_RESTART_PASS`. The `tidb` and `tidb-rustfs` jobs set it.

The `tidb-rustfs` job also runs
`apps/mount-rs-cli/tests/native_tidb_sqlite_crash.rs` (Linux FUSE only,
from `scripts/test-tidb-rustfs-consumers.sh`): SQLite commits transactions
on a TiDB-backed mount of the real `mount-rs` binary, the mount process is
killed with SIGKILL inside a transaction, the volume is mounted again once
the dead process's 30-second writer lease expires (a remount before that is
refused), and SQLite's `PRAGMA integrity_check` must pass with every
committed transaction present and the interrupted one all there or absent.
It runs with TiDB blocks and with RustFS blocks and prints
`TIDB_SQLITE_MOUNT_CRASH_PASS` for each.

Neither is a power-loss test. SIGKILL leaves the page cache and the disks
alone, so data a TiKV or PD had written but not fsynced survives it. Real
power loss and host fsync behaviour need hardware, or a block device that
drops unsynced writes, and are not tested.
