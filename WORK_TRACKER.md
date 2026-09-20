# Workstream and task tracker

Updated: 2026-09-20. Baseline: `2452463`, plus explicitly identified uncommitted
work below. Overall status: **in progress; not release-ready**.

This is the delivery dashboard. [Requirements](REQUIREMENTS.md) define scope;
[porting evidence](PORTING_STATUS.md) and the [API parity ledger](docs/public-api-parity.md)
retain detailed results. A passing component test is not end-to-end acceptance.

## How to read and maintain this tracker

- **Landed:** committed implementation, not necessarily full acceptance.
- **Implementing:** active, uncommitted work; not delivered.
- **Verifying:** implementation exists, acceptance evidence is incomplete.
- **Planned:** required but not yet assigned or implemented.
- **Deferred:** deliberately sequenced later, not forgotten or completed.
- Checked tasks describe only the precise scope stated. Unchecked tasks remain open.
- Main maintains this file after handoffs, validated commits, failures and new
  findings. Workers report task IDs, changed files, commands/results and blockers
  to main rather than concurrently editing it. Split newly discovered work into
  named tasks below. Keep failed, skipped and unavailable evidence visible.
- Commit and push validated chunks to `origin/main`; do not call a workstream
  complete until its integration and platform acceptance gates pass.

## Workstream dashboard

| ID | Stream | Status | Current owner |
| --- | --- | --- | --- |
| W01 | Core and mountx parity | Verifying | Main |
| W02 | Metadata/block split and chunking | Verifying | Main |
| W03 | Memory and SQLite stores | Landed; extending | Jason |
| W04 | PGlite | Verifying | Copernicus / Main |
| W05 | Cloudflare R2 | Credentials verified; integration pending | Main |
| W06 | RustFS integration service | Landed; extending | Hooke |
| W07 | FoundationDB | Implementing | Hilbert |
| W08 | TiDB | Implementing | Arendt |
| W09 | Node / napi-rs and public API | Verifying | Main / Jason |
| W10 | FUSE, NFS, 9P, WebDAV, S3 | Landed; verifying | Main |
| W11 | Config-driven CLI | Landed; extending | Chandrasekhar |
| W12 | Safely hosting SQLite files | Partial evidence | Main |
| W13 | macOS FSKit | Implementing | James |
| W14 | Versioned filesystems | Implementing | Jason |
| W15 | Mount-free SQLite VFS | Implementing | Hume |
| W16 | just-bash / Mastra adapters | Landed locally; hosted verification pending | Confucius / Main |
| W17 | Multi-drive HTTP server | Implementing | Copernicus |
| W18 | Benchmarks and dependency budget | Partial implementation | Main |
| W19 | Compression | Design review recorded | Main |
| W20 | CI, packaging and final acceptance | Verifying | Main |
| W21 | Reference review and learnings | Ongoing | Main |
| W22 | Distributed caching | Deferred for discussion | User / Main |
| W23 | Physical copy-on-write | Future requirement | Unassigned |
| W24 | Domain and marketing site | Planned; approval needed | User / Main |
| W25 | Actual AWS S3 integration | AWS MCP access needed | Main |
| W26 | Apache Ozone S3 backend | Planned; unverified | Unassigned |

## Decisions and external prerequisites

- [x] **D01 — Live R2 credentials:** created bucket-scoped object read/write
  credentials for `mount-rs-integration-tests`, stored in macOS Keychain, expiring
  2026-09-27. Read-only S3 listing passed. Full integration acceptance remains
  open; credentials are not committed and RustFS evidence is not a substitute.
- [x] **D02 — License:** user selected Apache-2.0 for mount-rs on 2026-09-20.
  First-party package declarations use Apache-2.0; preserve third-party notices,
  including MIT attribution for upstream-derived portions.
- [ ] **D03 — FSKit:** obtain signing/install/activation authorization and host
  prerequisites when executable extension testing is ready.
