# Driver metric export

Extend the latest native diagnostic contract and Node phase consumer to expose
the fixed direct SDK and TiDB recorder rows. Preserve the separate raw blob API
schema, exact decimal counters, default-disabled configuration and unchanged
performance floor.

1. Record a behavioral Node RED for missing returned-row and per-row pending
   accounting before changing the consumer.
2. Declare closed operation families and their distinct call/byte meanings.
   Provider, lock, transaction and SQL spans overlap; combined counts or elapsed
   time are not application IOPS or CPU time. Unknown bytes/rows remain unknown.
3. Export checked returned-row deltas and before/after per-row gauges. Reject
   missing/reset/inconsistent counters and preserve bounded partial evidence.
4. Test exact Rust JSON through the Node consumer after serialized native gates.
   Retain source hashes, commands, outcomes and unmeasured coverage.

Historical evidence and schemas stay bound to their original sources. TiDB pool
checkout includes connection/session creation; isolated queue wait, internal
HTTP/TiKV requests and physical flash IOPS remain unavailable.

## Closed current contract

Native schema v3 exports all 78 ordered names from the core recorder, per-row
in-flight gauges and exact decimal returned-row/observation counters. The
separate raw object-store schema stays v1. No earlier-version compatibility or
throughput-floor change is introduced.

The declared families distinguish NAPI providers, direct SDK providers, PGlite
client-lock acquisition, TiDB checkout, session configuration, open setup,
transaction lifecycle and categorized SQL adapter invocations. Their spans can
nest and overlap. Family call counts and elapsed time are not application IOPS,
internal request counts or CPU time. Known block payload bytes are measured at
selected boundaries; metadata, lock, transaction and unmeasured SQL payload
bytes remain unavailable. Stored zero bytes cannot establish absence of payload.

`storage_operations` declares the closed rows; `storage_instrumented_operations`
separately declares source-audited instrumentation. The TiDB portion comes from
the producer's explicit 18-label public coverage list, preserving core order.
`measurement.tidb_coverage` exports that public constant's static decimal site
counts, fixed scopes and unavailable list. Its `source_sites_instrumented` status
describes source sites, not dynamic calls or tested server performance. Known SQL
payload bytes cover successful block INSERT input and block-body SELECT output;
other SQL payload bytes are unavailable. Pool checkout duration includes lazy
connection and session configuration; isolated queue wait remains unavailable.

Known returned SQL Option/Vec counts are separate from affected rows and from
payload bytes. A successful known zero-row result increments observations;
ordinary `finish_success` leaves zero observations and an unknown row quantity.
Non-SQL families must not claim row observations. Node validates both cumulative
endpoints and additive phase deltas, including observation count no greater than
successful calls and no returned rows without an observation. Per-row and global
gauges must both be quiescent for complete phase evidence.

Invalid snapshots retain bounded fixed-label observations, including all 78
rows and whitelisted v3 metadata. Summaries separate the families and preserve
elapsed time and returned-row observations without turning unavailable banks
into observed zero counters.

## Execution record and remaining gates

- Root's first Node behavioral RED demonstrated missing exact row deltas;
  original RED/GREEN receipts remain immutable in the driver export packet.
- The actual native Rust serializer RED compiled and exited 101 at the v2 versus
  v3 schema assertion. Its fresh binary hash was captured before overwrite.
  The broad workspace source inventory detected concurrent changes only in two
  disjoint service paths; the native/core/SDK/TiDB source hashes stayed exact.
- Additional fixture-only Node REDs caught missing family retention, valid
  cumulative endpoints producing invalid row-observation deltas, and a block
  put falsely claiming SQL rows. The precision fixture now uses a SQL category.
- Eight focused Node suites and the full explicit JavaScript capture-mock suite
  pass on the current source. They do not load a native addon or query a backend.
- A further fixture-only RED rejected the final audited TiDB metadata and
  checkout availability contract. Its GREEN passes all eight focused suites,
  including malformed coverage/site-count rejection and sanitized metadata.
  TiDB's public coverage handoff declares all 18 production labels as source
  instrumented. Its separate provider unit/strict Clippy gates passed; live TiDB
  behavior remains unverified.
- Native v3 source implementation follows the actual RED. The isolated enabled
  serializer GREEN passes and its exact JSON reaches Node with all 78 rows and
  78 source-covered names. The Node control reuses that retained JSON at both
  endpoints; it does not execute a workload or query a backend.
- Existing isolated enabled dynamic-provider memory and R2 construction-only
  controls pass, along with strict NAPI all-targets Clippy. All four Rust command
  receipts bind the same unchanged 405-file source/configuration footprint and
  native source hash. The fresh executed test binary hash is retained.
- Cargo/native leases are released. Touched formatting and the final source
  freeze bind this implementation for independent review.

Instrumentation and serializer controls do not prove a live TiDB/R2 query,
full-capacity qualification, storage durability, cache policy or physical IOPS.
Independent SPEC/QUALITY review of the final frozen sources remains required.
