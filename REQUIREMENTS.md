# Delivery requirements

This file supplements the active porting goal and preserves subsequent user
requirements. `PORTING_STATUS.md` records evidence, not a reduction of scope.
Current workstreams and actionable tasks are tracked in [WORK_TRACKER.md](WORK_TRACKER.md).
User-selected architectural inspiration is tracked in [REFERENCES.md](REFERENCES.md),
including agentfs, Archil, and Tensorlake; mountx remains the compatibility oracle.

## Current completion gates

- Complete the Rust port of the pinned `pithings/mountx` source with behavioral
  parity tests, separate core/integration crates, minimal dependencies, and
  napi-rs Node bindings.
- Verify memfs, SQLite, Cloudflare R2, and real PGlite integrations. Local
  object-store tests do not replace authenticated live R2 verification.
- Implement and verify FoundationDB and TiDB backing stores as separate
  integration crates, including the end-to-end acceptance below.
- Verify supported macOS and Linux configurations, including native operations
  and the Node package. Commit and push validated work chunks to `origin/main`.
- Deliver native macOS FSKit support as an explicit required transport, not
  merely FUSE/NFS fallback or a future optional investigation; see below.
- **Safely host SQLite database files on mount-rs filesystems.** This is distinct
  from using SQLite as mount-rs's persistence backend. It is a required delivery
  gate, not established by backend tests or ordinary file round trips.
- Provide a **SQLite VFS for mount-free database access** and a **mount-independent
  filesystem API with just-bash and Mastra adapters**, as specified below.
- **Separate metadata storage from byte/block storage.** Drivers must compose
  independent stores, including configurations where metadata and bytes use
  different backing services. The current whole-filesystem JSON snapshot is
  transitional and does not satisfy this architecture requirement.
- **Implement fixed-size chunking initially behind an extensible interface.**
  Persist algorithm/version and parameter identification. Additional algorithms
  are future extensions; the user selected fixed-size for initial delivery.
- **Support versioned filesystems**, including stable historical views and
  explicit durable snapshots, as specified below. This is current scope.
- **Add and run aligned storage benchmarks.** Use the ComputeSDK storage suite
  below as the workload reference; record reproducible results alongside, not
  instead of, correctness and durability evidence.

### FoundationDB and TiDB backing-store acceptance

- Provide separate `mount-rs-foundationdb` and `mount-rs-tidb` integration
  crates with minimal justified dependencies; keep database clients out of
  core crates. Each exposes metadata and block stores that can be selected
  independently and composed with other providers.
- Preserve atomic metadata publication, compare-and-swap, fenced ownership,
  immutable blocks, and explicit durability barriers. Handle transaction
  conflicts, retries, cancellation, and ambiguous commit outcomes without
  duplicate effects or falsely acknowledging durability. Document and test
  provider key/value/transaction limits and chunk-size constraints.
- Expose provider selection through Rust, napi-rs Node factories, and the
  config-file CLI, with credential environment references and no secret output.
- Run shared filesystem contract, split-provider, concurrency/fencing,
  reconnect/restart, and failure-recovery integration tests against actual
  FoundationDB and actual TiDB. Emulators or MySQL-compatible substitutes alone
  do not prove these integrations. Include mounted SQLite-hosting tests and
  benchmark lanes with explicit remote-service prerequisites.
- Verify macOS and Linux client/build support, including FoundationDB native
  client requirements. Document reproducible local/CI service setup and
  distinguish missing services or credentials from passing tests.

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

## Compression evaluation

Compression evaluation is tracked in [the compression review](docs/compression-design.md).
Review independent per-chunk compression versus pre-chunk streaming, zstd and
alternative codecs, logical/physical identities, dictionary retention, provider
limits and random-I/O costs. Select defaults from reproducible macOS/Linux
benchmarks and preserve SQLite durability, version readability and separate
metadata/block-store composition. The review is not implementation acceptance.

## RustFS integration-test service