- [ ] **D04 — Distributed cache:** discuss topology, consistency and service
  choice with the user after the primary implementation is accepted.
- [ ] **D05 — Launch:** confirm AWS account, domain cost/renewal and Vercel
  deployment/account choices before purchase or production changes.
- [ ] **D06 — Publication:** obtain authorization for package publication;
  permission to commit and push is not permission to publish packages.

## W01 — Core and mountx behavioral parity

- [x] Land Rust filesystem contract and implementations, with separate crates.
- [x] Pin mountx oracle to `85361a8212ff9bff8e69f62fa8993ef2c2ec51e8`.
- [x] Latest local upstream-suite stage: 1,194 passed, 88 skipped; not full parity
  acceptance and not evidence that skipped behavior works.
- [ ] W01.1 Complete every applicable export/behavior in the API parity ledger.
- [ ] W01.2 Classify all upstream skips and add coverage for required behavior.
- [ ] W01.3 Finish seeded cross-engine traces and retain reproducible failures,
  seeds and revision-matched results for every required backend.
- [ ] W01.4 Verify errors, paths, binary data, links, timestamps, handles,
  concurrency and lifecycle on supported macOS and Linux configurations.

## W02 — Independent metadata, blocks and chunking

- [x] Land metadata/block contracts, fixed-size chunking, immutable blocks,
  fenced publication and ordered durability barriers.
- [x] Exercise local mixed-provider compositions.
- [ ] W02.1 Complete live mixed-provider matrix, including Node and CLI paths.
- [ ] W02.2 Verify stale writers, CAS conflicts, partial uploads, retry ambiguity,
  crash/reopen and provider capability failures across remote combinations.
- [ ] W02.3 Document format/version migration and chunk-size compatibility.
- [ ] W02.4 Define safe orphan cleanup and garbage collection before enabling it.
- [ ] W02.5 Preserve an extensible chunker interface. Fixed-size is initial scope;
  additional algorithms need explicit implementation and compatibility tests.

## W03 — Memory and SQLite persistence

- [x] Land memory and SQLite filesystem, metadata and block integrations.
- [x] Local provider run passed five memory and four SQLite library tests.
- [x] Local draft SQLite schema reopening fix uses explicit INSERT columns;
  this and additive versioning changes are **not yet committed**.
- [ ] W03.1 Review and land additive version storage without regressing old stores.
- [ ] W03.2 Test migrations, concurrent open, reopen, transaction rollback and
  uncertain commit behavior with revision-matched evidence.
- [ ] W03.3 Keep SQLite backend durability distinct from SQLite-hosting safety.

## W04 — PGlite

- [x] Land real socket-server integration and provider/factory coverage.
- [x] Fix transaction cleanup before releasing a connection slot (`29ffb3b`).
- [x] Add injected cleanup-failure regression (`cfce82a`): failed cleanup retains
  ownership and rejects unsafe reconnect rather than reusing the slot.
- [x] W04.1 Full `scripts/test-pglite.sh` run completed with exit 0. Passed stages
  include slot cleanup, injected failure, provider parity, reconnect, fencing,
  cancellation, disk restart, mixed stores, Node factories and userspace FUSE.
  Upstream suite passed with skips; all eight trace lanes passed five seeds of
  621 operations each. This local run does not replace hosted/live-R2 evidence.
- [ ] W04.2 Confirm hosted macOS/Linux reruns close the previous reconnect failure.
  Run `35493696795`, job `106032856390`, still failed bounded close/reopen with
  a server communication error. Copernicus owns the handshake-race investigation;
  the newer local pass does not close this intermittent hosted failure.
- [ ] W04.3 Integrate versioning, mount-free VFS and native SQLite-hosting tests.

## W05 — Cloudflare R2

- [x] Land object-store/R2 driver code and configurable endpoint support.
- [x] W05.1 Unblock D01 and run authenticated tests against actual Cloudflare R2.
  On 2026-09-20, main passed the explicit live filesystem contract test and
  SQLite-metadata/R2-chunk roundtrip/reopen test (two tests, no skips). Credentials
  remain in Keychain; this is local dirty-worktree evidence, not release acceptance.
