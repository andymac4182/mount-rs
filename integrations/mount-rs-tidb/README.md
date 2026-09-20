# mount-rs-tidb

TiDB-backed implementations of the core `MetadataStore` and `BlockStore`
contracts. The providers are independent: metadata and immutable bytes may be
connected with separate `TidbStorageOptions` values and composed with other
mount-rs providers.

## Transaction contract

- Metadata lease acquisition, renewal, release, and publication select TiDB's
  pessimistic transaction mode before an explicit transaction, then use a
  volume-row `SELECT ... FOR UPDATE`. TiDB Cloud services that expose
  `tidb_txn_mode` as read-only must retain pessimistic mode. The provider checks
  the effective session value before every transaction and rejects optimistic,
  missing, or unknown modes rather than assuming a deployment default.
- TiDB's provider clock is read inside the transaction. A lease is valid only
  when owner, fence, exact expiry, and provider-clock expiry all match.
- Publication checks the lease and expected revision while holding the row
  lock, then updates the namespace and revision in the same transaction.
  Revision mismatch is `EAGAIN`; stale or expired fencing is `ESTALE`.
- Lock/deadlock conflicts known before commit map to `EAGAIN` and the caller
  may retry the complete operation. There is no automatic transaction replay.
  A `COMMIT` error is returned as an I/O error with an unknown outcome and is
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
with permission to create the two provider tables and read/write rows:

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

CI should start a pinned TiDB service (TiDB plus its required storage/PD
components, not MySQL), wait for port and SQL readiness, create a least-
privilege test database/user, set `MOUNT_RS_TIDB_URL` without printing it, and
run the ignored test. A separate failure-injection lane should interrupt the
client connection around statements and commit, then verify reconnect/CAS
reconciliation; such a lane is required before claiming ambiguous-commit and
crash-recovery acceptance. macOS and Linux compile lanes can use the same
remote/local TiDB endpoint; the native client is the MySQL protocol driver and
does not require a TiDB SDK.
