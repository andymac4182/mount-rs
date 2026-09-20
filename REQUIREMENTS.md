# Delivery requirements

This file supplements the active porting goal and preserves subsequent user
requirements. `PORTING_STATUS.md` records evidence, not a reduction of scope.
User-selected architectural inspiration is tracked in [REFERENCES.md](REFERENCES.md),
including agentfs, Archil, and Tensorlake; mountx remains the compatibility oracle.

## Current completion gates

- Complete the Rust port of the pinned `pithings/mountx` source with behavioral
  parity tests, separate core/integration crates, minimal dependencies, and
  napi-rs Node bindings.
- Verify memfs, SQLite, Cloudflare R2, and real PGlite integrations. Local
  object-store tests do not replace authenticated live R2 verification.
- Verify supported macOS and Linux configurations, including native operations
  and the Node package. Commit and push validated work chunks to `origin/main`.
- **Safely host SQLite database files on mount-rs filesystems.** This is distinct
  from using SQLite as mount-rs's persistence backend. It is a required delivery
  gate, not established by backend tests or ordinary file round trips.
- **Separate metadata storage from byte/block storage.** Drivers must compose
  independent stores, including configurations where metadata and bytes use
  different backing services. The current whole-filesystem JSON snapshot is
  transitional and does not satisfy this architecture requirement.
- **Implement fixed-size chunking initially behind an extensible interface.**
  Persist algorithm/version and parameter identification. Additional algorithms
  are future extensions; the user selected fixed-size for initial delivery.
- **Add and run aligned storage benchmarks.** Use the ComputeSDK storage suite
  below as the workload reference; record reproducible results alongside, not
  instead of, correctness and durability evidence.

### Storage benchmark acceptance