- [ ] W05.2 Verify immutable writes, ranges, retries, reconnect, cleanup and
  concurrent publication with independently selected metadata providers.
- [ ] W05.3 Run Node, CLI/native, parity and benchmark lanes on live R2.
- [ ] W05.4 Record service identity and revision without recording credentials.
- [x] W05.5 Fix live Node factory expected-byte assertion and guarantee unique
  cloud fixture keys with exact cleanup. Main reran the full PGlite/R2 script
  successfully: actual R2 Node factory and DELETE/HEAD cleanup, independent
  PGlite metadata + R2 blocks, provider lifecycle/restart/fencing, Node chunked
  factories and userspace FUSE. Upstream: 1,194 passed, 88 skipped; eight seeded
  lanes × five seeds × 621 operations passed. The object-store trace lanes are
  local, not live R2 traces. This is local dirty-worktree evidence, not hosted
  platform or full release acceptance.

## W06 — RustFS integration service

- [x] Land isolated real-service harness and Linux CI job (`f5759cc`).
- [x] Main verified actual pinned RustFS contract tests, concurrent CAS, ranges,
  restart/fresh reads and guarded container/data cleanup; baseline run passed.
- [ ] W06.1 Review and land current split SQLite/PGlite metadata, Node factory
  and CLI extensions. These are active uncommitted changes.
- [ ] W06.2 Verify the hosted RustFS CI job, not just its configuration.
  Hosted runs `35493800880` / `35493696795` passed block/restart tests but
  failed cleanup of container-owned `.rustfs.sys` bind-mount files with permission
  denied. Hooke owns validated ownership-aware cleanup and a Linux regression.
- [ ] W06.3 Add fault and benchmark workloads with reproducible service settings.
- [ ] W06.4 Provide isolated RustFS service orchestration for W07.6 and W08.5;
  test actual composed filesystems rather than unrelated backend smoke tests.

## W07 — FoundationDB

- [ ] W07.1 Review and register the separate draft integration crate.
  Worker feature-enabled check/Clippy passed; this is not live-service evidence.
- [ ] W07.2 Finish isolated real FoundationDB client/server harness. Current
  native execution was blocked by missing `fdb_c`; do not replace with mocks.
- [ ] W07.3 Resolve production lease/time semantics: default unsupported clock
  behavior and a development clock do not establish safe distributed fencing.
- [ ] W07.4 Validate transaction/block limits, CAS, stale writers, durability,
  restart and backend identity against the real service.
- [ ] W07.5 Add Node, CLI, native-mount and macOS/Linux acceptance coverage.
- [ ] W07.6 **FoundationDB metadata + RustFS S3 chunks:** Hilbert owns real-service
  integration, multi-chunk/boundary/partial-write/truncate round trips, namespace
  persistence, fresh-client/service restart, CAS and stale-writer assertions.
  Coordinate RustFS lifecycle with Hooke; no emulated acceptance.

## W08 — TiDB

- [x] Local draft library tests passed three cases: limits, stable identifiers,
  and URL redaction. Crate/workspace registration remains uncommitted.
- [ ] W08.1 Review and land TiDB metadata/block implementation and dependencies.
- [ ] W08.2 Complete real TiDB/PD/TiKV Docker harness and restart results;
  MySQL compatibility alone does not constitute TiDB verification.
- [ ] W08.3 Verify provider time/fencing, ambiguous commits, concurrency and
  deployment durability assumptions. Liveness queries are not fsync evidence.
- [ ] W08.4 Add Node, CLI, native-mount and macOS/Linux acceptance coverage.
- [ ] W08.5 **TiDB metadata + RustFS S3 chunks:** Arendt owns real-service
  integration with the same mixed-provider acceptance as W07.6, using actual
  TiDB/PD/TiKV and RustFS. Add runnable isolated orchestration and retain results.

