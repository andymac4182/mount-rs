# PGlite engine and socket baseline

This diagnostic runs the stock engine and stock socket adapter. It does not change
the production provider or serialize all provider connections behind a new gate.

```sh
PGLITE_MODULE_ROOT=/path/to/tests/pglite/node_modules \
PGLITE_BASELINE_DATA_DIR=/fresh/owned/pglite-directory \
PGLITE_BASELINE_ITERATIONS=1000 \
PGLITE_BASELINE_CLIENTS=10 \
node providers/mount-rs-pglite/diagnostics/direct-baseline.mjs > /tmp/pglite-baseline.json
```

Use a fresh owned directory. The script preserves it for inspection. Omitting the
data directory measures an in-memory engine and reports that identity explicitly.
Begin with 5 iterations to check the harness before recording a timed baseline.
The default describes the statement, matching `tokio-postgres::query_typed`.
`PGLITE_BASELINE_DESCRIBE_STATEMENT=0` selects the portal as a counterexample.
`PGLITE_BASELINE_FRAGMENTED_PROBE=1` adds a 128 KiB binary parameter to the
multi-client probes to exercise partial TCP frame delivery.
`PGLITE_BASELINE_FRAGMENT_BYTES=458752` selects a 448 KiB payload, and
`PGLITE_BASELINE_PARAMETER_COUNTS=2,3,7` varies the integer parameter counts.
Client count is physical socket connections, unlike remote SDK clients multiplexed
onto a smaller number of provider connections. Preserve that topology difference
when interpreting these counterexamples.

Each direct engine operation issues one SQL statement, except the explicit
transaction operation, which issues BEGIN, UPDATE, SELECT, and COMMIT. Read
operations validate the returned 4 KiB payload length. Socket upload operations
send actual binary bytea payloads; socket reads return text bytea, so 4 KiB becomes
8194 response value bytes. This encoding differs from the Rust provider's binary
response decoding and should be accounted for when comparing wire bytes.

Socket counters include Parse, Bind, Describe, Execute, Close, Sync, startup,
Terminate, responses, errors, and bytes. Engine protocol counters independently
count calls and bytes reaching `execProtocolRawStream`. A protocol frame is not an
SQL statement or disk I/O. Startup and cleanup counters are included in each
connection's totals. Direct stage SQL statement counts are attempted counts, not
internal engine query planner or page accesses.

The multi-client diagnostic varies parameter counts across clients. It records
SQL errors and unexpected result values, then repeats with unique statement
names. This is a diagnostic counterexample: portal and transaction state remain
shared by the stock adapter, and named statements are not proposed as a complete
production repair. These phases are correctness probes rather than throughput
measurements.

At each quiescent stage boundary, an independent autocommit statement calls
`pg_stat_force_next_flush()`, then `pg_stat_clear_snapshot()`, then one statement
reads database, I/O, WAL, table row/page, and tracking counters. These three SQL
statements run outside the timer. The flush call bypasses normal PostgreSQL
counter batching; clearing discards cached statistics. The helper reports their
availability and retains raw snapshots. It checks that observed commits minus
the three observer transactions equal the successful workload operations. An
empty control measures the same sampling work without a workload.

Table/TOAST/index page deltas describe only the benchmark relation and exclude
catalog pages accessed by sampling queries. Database-wide and `pg_stat_io`
deltas retain observer work explicitly; the empty control is reported separately
rather than silently subtracting its page accesses. Buffer hits, page reads,
and WAL writes are backend counters, not host physical IOPS. Counter view
availability alone does not prove correct updates; a failed commit reconciliation
or unavailable flush function must remain visible in the report.

Counter flush behavior is documented in [PostgreSQL statistics documentation](https://www.postgresql.org/docs/18/monitoring-stats.html)
and implemented in [pgstat.c](https://github.com/postgres/postgres/blob/REL_18_STABLE/src/backend/utils/activity/pgstat.c).