Align with [computesdk/benchmarks storage](https://github.com/computesdk/benchmarks/tree/master/benchmarks/storage).
The inspected reference revision is `92fbbc9ba7739111899121195236acb4fc6a8bb5`.
Pin that revision in the benchmark mapping and explicitly document any later
updates or workload deviations.

- Match the upload/write → full-byte download/read → delete lifecycle at
  1, 4, 10, and 16 MiB. Include sequential concurrency-one runs and configurable
  iterations/concurrency. Keep initialization outside the timed operations.
- Report raw samples, upload/download milliseconds, download throughput in
  decimal Mbps, median/p95/p99, failures, timeouts, success rate, and cleanup
  failures. Do not count missing credentials or failed operations as successes.
- Compare equivalent mountx and mount-rs workloads, including the Node binding,
  across memfs, SQLite, real PGlite, and live R2-backed split storage. Identify
  metadata/block providers separately. Label direct API versus mounted-path
  measurements; they are not interchangeable.
- Record revision, platform/architecture, runtime versions, chunk size,
  payload sizes, iterations, concurrency, cache state, synchronization policy,
  and remote region/network context. Separate volatile and durable results;
  do not silently remove barriers to improve timings.
- Validate returned bytes outside the timed download and clean up only objects
  created by the run. Include a short CI smoke mode and a repeatable full-run
  command with machine-readable results. Do not invent a performance threshold
  or claim superiority from unmatched environments.
- Track the reference's snapshot/fork benchmarks with the future copy-on-write
  requirement. Do not emulate snapshots with full copies and present that as
  implemented copy-on-write performance.

The direct-API runner is implemented in `benchmarks/storage`. Local memory,
SQLite, split SQLite, and pinned TypeScript memory workloads have been exercised.
This does not close the full benchmark requirement: remote backends, platform
coverage, mounted-path workloads, and the separate dispatch review still need
their own evidence.

### Dispatch and dependency performance acceptance

Before completion, review `async-trait`, handle erasure, and dynamic dispatch
across `FsDriver`, `FileHandle`, `MetadataStore`, and `BlockStore` on Rust 1.95.
Prefer native async / `impl Future + Send` on generic paths where practical;
retain explicit dynamic adapters where runtime backend selection requires them.
Preserve independent metadata/block composition, extensibility, and Node APIs.
Removing a macro while retaining equivalent boxing is dependency cleanup, not
evidence of a performance improvement.

- Audit NAPI `DynMetadataStore`, `DynBlockStore`, and `MountDriver` forwarding
  for redundant future boxing, Arc cloning, and path/key/byte copies. Remove
  only costs that can safely be avoided within the required lifetimes.
- Establish reproducible before/after small-operation baselines: memfs reads
  and writes, stat/open/close, small blocks, chunk-size scaling, concurrency,
  latency/throughput, and allocations where feasible. Distinguish direct
  generic Rust, erased Rust, and Node overhead. These supplement, not replace,
  the ComputeSDK-aligned large-file benchmarks.
- Label volatile, cached, durable SQLite, PGlite, and live R2 measurements
  separately. Preserve synchronization guarantees; report unavailable backend
  evidence honestly and never infer percentage gains without measurements.
- After changes, rerun parity, mixed-store, cancellation/lifecycle,
  SQLite-hosting, and macOS/Linux gates. Document the final dispatch design,
  direct dependency footprint, remaining dynamic boundaries, reasons for any
  retained `async-trait`, and measured performance/build-size/time trade-offs.

### Metadata, block storage, and chunking acceptance

- Keep metadata-store, block-store, chunking, and filesystem orchestration
  responsibilities explicit. Provider integrations remain separate crates;
  core contracts must not import a cloud SDK or a database implementation.
- Metadata represents namespace/inodes, attributes, links, file lengths,
  ordered chunk/extents, and revisions. Bytes reside in independently selected
  block storage, not in metadata snapshots disguised as a second interface.
- Exercise mixed configurations, including in-memory stores, SQLite/PGlite
  metadata with remote R2 blocks, and local alternatives that run without
  credentials. Report tested combinations rather than assuming all pairings.
- Define atomic publication across the two stores: referenced blocks must be
  available and durable to the promised level before metadata acknowledges a
  commit. Failed uploads, metadata conflicts, retries, crashes, and abandoned
  blocks must have explicit recovery and reclamation semantics.
- Preserve file/handle behavior across chunk boundaries: arbitrary offset
  reads/writes, append, partial overwrite, truncate/grow, sparse/zero-filled
  ranges, concurrent handles, links, and reopen. Test boundary sizes and
  randomized operations against the TypeScript oracle.
- Persist chunker version and parameters so existing files remain readable
  when defaults change. Verify determinism, bounded chunk sizes, reassembly,
  empty files, and rules for future algorithm switching/migration. Selecting an algorithm
  must not silently reinterpret existing data.
- Run SQLite-hosting acceptance through this split-store architecture as well
  as memory. Backend snapshot tests from the transitional implementation do
  not prove the final architecture's transaction or durability guarantees.

This requirement does not by itself bring user-visible copy-on-write snapshots
or clones into current scope; those remain the separate future requirement.

### SQLite hosting acceptance

Run the real SQLite engine against paths inside actual mounts, with evidence
for every supported platform/transport/backend combination. Record the tested
SQLite version, journal mode, synchronous setting, and storage configuration.

- Transaction commit, rollback, reopen, schema changes, and binary data must
  preserve contents; `PRAGMA integrity_check` must return `ok` after each
  recovery scenario.
- Multiple connections and separate processes must exercise readers, competing
  writers, lock contention, busy errors/timeouts, and connection termination.
  Verify filesystem lock semantics; do not substitute an in-process test mutex.
- Test rollback-journal and WAL modes, including journal/WAL/SHM lifecycle,
  checkpointing, file extension/truncation, and any required shared mappings.
  A mode that cannot be supported safely must fail explicitly and remain a
  documented acceptance gap, rather than silently changing modes.
- Verify synchronization ordering and durability under the declared storage
  guarantees: committed data survives filesystem-service restart and backend
  reopen; interrupted writes, failed synchronization, and process crashes do
  not produce silent corruption or falsely acknowledged durable commits.
- Separate SQLite-process failure from mount-service failure and backend
  failure. Test recovery with fault injection and abrupt termination, not only
  graceful closes. Do not equate process-restart tests with power-loss proof.
- Define and enforce the writer/ownership model for persistent and remote
  backends. Concurrent mounts must not silently corrupt one database; any
  single-mount restriction must be enforced and clearly reported. Snapshot CAS
  alone is not proof of SQLite-compatible locking or transaction durability.
- Memfs must preserve transaction/locking correctness while explicitly reporting
  its volatile nature. Do not claim crash-persistent storage for memfs.

Until these gates have evidence, SQLite hosting is **not verified safe**.

## Config-file CLI deliverable and integration acceptance

Provide a usable CLI output that mounts from a documented, versioned config
file, while retaining existing command-line use. Cover transport, mountpoint,
read-only and SQLite single-host policy, driver selection, independent metadata
and block providers, and fixed-size chunk parameters. Keep integration-specific
dependencies out of core crates.

- Document the config schema, relative-path resolution, defaults, and explicit
  CLI override precedence. Reject unknown fields and invalid combinations before
  mounting or modifying persistent stores.
- Support credential environment references; never print resolved secrets in
  errors, diagnostics, or example configs.
- Provide runnable examples for local memory, SQLite, PGlite, and R2-backed
  compositions, identifying required external configuration honestly.
- Test the built CLI with real config files, including invalid config and
  override cases. Add opt-in native integration coverage that starts the CLI,
  mounts, performs filesystem I/O, verifies configured provider behavior, then
  stops and verifies unmount/cleanup on macOS and Linux. Parser-only tests do
  not satisfy this mount integration requirement.

## Future requirement: copy-on-write

Track copy-on-write as future work, not a current feature or a requirement to
implement during this port unless the user explicitly brings it into scope.

The future design must define snapshot/clone semantics, isolation between views,
atomic publication, crash consistency, synchronization with live writers,
shared-data reclamation, and backend support. It must preserve the filesystem
contract and SQLite-hosting guarantees. Snapshot serialization or full-snapshot
conditional replacement is not itself copy-on-write support. The API and storage
granularity remain design decisions; no implementation or compatibility claim
is made yet.