## W09 — napi-rs, Node API and packaging

- [x] Land JavaScript driver bridge, codec/server hooks and 9P ESM exports.
- [x] Fix shutdown test's early rejection handling (`8d1f4ad`); 25 strict repeats
  passed locally. This corrects the test race, not a proven runtime defect.
- [x] Add server-test phase/cleanup diagnostics (`2452463`); 20 strict main
  repeats passed. Earlier intermittent timeout cause is not established.
- [x] Full local Node suite passed after fixes, excluding opt-in service/native
  lanes; opt-in skips are not acceptance evidence.
- [ ] W09.1 Close remaining public exports, factories and declaration parity gaps.
- [ ] W09.2 Investigate any repeated server timeout using the new diagnostics.
- [ ] W09.3 Expose new providers, versioning, VFS and HTTP through tested APIs.
- [ ] W09.4 Verify platform artifacts, package install and native loading on both
  operating systems; keep package publication behind D06.

## W10 — Filesystem and protocol transports

- [x] Land FUSE, NFS, 9P, WebDAV and S3 transport implementations and tests.
- [x] Land HTTP early-rejection regression (`aa44413`).
- [x] Land incremental S3 chunked codec helpers (`e218d18`); all 27 S3 tests and
  strict Clippy passed locally.
- [ ] W10.1 Finish per-transport backend/platform acceptance matrix, including
  native lifecycle, disconnect/error behavior and streaming/backpressure.
- [ ] W10.2 Verify transport auto-selection and explicit unsupported behavior.
- [ ] W10.3 Ensure all protocol paths operate on the same configured drive state.

## W11 — Config-driven CLI

- [x] Land strict configuration and CLI (`f20f1e3`).
- [x] Land actual macOS NFS host-backed mount/IO/unmount lifecycle and CI gate
  (`c4058f2`); main native test and strict Clippy passed.
- [x] W11.1 Review SQLite-backed native lifecycle and UID/GID config; main's
  33 unit + seven integration tests and formatting passed. Landing with this update.
- [x] W11.2 Fix review finding: explicit ownership mismatch on read-only config
  must not mutate persisted root ownership.
- [x] W11.3 Fix review finding: an existing `0:0` root is not proof of a new
  filesystem. Preserve existing ownership; avoid check-then-create races.
- [x] W11.4 Main ran actual macOS config→NFS mount→SQLite workload→unmount→
  fresh-process reopen: PASS, SQLite 3.51.0, DELETE journal, synchronous FULL.
  Process locking/recovery passed; mount-service crash was not tested here.
- [ ] W11.5 Extend configuration and lifecycle coverage to all new providers,
  version views, FSKit and multi-drive HTTP without silently falling back.

## W12 — Safely host SQLite database files

- [x] Record prior Linux FUSE DELETE/WAL and macOS NFS single-host DELETE
  evidence; those configurations alone do not establish universal safety.
- [ ] W12.1 Define and test supported journal/locking modes per transport.
- [ ] W12.2 Prevent silent WAL fallback from being reported as WAL success.
- [ ] W12.3 Exercise multiple connections/processes, readers/writers, locks,
  sync barriers, rename/unlink, disk-full errors and crash/restart integrity.
- [ ] W12.4 Run integrity checks and acknowledged-commit recovery across every
  supported metadata/block combination and operating system.
- [ ] W12.5 Explicitly reject unsupported safety modes and document constraints.
- [ ] W12.6 Run the new Linux CLI SQLite-backed FUSE SIGKILL/reopen test in
  hosted CI. Implementation and bounded cleanup are added; main independently
  passed both shared macOS NFS lifecycle regressions, formatting and Clippy.
  Linux execution is not yet verified on this macOS host.

## W13 — FSKit

- [x] Land unsigned SDK compile target (`5993984`); main arm64 compile passed,
  worker reported x64 compile. Neither is a mounted-filesystem result.