Set up actual [RustFS](https://rustfs.com/) for reproducible local and CI
integration tests. Pin the service release and container digest or binary
checksum; document macOS/Linux setup, readiness, isolated test credentials,
test-owned buckets/prefixes, persistent restart fixtures, and bounded cleanup.
Do not touch pre-existing user buckets or expose the test service publicly.

Exercise the S3-compatible block provider against RustFS with real requests:
immutable publication, conditional operations, full/range reads, errors,
concurrent access, reopen and service restart. Include split metadata/block
compositions, Node factories, CLI configuration, and mounted tests where
available. Wire the required service tests into CI; a mock or a skipped test
does not satisfy this requirement. RustFS does not replace live Cloudflare R2
verification: retain both service-specific acceptance gates.

Use the pinned RustFS service for mount-rs storage benchmarks where supported,
with isolated namespaces and recorded resource,
cache, transport, and durability settings. Record setup failures and unsupported
combinations instead of fabricating benchmark results.

## Artifact FS inspiration and benchmark comparison

Include [cloudflare/artifact-fs](https://github.com/cloudflare/artifact-fs)
as architecture inspiration and an additional feature/benchmark comparison
target; mountx remains the behavioral oracle.

- Pin a source revision and review lazy hydration, metadata availability before
  bytes arrive, prefetch priorities/concurrency, cache behavior, local overlays,
  generation publication, restart recovery, and macOS/Linux mount support.
  Record applicable lessons and explicitly distinguish verified features from
  proposed mount-rs work; adding a reference does not require a Git backend.
- Produce a source-backed feature matrix comparing Artifact FS and mount-rs.
  Mark unsupported or non-equivalent workloads rather than implying parity
  between a Git-backed filesystem and arbitrary storage engines.
- Benchmark time to usable tree, first-file read, cold/warm reads, selective
  versus full hydration, small-file metadata operations, and concurrent reads
  where both systems support equivalent workloads. Validate returned bytes.
- Pin builds and datasets; record mount protocol, hardware/OS, backend/network,
  cache/prefetch settings, downloaded bytes, memory, raw samples, latency and
  throughput. Separate preparation, mount readiness, and hydration costs;
  include end-to-end startup totals so deferred work is not hidden.
- Preserve the ComputeSDK benchmark requirements. Record actual
  results or explicit prerequisites/unsupported cases, never inferred wins.
  Inspect licensing before code reuse and retain minimal dependencies.

## Mount-free SQLite VFS

- Deliver a separate SQLite VFS integration crate backed by mount-rs storage
  engines. It must support environments where native filesystem mounting is
  unavailable, without secretly requiring FUSE, NFS, FSKit, or a host mount.
- Specify and implement SQLite's file lifecycle, random reads/writes, short-read
  zero filling, truncate, synchronization, locking, access/delete, and error
  contracts. Advertise only device characteristics actually guaranteed by the
  selected backend. Address SQLite's synchronous callbacks over async providers
  without runtime deadlocks or unsafe borrowed-buffer lifetimes.
- Explicitly define supported journal modes, writer/reader concurrency and
  process boundaries. WAL requires correct shared-memory and locking support;
  do not claim WAL support from a rollback-journal implementation.
- Test actual SQLite connections using this VFS against supported storage
  compositions: transactions, contention, reopen, integrity checks, abrupt
  interruption, failed barriers, partial writes and ambiguous publication.
  Compare SQL results with conventional SQLite and verify durable acknowledgements.
- Expose usable Rust and Node entry points with reproducible examples and
  platform/runtime support documentation. Keep this distinct from both SQLite
  as a storage backend and SQLite hosted on an OS-mounted filesystem.

## Mount-independent filesystem API and consumer adapters

- Expose the same logical drives through a usable API without an OS mount,
  including drives also served through native mounts. Reuse the same storage,
  namespace, permission/read-only policy, version view and concurrency rules;
  do not copy mounted contents into a separate in-memory filesystem.
- Provide documented Rust/Node access and separately packaged adapters for
  just-bash and Mastra virtual filesystems, based on their actual version-pinned
  contracts. Verify supported methods and report unsupported capabilities
  explicitly; do not substitute a lookalike interface for consumer integration.
- Run real consumer-level integration tests for reads/writes, directories,
  rename/delete, metadata, errors, path handling, read-only/pinned versions,
  provider persistence, and cancellation/cleanup. Test shared visibility between
  API access and an actual mount where the platform permits it.
- Define how external processes/systems access the API. In-process adapters
  must not silently imply a remote service exists; any network API needs an
  explicit transport, authentication, isolation, and error/versioning contract.
- Test in a mount-disabled environment and document backend prerequisites,
  synchronous/asynchronous interface limits, and browser/runtime restrictions.

## Multi-drive HTTP service and distributed-cache integration

- Deliver a separately packaged HTTP server over mount-rs that exposes multiple
  named logical drives from one service. Drives must work without OS mounting
  and may also be served through native mounts, using the same authoritative
  namespace, version views, permissions and independently configured stores.
- Provide config-driven drive registration and discovery, stable drive IDs,
  documented/versioned routes and errors, streamed/range file reads and writes,
  metadata and directory operations, cancellation, backpressure and bounded
  request sizes. Distinguish logical drive exposure from actual kernel mounts.
- Enforce per-drive authorization and namespace isolation: paths, caches,
  credentials and version references must never cross drive/tenant boundaries.
  Define TLS/authentication deployment requirements and fail closed on unknown
  drives or unsupported operations. Do not silently expose the service publicly.
- Support the distributed-cache layer described below across multiple HTTP
  server instances. Design its extension boundary now; implement the cache
  only after primary acceptance and the requested user design discussion.
  Key immutable bytes by drive/storage identity and immutable content/version;
  mutable namespace/head caches require an explicit coherence contract.
- Preserve authoritative durability/fencing: cached acknowledgements cannot
  substitute for fsync or committed metadata. Specify cache bypass, invalidation,
  eviction, restarts, partitions and stale-reader behavior before implementation.
- Test two or more drives with separate and shared provider configurations,
  mount-disabled clients, native/API shared visibility, pinned versions,
  authorization failures, traversal attempts, and concurrent writes. Then test
  multiple HTTP instances with the actual chosen distributed cache, including
  restart/outage/invalidation and cross-drive isolation, with measured cold/warm
  latency, backend requests and memory. A process-local cache is not acceptance
  evidence for distributed caching.

## Native macOS FSKit acceptance

- Implement an FSKit transport/integration separately from platform-neutral
  core crates. Share the existing filesystem and independently selectable
  metadata/block providers rather than introducing another filesystem engine.
- Document supported macOS versions/architectures, SDK and build requirements,
  extension packaging, signing/entitlements, installation, activation, and
  cleanup. Obtain any required user approval for host extension installation
  or activation; do not bypass macOS protection mechanisms.
- Expose explicit FSKit selection through the CLI/config and Node mount APIs,
  with capability detection and actionable unsupported-platform/setup errors.
  Never silently substitute FUSE or NFS when FSKit was explicitly requested.
- Verify actual FSKit-mounted I/O, metadata, namespace operations, concurrency,
  read-only behavior, shutdown/unmount, and recovery against the shared
  contract and relevant mountx behavior. Test persistent and mixed backends.
- Run actual SQLite-hosting transaction, locking, synchronization, and recovery
  tests on FSKit mounts; document journal-mode and platform limits honestly.
- Include version-pinned views when versioning is available, and mounted-path
  benchmark coverage. Unit tests, compilation, mocks, or another transport's
  green results do not satisfy native FSKit acceptance. If CI cannot activate
  the extension, record the remaining real-macOS verification gate explicitly.

## Versioned-filesystem acceptance

Review [TLFS](https://docs.tensorlake.ai/filesystems/introduction) as the design
reference, not a new behavioral oracle or an automatic dependency.

- Define durable version IDs, ordered history, immutable snapshots with optional
  labels, historical reads, and read-only mounts pinned to a selected version.
  Distinguish a pinned view from a read-only mount following the current head.
- Provide an explicit restore/fork operation from a retained version. Publishing
  a restored head must be atomic and concurrency-checked; never overwrite live
  writer state silently. Fork changes must not alter their source snapshot.
- Define checkpoint capture boundaries under concurrent writes, cancellation,
  provider failure, and restart. A returned durable snapshot must reference
  durable blocks and metadata; incomplete publication must not appear in history.
- Expose versioning through Rust, Node, and config-driven CLI operations. Test
  old and current bytes, namespace, metadata, sparse files, rename/delete,
  mixed providers, concurrent publication, and recovery on macOS and Linux.
- Specify retention, snapshot deletion, open-view pinning, and block reclamation
  before implementing cleanup. Never reclaim data reachable from a retained
  version, active view, or in-progress publication.
- Keep SQLite snapshots application-consistent using a documented quiescence or
  database-aware checkpoint/backup procedure. Arbitrary filesystem snapshots
  and last-writer-wins file merging do not establish safe SQLite backup/restore.
- Document volatile memfs limitations and each provider's persistence guarantees.
  Autosave, multi-writer merge policy, and remote replication timing require
  explicit design decisions; do not inherit TLFS policies without evaluation.

Versioning is required now. Efficient physical copy-on-write remains a future
implementation requirement: correct initial snapshots/forks may use copying,
but must disclose costs and cannot be advertised as copy-on-write.

## End-of-primary-work review and distributed cache follow-on

After completing and verifying the primary filesystem uses and integrations,
review the [Erlang gen_statem guide](https://www.erlang.org/doc/system/statem.html)
for additional lessons about storage coordination and communication. Record
source-backed findings and any proposed follow-up changes before final handoff.

Track a **distributed cache** as a feature to complete after the primary uses.
It must integrate with the multi-drive HTTP service above, not only direct
in-process driver calls.
When that phase is reached, discuss its scope and design with the user before
implementation. Do not silently choose a cache service, consistency model, or
topology, or start it while primary acceptance remains incomplete.

That discussion should settle metadata versus immutable-block caching,
coherence/invalidation and versioning, ownership/fencing, eviction and capacity,
failure/partition behavior, authentication, deployment/dependency cost, and
benchmarks. Distinguish disposable cached data from authoritative durable
storage; preserve SQLite locking, fsync, and recovery guarantees. The feature
is deferred pending that discussion, not implemented or implicitly waived.

## Launch follow-up: domain and marketing site

- Buy `mount-rs.com` through AWS domain registration, subject to availability.
  Before purchase, verify the intended AWS account, current registration and
  renewal prices, registrant details/privacy, and obtain explicit confirmation
  of the paid transaction. This checklist entry does not authorize a purchase.
- Build and deploy a mount-rs marketing site on Vercel, connect the domain,
  configure DNS and HTTPS, and verify the public site from the actual deployed
  URL. Confirm the Vercel team/project and AWS DNS ownership before changes.
- Base feature, platform, durability, and benchmark claims on verified release
  evidence. Clearly distinguish implemented, experimental, planned, and
  unsupported capabilities; do not market pending acceptance as complete.
- Keep this launch work tracked alongside engineering delivery. No domain has
  been registered and no marketing deployment is implied by this requirement.

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
