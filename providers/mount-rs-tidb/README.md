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