- [ ] W13.1 Implement FSVolume operations/read-write and tested Swift/Rust IPC.
- [ ] W13.2 Verify errors, handles, identity, concurrency and lifecycle at the seam.
- [ ] W13.3 Complete packaging, entitlements and signing plan, then request D03.
- [ ] W13.4 Activate and test real FSKit mounts, CLI integration, persistence and
  supported SQLite workloads. NFS/FUSE fallback does not satisfy this stream.

## W14 — Versioned filesystems

- [x] Record versioning design (`dc6b0bb`).
- [x] Local uncommitted prototype passed four tests covering history, restore,
  fork, pins/retention, SQLite reopen and operation replay after head moves.
- [ ] W14.1 Review and land version IDs, publication, pinned views, history,
  restore/fork, retention and explicit provider capabilities.
- [ ] W14.2 Fix pin lifetime: an open historical handle must retain protection or
  be invalidated when its view closes; do not allow reads after protection lapses.
- [ ] W14.3 Validate expiry before/after reads and during slow in-flight reads;
  protect immutable public records and test close/read/delete races.
  Worker checkpoint reports pin-lifetime fixes, seven versioning tests and
  strict Clippy passing; main review and independent verification remain open.
- [ ] W14.4 Verify replay payload matching, CAS conflicts, cross-store forks,
  crash/reopen, block reachability and safe retention/GC boundaries.
- [ ] W14.5 Add real remote-provider, Node and CLI coverage; clearly label
  unsupported capabilities and database-quiescence requirements.
- [ ] W14.6 Keep full-copy snapshots distinct from future physical COW (W23).

## W15 — Mount-free SQLite VFS

- [ ] W15.1 Complete separate draft crate and actual mount-rs storage bridge;
  host-file reference implementation alone does not satisfy the requirement.
- [ ] W15.2 Fix registration lifetime escapes through connection extraction or
  mutable access; test duplicate names and independently opened connections.
- [ ] W15.3 Replace noop-waker/busy polling with a valid executor/reactor contract
  or explicit unsupported rejection; test genuinely asynchronous storage.
- [ ] W15.4 Implement real cross-connection SQLite locking or explicit safe
  serialization, with fencing. Local lock enums alone are insufficient.
- [ ] W15.5 Test stale-reader promotion, separate processes, lost updates,
  short reads, sync failures, journal recovery and database integrity.
- [ ] W15.6 Expose and integrate through Rust and Node across storage engines
  without requiring a native mount; document supported journal modes.

## W16 — Mount-independent consumer adapters

- [x] W16.1 Implement separate just-bash and Mastra adapters over mount-rs drives.
- [x] W16.2 Main independently passed actual just-bash 3.4.2 and Mastra 1.67.0
  tests, strict TypeScript checking and package dry-run (11 intended files).
  Memory and SQLite reopen/shared Node API visibility are covered. Wired into
  test-all and four-platform Node CI; hosted results remain pending.
- [ ] W16.3 Verify binary/path/error behavior, readonly/versioned views, shared
  namespace visibility, persistence and lifecycle in consumer integration tests.
- [ ] W16.4 Reuse the drive abstraction with W17 without requiring OS mounts.
- [x] W16.5 Fix review findings before landing: reject recursive copy into a
  descendant (including symlink aliases), preserve source on same-object copy,
  and handle partial/zero-progress writes. Add targeted regressions.
- [x] W16.6 Document that waiting for admitted operations does not establish
  bounded cleanup if a backend request never resolves.
- [ ] W16.7 Add explicit backend cancellation/timeout semantics and remote-store,
  versioned-view and actual native-mount shared-visibility acceptance.

## W17 — Multi-drive HTTP server

- [ ] W17.1 Implement a separately packaged server and drive registry/config;
  expose several named drives, including unmounted drives.
- [ ] W17.2 Define discovery, routing, filesystem operations, streaming/ranges,
  stable errors and lifecycle; share the actual native/API drive namespace.
- [ ] W17.3 Add per-drive authorization/isolation, limits and deployment/TLS guidance.
- [ ] W17.4 Test multiple drives and mixed stores through real HTTP clients,
  including concurrency, restart, failures and Node/CLI configuration.
- [ ] W17.5 Define a cache integration boundary, but do not implement/select the
  distributed cache until the W22 discussion and primary acceptance.

## W18 — Benchmarks, allocations and dependencies

- [x] Land canonical dispatch benchmark (`2aecccf`) and forwarding allocation
  cleanup (`a84faf7`); cleanup is not evidence of a measured performance win.
- [ ] W18.1 Align workloads with ComputeSDK storage benchmarks, including
  representative 1/4/10/16 MiB data sizes and small-file/metadata workloads.
- [ ] W18.2 Measure direct Rust, Node and native paths across required stores;
  report cold/warm behavior, latency, throughput, memory and service settings.
- [ ] W18.3 Produce controlled before/after dispatch/allocation results.
- [ ] W18.4 Add license-reviewed ArtifactFS comparisons and explain semantic
  differences rather than presenting incomparable throughput as equivalent.
- [ ] W18.5 Audit dependency count/features per crate; justify runtime/codec/SDK
  additions and keep integrations out of the core dependency surface.
- [ ] W18.6 Review the untracked duplicate dispatch draft against the canonical
  benchmark before any cleanup; preserve unrelated working-tree files.

## W19 — Compression evaluation

- [x] Record [compression design review](docs/compression-design.md) (`642e81a`):
  initial recommendation is chunk first, then independently compress blocks
  through an optional wrapper. This is a proposal, not implemented compression.
- [ ] W19.1 Benchmark raw, zstd levels 1/3/6, LZ4 and justified alternatives over
  representative data and chunk sizes; compare pre/post-chunking layouts.
- [ ] W19.2 Measure ratio, CPU, memory, random access, rewrites, deduplication,
  network cost and effects on future chunkers/version diffs.
- [ ] W19.3 Specify codec/version metadata, dictionaries, corruption handling,
  decompression bounds and compatibility before choosing implementation scope.

## W20 — CI, packaging and final acceptance

- [ ] W20.1 Obtain revision-matched green required hosted macOS/Linux jobs.
  Latest jobs were queued/in progress at this update; earlier Node/PGlite
  failures are not closed by local fixes alone.
- [ ] W20.2 Verify new RustFS and CLI native gates actually execute and pass.
- [ ] W20.3 Add real FoundationDB/TiDB, VFS, versioning, adapters and HTTP gates.
- [ ] W20.4 Run locked build/tests, formatting and strict Clippy on final changes;
  retain logs and clearly distinguish ignored, skipped and credential-gated lanes.
- [ ] W20.5 Validate install/package artifacts, license consistency and dependency
  inventory; document supported and explicitly unsupported configurations.
- [ ] W20.6 Audit every task and requirement against code plus integration
  evidence before marking the goal complete. Do not claim “100%” from test count.

## W21 — Reference review and learnings

Canonical source links and license considerations live in [REFERENCES.md](REFERENCES.md).
For each review, record applicable lessons, rejected ideas and resulting task IDs;
listing a source does not mean it has been reviewed or its code can be reused.

- [ ] W21.1 Close agentfs and Archil architecture/storage review.
- [ ] W21.2 Close Tensorlake repository/TLFS and changed-byte Firecracker snapshot
  review; feed version/diff implications into W14/W23.
- [ ] W21.3 Review relevant CrabBuild repositories for blob-backed versioning.
- [ ] W21.4 Review SlateDB for S3 storage, publication and recovery tradeoffs.
- [ ] W21.5 Review SQLite's VFS abstraction for logical/storage separation.
- [ ] W21.6 Review Cloudflare ArtifactFS for feature and benchmark comparisons.
- [ ] W21.7 Review Erlang gen_statem at the end of primary work for storage and
  communication lifecycle lessons; create follow-up tasks for applicable findings.
- [ ] W21.8 Verify license compatibility before incorporating any reference code.

## W22 — Distributed cache: discuss after primary work

- [ ] W22.1 Hold D04 discussion before implementation: topology, service choice,
  consistency, invalidation, failure behavior, tenancy and cost expectations.
- [ ] W22.2 Implement the agreed cache across multiple HTTP server instances and
  drives; a single-process cache is not distributed-cache acceptance.
- [ ] W22.3 Test stale reads, concurrent writes, eviction, partitions, recovery,
  version-pinned reads and cross-drive isolation; benchmark against uncached IO.

## W23 — Future physical copy-on-write

- [ ] W23.1 Define immutable sharing, changed-byte/block granularity, reference
  lifetime, writable forks and reclamation using W14 and reviewed sources.
- [ ] W23.2 Agree implementation sequencing after primary requirements; then
  implement and test crash-safe sharing, isolation and changed-byte cost scaling.

## W24 — Domain and marketing site

- [ ] W24.1 After D05, buy `mount-rs.com` through AWS with agreed renewal settings.
- [ ] W24.2 Build and deploy Vercel marketing site using verified capability claims.
- [ ] W24.3 Verify public DNS, HTTPS and actual deployment, and record ownership/
  operational handoff. This backlog entry does not authorize spending now.

## W25 — Actual AWS S3 integration

- [ ] W25.1 Connect AWS MCP (not exposed in current tool inventory) and resolve
  account/region; user requested an isolated S3 testing setup.
- [ ] W25.2 Provision private test bucket, narrowly scoped access, and test-data
  cleanup/retention policy. Keep credentials outside chat and source control.
- [ ] W25.3 Execute actual AWS S3 block and composed-filesystem integration
  tests with restart/reopen, ranges, conditional immutable writes and cleanup.
  AWS S3 evidence does not replace Cloudflare R2 or RustFS acceptance.

## W26 — Apache Ozone S3 backend

- [ ] W26.1 Pin an Apache Ozone release and container digests; provide isolated
  local/CI orchestration, readiness, authentication and bounded cleanup on
  macOS/Linux. Record service topology, replication and durability settings.
- [ ] W26.2 Run the immutable block contract through its actual S3 gateway:
  conditional publication, concurrent writers, full/range reads, missing objects,
  binary multi-chunk content, restart/reopen and injected failures. Verify
  conditional semantics explicitly; never emulate away unsupported guarantees.
- [ ] W26.3 Test Ozone blocks with independent SQLite, PGlite, TiDB and
  FoundationDB metadata through ChunkedFs, including partial writes/truncation,
  revision CAS and stale-writer fencing.
- [ ] W26.4 Cover Node factories and CLI configuration; add required CI gates
  and document verified versions, limitations and platform evidence. Ozone is
  requested support, not yet a verified supported backend.

## Recent landed chunks

| Commit | Scope | Evidence boundary |
| --- | --- | --- |
| `2452463` | Server-test phase/cleanup diagnostics | Local repeats; timeout cause unresolved |
| `2085b19` | HTTP/cache requirements and SQLite VFS reference | Requirements, not server implementation |
| `cfce82a` | PGlite injected cleanup failure | Local regression passed |
| `8d1f4ad` | JS driver shutdown test ordering | 25 strict local repeats |
| `e218d18` | S3 incremental codec helpers | 27 tests and Clippy locally |
| `f5759cc` | Actual RustFS harness and CI wiring | Local baseline passed; hosted pending |
| `642e81a` | Compression review | Design, not codec implementation |
| `c4058f2` | CLI macOS NFS native lifecycle | Host-backed native lane passed |
| `29ffb3b` | PGlite cleanup/slot ordering | Local regressions; hosted rerun pending |
| `5993984` | FSKit SDK compile target | Unsigned compilation, not activation |
