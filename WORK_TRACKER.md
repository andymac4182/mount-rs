# Workstream and task tracker

Updated: 2026-09-21. Baseline: `3042d09`, plus explicitly identified uncommitted
work below. Overall status: **in progress; not release-ready**.

This is the delivery dashboard. [Requirements](REQUIREMENTS.md) define scope;
[porting evidence](PORTING_STATUS.md) and the [API parity ledger](docs/public-api-parity.md)
retain detailed results. A passing component test is not end-to-end acceptance.

Current local acceptance: on 2026-09-20, `scripts/test-all.sh` exited 0 at
`73c33e0` with the pinned mountx checkout and live, bucket-scoped Cloudflare R2
credentials held outside the repository. The run passed the complete Rust and
Node suites, memfs/SQLite/PGlite parity, live PGlite lifecycle and split-store
gates, authenticated R2 contract and five-seed R2 differential traces, the
configuration-driven R2 CLI (including both metadata-provider compositions),
HTTP/remote reopen, and R2 cleanup. Native privileged mounts, FSKit signing and
hosted Windows/macOS CI remain separate evidence boundaries.

Latest hosted evidence: [CI run 35499717435](https://github.com/andymac4182/mount-rs/actions/runs/35499717435)
at `37e9ba1` passed RustFS, Ozone, Linux Rust, Linux x64/arm64 Node,
Windows Node, and Linux native FUSE/NFS/9P/WebDAV jobs. Windows Rust failed
the HostFs lexical-root and directory-open tests; macOS jobs remain queued.
This evidence predates `3f52b44` and does not qualify that newer revision.
Its separate fault-injection workflow also passed (run `35499717417`).

Windows HostFs follow-up: directory handles now use backup semantics, raw
Win32 error mappings are separate from platform libc errno values, and root
tests cover drive/UNC paths. Main passed 14 macOS tests and strict Clippy;
post-fix Windows runtime CI is still required. Directory behavior was checked
against [libuv's Windows implementation](https://github.com/libuv/libuv/blob/v1.x/src/win/fs.c).
Run `35500474806` reached a different failure first: SQLite fencing setup
exhausted a 75 ms lease before publication. The test now uses long-lived setup
leases and explicitly expires the persisted lease before takeover, retaining
busy-owner, stale-writer, and winner-persistence assertions. Five local chunked
concurrency tests passed; Windows runtime confirmation remains required.
At `83bca86`, Windows job `106052286866` passed those fencing and HostFs unit
tests, then failed five HostFs integration tests: unsupported symlinks,
read-only create flags, and hard-link metadata. Lagrange is implementing these
without skipping the tests. Windows Node job `106052286914` passed.

Main ran `cargo test --workspace --all-targets --all-features --locked --offline`
against an isolated committed `cf3c485` snapshot on macOS: exit 0. Explicitly
ignored remote/native gates are not acceptance evidence from that run.
Main also reran the full oracle-enabled Node package suite after `753df86`:
exit 0, including harness/structural drivers, server protocols, 44 typed 9P
cases, memory parity, distribution and artifact aggregation. PGlite/R2 factory
and native-mount opt-ins were skipped in this run and remain separate gates.

Current-cycle evidence on 2026-09-21 adds a mount-free W01 concurrency packet,
an isolated provider/consumer matrix, and an eight-cell SQLite reliability
matrix. The dedicated PGlite lifecycle gate passed the Rust SDK rows for memfs,
memory/memory, SQLite/SQLite and PGlite/PGlite; the Node SDK rows for memfs,
SQLite, chunked-memory, chunked-SQLite and chunked-PGlite; and the five CLI
config/runtime rows. R2 rows remained explicit skips because this shell did not
have the credential variables. The post-fix full gate also passed the Rust and
Node suites, five-seed traces across memory/SQLite/object-store/chunked/PGlite
backends, and the N-API distribution checks. The full root gate exited 0 with
1,194 upstream tests passed and 40/40 seeded trace lanes passed; native mounts,
hosted Windows/Linux runs and live R2 remain separate acceptance gates.

Focused current-head acceptance after the follow-up packets: `mount-rs-sdk`
unit tests passed (2/2), the Rust provider matrix passed memfs, memory/memory
and SQLite/SQLite (3/3, with PGlite/R2 explicit skips), the Node SDK CLI direct
driver self-test passed, the complete N-API suite passed, chunked tests passed
(12/12 plus 7/7 concurrency), SQLite VFS tests passed (4/4 unit, 16/16 engine
and 16/16 bridge), and local HostFs tests passed (14/14 including the Windows
oracle cases where runnable). These are focused local gates at `3042d09`; the
current shell still lacks live R2/PGlite credentials and hosted Windows/macOS
and privileged native-mount runs remain unqualified.

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

### Delegated execution and rotation

`Main` in the dashboard means the coordinator owns cross-stream integration,
acceptance evidence, tracker updates and commit/push; it does not mean that
every implementation task is being done serially. Workers receive disjoint
write scopes, return exact paths and test evidence, and are closed after their
patch is integrated. A completed worker is then rotated into the next open,
non-overlapping packet. The current bounded allocation is:

| Worker | Packet | Write scope | Handoff state |
| --- | --- | --- | --- |
| Peirce | W12/W15 SQLite VFS and WAL/reliability seam | `integrations/mount-rs-sqlite-vfs/**`, related VFS plan | Integrated |
| Mill | W08 TiDB provider and RustFS composition harness | `integrations/mount-rs-tidb/**`, `tests/tidb/**`, TiDB harness | Integrated |
| Aristotle | W13 macOS FSKit seam | `integrations/mount-rs-fskit/**` | Integrated checkpoint |
| Meitner | W24 TanStack Start marketing/docs site | `apps/site/**` | Child task complete; custom domain live |
| Ohm | W18.6 storage-dispatch draft review | `benchmarks/storage/dispatch/**` | Closed; no change recommended |

Closed packets already integrated this cycle include Mendel (FUSE), Lagrange
(Windows host), Epicurus (CLI), Maxwell (FoundationDB), Newton/Astra (R2
design review), Raman (scoped napi-rs package distribution), Aristotle (the
unsigned FSKit bridge checkpoint), Mill (the TiDB provider/harness checkpoint),
Peirce (the SQLite VFS/WAL checkpoint), Russell (CLI/HTTP edge coverage),
Aquinas (Windows CI parity), Cicero (parity audit), and Ohm (storage-dispatch
review). Main
rotates those slots rather than assigning multiple workers to the same files.

### Narrow-band completion order

To reduce half-finished breadth, new worker packets are not opened outside the
current vertical slice until its acceptance gate is green:

1. **P0 runtime path:** core mountx parity, independent metadata/block stores,
   fixed-size chunking, and the shared contract through memfs, SQLite, PGlite
   and authenticated Cloudflare R2.
2. **P0 consumer path:** the same configured drives through napi-rs, the
   config-file CLI and the multi-drive HTTP API, with reopen, range, partial
   write, truncate, concurrency, fault-injection and cleanup evidence.
3. **P0 reliability/platform path:** SQLite hosting/VFS/WAL matrix and
   macOS/Linux qualification, while Windows CI remains a required parallel
   signal and is never treated as passed from Unix evidence.
4. **P1 integration path:** TiDB and FoundationDB metadata with RustFS blocks,
   then FSKit and remaining native acceptance. Existing bounded workers may
   finish their current packets, but they must not grow scope.
5. **P2 evidence/product path:** versioning, benchmarks/compression, AWS/Ozone,
   reference reviews, site/domain and publication hardening.

Distributed cache (W22), physical copy-on-write (W23), lifecycle hooks (W29)
and OTel (W30) remain deferred discussions/features until the P0/P1 gates are
complete.

## Workstream dashboard

| ID | Stream | Status | Current owner |
| --- | --- | --- | --- |
| W01 | Core and mountx parity | Active simple-first; core harness, pinned trace evidence and skip inventory landed; full parity remains open | Main (packets integrated) |
| W02 | Metadata/block split and chunking | Verifying; persisted chunker metadata and partial-write/reopen gates landed | Main |
| W03 | Memory and SQLite stores | Landed; extending | Main |
| W04 | PGlite | Verifying | Main |
| W05 | Cloudflare R2 | Live provider and configuration-driven CLI gates passed; broader benchmark/release evidence remains | Main |
| W06 | RustFS integration service | Landed; extending | Lagrange (complete slice) / Main |
| W07 | FoundationDB | Provider/composition passed; standalone crate committed, root registration pending | Maxwell (complete slice) / Main |
| W08 | TiDB | Crate and single-node harness landed; durable topology capacity-gated; RustFS composition pending | Mill (checkpoint) / Main |
| W09 | Node / napi-rs and public API | Verifying; public Rust SDK, Rust-backed FUSE state, and Node SDK CLI landed; platform/package gaps remain | Main (packets integrated) |
| W10 | FUSE, NFS, 9P, WebDAV, S3 | FUSE codec subpath landed; native and cross-platform transport acceptance remains open | Main (packet integrated) |
| W11 | Config-driven CLI | Rust CLI now consumes the Rust SDK; Node CLI consumes the Node SDK; provider/remote/hosted gates remain | Main |
| W12 | Safely hosting SQLite files | Local journal/WAL matrix, fail-closed bridge, and recovery gates landed; hosted/Windows gates pending | Peirce (checkpoint) / Main |
| W13 | macOS FSKit | Unsigned bridge checkpoint passed; activation/signing pending | Aristotle (checkpoint) / Main |
| W14 | Versioned filesystems | Local foundation landed; integration pending | Main |
| W15 | Mount-free SQLite VFS | Rollback, process-local and host-local WAL gates landed; remote/Node/Windows acceptance pending | Peirce (checkpoint) / Main |
| W16 | just-bash / Mastra adapters | Landed locally; hosted verification pending | Main |
| W17 | Multi-drive HTTP server | Server/CLI and local edge-case coverage landed; RustFS remote passed, Cloudflare acceptance pending | Russell (complete slice) / Main |
| W18 | Benchmarks and dependency budget | Partial implementation | Ohm / Main |
| W19 | Compression | Design review recorded | Main |
| W20 | CI, packaging and final acceptance | Verifying | Main |
| W21 | Reference review and learnings | Ongoing | Main |
| W22 | Distributed caching | Deferred for discussion | User / Main |
| W23 | Physical copy-on-write | Future requirement | Unassigned |
| W24 | Domain and marketing site | TanStack Start site deployed; `mount-rs.com` and `www.mount-rs.com` live on Vercel | Meitner (complete slice) / Main |
| W25 | Actual AWS S3 integration | Private test bucket verified; Rust tests pending | Main |
| W26 | Apache Ozone S3 backend | Local block/restart gate passed; mixed stores pending | Main |
| W27 | Native Windows support and CI | HostFs Windows packet landed; hosted runtime qualification pending | Main |
| W28 | Deterministic fault injection | Implementing | Main integration |
| W29 | User-configurable lifecycle hooks | Deferred for later | Unassigned |
| W30 | OpenTelemetry traces, metrics and logs | Deferred for later | Unassigned |
| W31 | Per-drive mounts from one backing datastore | Deferred for future design | Unassigned |

## Decisions and external prerequisites

- After the app restart, the nine prior worker handles were missing. Their
  checkout edits were preserved. Seven replacement Luna Max workers were
  started and verified running; current ownership is in the dashboard above.
  Historical worker names in individual evidence entries identify earlier
  work, not live sessions. Main owns integration, root manifests, and commits.

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
- [x] **D05 — Launch:** user authorized the existing Vercel Hobby plan and the
  `mount-rs.com` launch. Route 53 and Vercel access are verified; no duplicate
  purchase, paid upgrade or additional paid resources were used.
- [x] **D06 — Rust crate publication authorization:** user authorized publication
  from CI on `main` when ready. npm publication must use `@mount-rs`, with secure
  release controls for both registries. Verify namespace ownership, OIDC trust,
  protected main-only release jobs, provenance/package integrity and readiness
  before publishing. This authorization is not release-readiness evidence.

## W01 — Core and mountx behavioral parity

Current priority is simple-to-complex execution. Close the mount-free
in-memory Rust/Node/oracle contract first, then add persistence, remote
providers, transports, and native mounts only as separate later gates. A
passing remote/provider or native test cannot close a simpler parity gap, and
the W01 stream must remain open until its skipped behavior is classified and
the applicable oracle-backed cases are covered.

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
- [x] W01.5 Add a bounded deterministic concurrency packet: six scenarios,
  five explicit unsupported classifications, zero oracle mismatches, and five
  consecutive pinned-oracle runs. Cross-process/crash, exact append ordering,
  cancellation/close races, durability/restart and transport/native concurrency
  remain open by classification.

Evidence landed without closing the remaining W01 acceptance gates:

- [x] `0de1832` adds a mount-free core parity harness: 56 operations, 35
  successful results, 21 stable expected errors and zero mismatches against the
  pinned oracle. It intentionally does not claim concurrency, persistence,
  providers, transports or native mounts.
- [x] `ae2f12d` adds the complete 88-row upstream skip inventory and a
  revision-checked trace-evidence runner. Five pinned seeds passed on the
  memory/SQLite/object-store/chunked six-backend lane, and the dedicated
  PGlite lifecycle extended the same five-seed, 621-operation trace to
  `pglite` and `chunked-pglite`; live R2 remains credential-gated.
- [x] `dd65770` adds the Rust-backed N-API FUSE codec subpath and declarations;
  it is mount-free protocol coverage, not native FUSE session acceptance.
- [x] `0d7f1f4` proves the Rust CLI's real macOS NFS mount path with independent
  Rust and Node filesystem clients; `0dca1d1` adds a separate Node SDK CLI and
  its opt-in real macOS NFS self-test. Linux and provider-backed SDK matrices
  remain unverified here.
- [x] The isolated provider/consumer matrix exercises Rust SDK, Node SDK and
  CLI consumers with machine-readable PASS/SKIP/FAIL output. Local Rust rows
  are 3/3, local Node rows are 4/4, and CLI config/runtime rows are 5/5; the
  PGlite lifecycle adds Rust and Node PGlite rows. R2 rows remain explicit
  skips without credentials and do not count as live-provider acceptance.
- [x] `29337f7` makes the Rust CLI construct all local/provider drivers through
  the public `mount-rs-sdk` facade and adds a public Node CLI SDK self-test. The
  Rust SDK example, Rust CLI, Node CLI and provider matrix now exercise the same
  SDK contract; native mount and live remote-provider acceptance remain separate.

## W02 — Independent metadata, blocks and chunking

- [x] Land metadata/block contracts, fixed-size chunking, immutable blocks,
  fenced publication and ordered durability barriers.
- [x] Exercise local mixed-provider compositions.
- [x] `d6b80f4` persists chunker configuration/version metadata and covers partial
  writes, truncation, close/reopen and stale publication in the chunked store;
  the focused chunked suite passed 12/12 plus 7/7 concurrency cases locally.
- [ ] W02.1 Complete live mixed-provider matrix, including Node and CLI paths.
- [ ] W02.2 Verify stale writers, CAS conflicts, partial uploads, retry ambiguity,
  crash/reopen and provider capability failures across remote combinations.
- [ ] W02.3 Document format/version migration and chunk-size compatibility.
- [ ] W02.4 Define safe orphan cleanup and garbage collection before enabling it.
- [ ] W02.5 Preserve an extensible chunker interface. Fixed-size is initial scope;
  additional algorithms need explicit implementation and compatibility tests.

## W03 — Memory and SQLite persistence

- [x] The standalone SQLite reliability packet passes 8/8 deterministic cells
  across DELETE/WAL persistence reopen, supported single-writer contention,
  ENOSPC block-put injection, post-publish unknown-commit recovery, FULL
  synchronization and `integrity_check`.
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
- [x] The bounded test-server teardown race is covered by an exact PostgreSQL
  Terminate-frame cleanup path plus an I/O-turn barrier. The readiness slot test
  passed 10/10 and bounded close/reopen passed 5/5; the full PGlite and root
  gates then passed without the prior `Eio` reconnect failure.
- [ ] W04.2 Confirm hosted macOS/Linux reruns close the previous reconnect failure.
  Run `35493696795`, job `106032856390`, still failed bounded close/reopen with
  a server communication error. Copernicus owns the handshake-race investigation;
  the newer local pass does not close this intermittent hosted failure.
  Main's latest local regression run passed both Node cleanup suites and both
  Rust bounded/shared-close tests after graceful half-close cleanup was added.
  Listener restoration preserves duplicate regular and once registrations;
  success and injected-detach-failure regressions cover that review finding.
  Hosted macOS/Linux confirmation remains open.
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
  Main executed actual R2 differential traces with seeds 4182, 1, 42, 65535,
  and 4294967295: 621 operations each, all 3,105 matched the pinned TypeScript
  oracle, with per-run snapshot cleanup. On 2026-09-20, the live N-API R2
  factory passed exact-key DELETE/HEAD cleanup, the configuration-driven HTTP
  CLI passed both split-store drives and owned-prefix cleanup, and the
  ComputeSDK-aligned smoke benchmark passed a 1 MiB fixed-64 KiB chunked
  PGlite/R2 run (write 2,902.11 ms, read 1,657.28 ms, 5.06 MiB/s, delete
  8.51 ms; one iteration, zero timeouts/failures). Native/hosted lanes and the
  full benchmark matrix remain open. The full `scripts/test-all.sh` rerun at
  `73c33e0` also passed the live R2 lane end-to-end; this does not close the
  native/hosted portions of this task.
- [ ] W05.4 Record service identity and revision without recording credentials.
- [x] W05.6 Run the configuration-driven CLI gate against the canonical Cloudflare
  R2 endpoint with scoped S3 credentials. On 2026-09-20, the live gate passed
  both PGlite-metadata/R2-block and SQLite-metadata/R2-block drives, ranged
  reads, auth isolation, graceful reopen, object-presence checks and owned
  prefix cleanup. The runner now counts returned `Contents` because R2 may
  omit AWS's optional `KeyCount`; it does not weaken the cleanup gate.
- [x] W05.5 Fix live Node factory expected-byte assertion and guarantee unique
  cloud fixture keys with exact cleanup. Main reran the full PGlite/R2 script
  successfully: actual R2 Node factory and DELETE/HEAD cleanup, independent
  PGlite metadata + R2 blocks, provider lifecycle/restart/fencing, Node chunked
  factories and userspace FUSE. Upstream: 1,194 passed, 88 skipped; eight seeded
  lanes × five seeds × 621 operations passed. The object-store trace lanes are
  local, not live R2 traces. The subsequent full acceptance rerun passed with
  live R2 and PGlite enabled, while native privileged mounts and hosted CI remain
  separate evidence boundaries.

## W06 — RustFS integration service

- [x] Land isolated real-service harness and Linux CI job (`f5759cc`).
- [x] Main verified actual pinned RustFS contract tests, concurrent CAS, ranges,
  restart/fresh reads and guarded container/data cleanup; baseline run passed.
- [x] W06.1 Land real split SQLite/PGlite metadata and Node-factory gates,
  bounded Docker/process-group cleanup, and isolated combo orchestration.
  Main reran both timeout regressions and the full real-service harness after
  the final script changes: exit 0, block/split-provider/Node/restart gates
  passed. CLI integration beyond existing configuration tests remains open.
- [x] W06.2 Verify the hosted RustFS CI job, not just its configuration.
  Hosted runs `35493800880` / `35493696795` passed block/restart tests but
  failed cleanup of container-owned `.rustfs.sys` bind-mount files with permission
  denied. Ownership-validated cleanup is implemented and locally passed;
  hosted Linux confirmation passed at `37e9ba1`, job `106049192404`.
- [ ] W06.3 Add fault and benchmark workloads with reproducible service settings.
- [ ] W06.4 Provide isolated RustFS service orchestration for W07.6 and W08.5;
  test actual composed filesystems rather than unrelated backend smoke tests.

## W07 — FoundationDB

- [x] W07.1 Review the separate FoundationDB integration crate and commit its
  provider/test harness. It remains a standalone package until target-gated root
  registration is safe for Windows all-features CI.
- [x] W07.2 Finish isolated real FoundationDB client/server harness. Main ran
  the real pinned 7.4.7 Linux ARM64 server/client in Docker; the provider
  contract passed. The container supplies `fdb_c`; no host install or mock.
- [ ] W07.3 Resolve production lease/time semantics: default unsupported clock
  behavior and a development clock do not establish safe distributed fencing.
- [x] W07.4 Add conservative transaction/block limits, CAS, stale-writer and
  deterministic lease-fencing checks. Provider restart and hosted identity remain
  separate acceptance work.
- [ ] W07.5 Add Node, CLI, native-mount and macOS/Linux acceptance coverage.
- [ ] W07.6 **FoundationDB metadata + RustFS S3 chunks:** main passed the real-service
  composition and provider contract in the full RustFS harness (exit 0), with
  multi-chunk round trips, fresh-client reopen, CAS and expired-writer fencing.
  The surrounding RustFS/PGlite VFS restart checks also passed, but are not
  FoundationDB service-restart evidence. FDB service restart, root integration,
  and hosted composition coverage remain open; no emulated acceptance.

## W08 — TiDB

- [x] W08.1 Review and land the separate TiDB metadata/block implementation and
  dependencies (`ca57758`); four unit tests, format checks and test binaries
  passed.
- [ ] W08.2 Complete the durable real TiDB/PD/TiKV Docker harness and restart
  results. The pinned v8.5.7 single-node ARM64 TiDB/TiKV run passed schema,
  UTF-8/trailing-space, identity and provider checks; the durable 3PD/3TiKV
  topology now fails fast because this Docker host has 8,232,747,008 bytes and
  the harness requires 10,737,418,240. MySQL compatibility alone does not
  constitute TiDB verification.
- [ ] W08.3 Verify provider time/fencing, ambiguous commits, concurrency and
  deployment durability assumptions. Liveness queries are not fsync evidence.
- [ ] W08.4 Add Node, CLI, native-mount and macOS/Linux acceptance coverage.
- [ ] W08.5 **TiDB metadata + RustFS S3 chunks:** the separate contract test and
  runnable harness are landed, but the actual mixed-provider test still needs
  explicit TiDB and RustFS services and has not been claimed as passed.

## W09 — napi-rs, Node API and packaging

- [x] Add public `createLoopback`/`resolveCapabilities` and associated types.
  Main passed pinned-oracle capability/binding/path/partial-I/O/error comparisons,
  TypeScript checks and package-content validation. This wrapper preserves caller
  driver/handle identity and does not own the caller's shutdown lifecycle.
- [x] Accept structural JavaScript drivers in mount/server factories and mixed
  S3 bucket maps, with owned-adapter cleanup and TypeScript declarations.
  Main independently passed the pinned-oracle structural factory suite and
  eight WebDAV DELETE status/survivor comparisons, plus 11 Rust WebDAV tests.
  Native structural mounts remain unverified; harness exports and remaining
  public API gaps are still open. These results do not qualify remote backends.
- [x] Migrate npm packages to `@mount-rs/core` and `@mount-rs/virtual-fs`,
  including loaders, dependencies, imports and distribution checks. No registry
  publication or namespace-ownership verification is implied.
- [x] Harden the scoped `@mount-rs/core` distribution metadata and generated
  platform-package loaders (`4fa908e`); aggregate, pack, loader and consumer
  checks pass. Publication and native artifact qualification remain open.
- [x] Land shared cancellation-safe shutdown, provider-reference release ordering,
  panic/error retry and Windows numeric flags. Main passed 12 native binding
  tests, strict Clippy, rebuilt-addon/oracle-enabled Node suite, consumer tests
  and TypeScript checks. R2/PGlite/native-mount opt-ins were skipped in this
  package run; hosted Windows verification remains open. Independent handles,
  servers and in-flight operations must finish before backing-file deletion.
- [x] Land JavaScript driver bridge, codec/server hooks and 9P ESM exports.
- [x] Fix shutdown test's early rejection handling (`8d1f4ad`); 25 strict repeats
  passed locally. This corrects the test race, not a proven runtime defect.
- [x] Add server-test phase/cleanup diagnostics (`2452463`); 20 strict main
  repeats passed. Earlier intermittent timeout cause is not established.
- [x] Full local Node suite passed after fixes, excluding opt-in service/native
  lanes; opt-in skips are not acceptance evidence.
- [x] `3042d09` exposes Rust-backed FUSE inode state through the N-API package;
  the Rust inode table, Node parity test, generated declarations and distribution
  checks passed locally.
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
- [x] Main passed the checked-in HTTP config as a real local demo after
  `4af2a30`: memory and split SQLite drives served, bearer isolation returned
  401, and split SQLite data survived a service restart. This is local
  evidence only and does not qualify live R2 or native-mount acceptance.
- [x] `d526c18` extends the HTTP subprocess contract with streamed writes,
  ranges, truncate, concurrent writes, aborted-write recovery and listener
  cleanup; the focused CLI/HTTP tests and strict Clippy passed locally.
- [x] `0d7f1f4` adds a bounded native demo using the Rust CLI, independent Rust
  and Node clients, real macOS NFS, SIGINT unmount and durable backing-byte
  verification.
- [x] `0dca1d1` adds the Node SDK CLI example and default mount-free checks;
  the opt-in macOS NFS self-test passed against the rebuilt local N-API addon.
- [x] `29337f7` adds direct Node SDK read/write self-test coverage and routes the
  Rust CLI through `mount-rs-sdk`; both CLI entry points are now usable examples
  of the public SDKs and are included in the provider/consumer matrix.
- [ ] W11.6 Run the same SDK-backed CLI flow against the configured metadata/
  block providers, including restart and cleanup, before treating the demo as a
  provider-integrated acceptance path.

## W12 — Safely host SQLite database files

- [x] Record prior Linux FUSE DELETE/WAL and macOS NFS single-host DELETE
  evidence; those configurations alone do not establish universal safety.
- [ ] W12.1 Define and test supported journal/locking modes per transport.
  The local VFS checkpoint covers the required rollback matrix for
  DELETE/TRUNCATE/PERSIST × NORMAL/FULL/EXTRA and explicit HostLocal/ProcessLocal
  WAL cases; MEMORY/OFF, all transports and hosted platform coverage remain open.
- [x] W12.2 Prevent silent WAL fallback from being reported as WAL success.
  `5d9e513` asserts effective WAL for supported HostLocal scope and rejects
  unsupported WAL requests instead of silently falling back.
- [ ] W12.3 Exercise multiple connections/processes, readers/writers, locks,
  sync barriers, rename/unlink, disk-full errors and crash/restart integrity.
  The checkpoint covers process/child-process locking, reader/writer snapshots,
  sync-fault recovery, checkpoint/reopen and integrity; rename/unlink,
  disk-full and hosted crash coverage remain open.
- [ ] W12.4 Run integrity checks and acknowledged-commit recovery across every
  supported metadata/block combination and operating system.
- [x] W12.5 Explicitly reject unsupported safety modes and document constraints
  for WAL capability scope in `docs/sqlite-vfs-wal-plan.md` (`5d9e513`).
- [ ] W12.6 Run the new Linux CLI SQLite-backed FUSE SIGKILL/reopen test in
  hosted CI. Implementation and bounded cleanup are added; main independently
  passed both shared macOS NFS lifecycle regressions, formatting and Clippy.
  Linux execution is not yet verified on this macOS host.
- [x] `d6b80f4` adds local SQLite VFS fail-closed behavior after injected block or
  metadata-publication failures, plus the focused unit/engine/bridge gates. This
  is a local reliability boundary, not hosted cross-platform acceptance.

## W13 — FSKit

- [x] Land unsigned SDK compile target (`5993984`); main arm64 compile passed,
  worker reported x64 compile. Neither is a mounted-filesystem result.
- [x] W13.1 Implement FSVolume operations/read-write and tested Swift/Rust IPC
  (`95aca9c`); Rust bridge tests, Swift frame tests, XPC lifecycle tests, and
  unsigned arm64 Xcode targets passed locally.
- [ ] W13.2 Verify errors, handles, identity, concurrency and lifecycle at the seam.
  The FSKit path-resource bridge now maps macOS 26+ `FSPathURLResource` values
  to rooted `HostFs` workers and its direct host-path lifecycle/reopen test
  passes; installed-volume concurrency and cross-mount visibility remain open.
- [ ] W13.3 Complete packaging, entitlements and signing plan, then request D03.
  A minimal `MountRsHost` containing app now embeds the FSKit appex in
  `Contents/Extensions` and builds unsigned; Apple team/profile authorization
  and activation remain open.
- [ ] W13.4 Activate and test real FSKit mounts, CLI integration, persistence and
  supported SQLite workloads. NFS/FUSE fallback does not satisfy this stream.

## W14 — Versioned filesystems

- [x] Land additive core versioning contract, separate coordinator crate and
  memory/SQLite history providers. Includes explicit publication identities,
  lost-ack fork reconciliation, pinned read-only views, restore, cross-store
  copying, schema/head validation and cancellation-safe pin accounting.
  Main tested the exact staged checkout: 6 memory, 10 SQLite and 20 versioning
  tests passed. This is not remote/Node/CLI/platform acceptance or physical COW;
  dropped views may retain pins until TTL unless explicitly closed.

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

- [x] Extend the same remote VFS fixture through a graceful PGlite process
  stop/start using its persisted data directory and a fresh listening port.
  Main's full local harness passed exact ledger and integrity checks after
  both RustFS and PGlite restarts, with fresh VFS test processes at each phase.
  Clean shutdown status is required. Abrupt process loss, power loss, WAL and
  hosted results remain separate unverified gates.
- [x] Actual RustFS service stop/start now has its own VFS fixture: separate
  prepare/reopen test processes share the persisted metadata/block prefix,
  verify two exact committed binary rows, exclusion of a rolled-back row,
  and `integrity_check = ok`. Full local RustFS harness passed both phases.
  PGlite itself was not restarted; this is not a power-loss or WAL result.
- [x] Live PGlite metadata + RustFS blocks VFS test passed locally: real SQLite
  transaction, exact binary bytes and integrity after reconnecting both clients.
  Added it to the standard RustFS harness instead of leaving it opt-in only.
  The surrounding RustFS suite also passed its service-restart check, but that
  fixture is separate: VFS-specific service restart and PGlite restart remain
  unverified. No remote power-loss durability claim is made.
- [x] Root-integrate the separate rollback-journal VFS and metadata/block
  storage bridge. Main passed 22 local tests and strict all-feature Clippy;
  the remote PGlite/RustFS test remains ignored in this run. Host-backed engine
  tests cover DELETE/TRUNCATE/PERSIST x NORMAL/FULL/EXTRA with exact binary
  ledgers and reopen checks. Memory/SQLite storage bridges have separate engine,
  fencing, failure and subprocess checks. The follow-up now adds all nine
  rollback journal/synchronous combinations to each memory and durable SQLite
  provider pair (18 cells), with exact committed/rolled-back bytes and reopen.
  WAL is explicitly rejected and remains unimplemented; Windows qualification,
  remote recovery, Node exposure, and broader crash/fault coverage stay open.
- [x] W15.1 Complete the separate draft crate and actual mount-rs storage bridge
  (`5d9e513`); host-file and StorageBackend implementations are both tested.
- [x] W15.2 Fix registration lifetime escapes through connection extraction or
  mutable access; test duplicate names and independently opened connections.
  Explicit quiescent close releases provider resources, rejects active callbacks
  and file handles, and retains backend-free callback tombstones. Main added
  extracted-connection and closed-wrapper/name-reuse regressions and passed
  28 local tests. Reentrant backend destruction runs outside the registry lock.
  Two remote tests remain opt-in; these local results are not WAL acceptance.
- [x] W15.3 Replace noop-waker/busy polling with a valid executor contract and
  test a real wake path (`5d9e513`); no hidden busy/noop-waker success path is
  used by the process-local bridge.
- [x] W15.4 Implement cross-connection SQLite locking or explicit safe
  serialization with fencing for the supported HostLocal/ProcessLocal scopes;
  child-process contention and stale-reader promotion tests pass in `5d9e513`.
- [ ] W15.5 Test stale-reader promotion, separate processes, lost updates,
  short reads, sync failures, journal recovery and database integrity. The
  checkpoint covers stale-reader promotion, separate processes, sync failures,
  journal recovery and integrity; lost-update/short-read and broader hosted
  crash coverage remain open.
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
  versioned-view and actual native-mount shared-visibility acceptance. The new
  `tests/native_shared_visibility.rs` records the macOS capability boundary
  without claiming that acceptance: the current FUSE API is Linux-only and the
  FSKit target has no signed/activated mounted-volume host. The real three-way
  test remains open until all three independent mount authorities and their
  shared-provider locking contract exist.

## W17 — Multi-drive HTTP server

- [x] Root-integrate the separate HTTP crate and shared lockfile. Real loopback
  clients exercise isolated memory/SQLite drives, bounded requests, streaming
  disconnect cleanup, and shutdown retries. Main passed 7 unit and 6 integration
  tests locally. Listener completion is cached under one mutex, with a
  deterministic cancellation-after-join regression. Hosted, Node/CLI, remote
  backend, and distributed-cache acceptance remain open.
- [x] W17.1 Implement a separately packaged server and drive registry/config;
  expose several named drives, including unmounted drives.
  CLI `serve-http --config` reuses provider factories with per-drive env-token
  references. Main tested the exact staged snapshot: 38 unit, 7 CLI, 2 real
  HTTP subprocess and 1 native-artifact tests passed; 2 native-mount tests
  remained opt-in/ignored. Tests cover memory/SQLite/split-store isolation,
  forced-process SQLite reopen, volatile-store loss, and Unix SIGINT shutdown.
  Portable HTTP coverage now runs on Windows CI; no Windows runtime pass yet.
- [x] Add mandatory real CLI HTTP gate to the RustFS harness: PGlite metadata
  and SQLite metadata each use actual RustFS chunks. Main's full harness run
  exited 0 with binary multi-chunk writes, full/range reads, per-drive auth,
  graceful shutdown and fresh-process durable reopen with distinct default
  owners. Existing VFS/RustFS/PGlite restart checks also passed. The live
  Cloudflare R2 CLI gate now passes; Windows remote execution and abrupt CLI
  crash recovery remain open.
- [ ] W17.2 Define discovery, routing, filesystem operations, streaming/ranges,
  stable errors and lifecycle; share the actual native/API drive namespace.
- [ ] W17.3 Add per-drive authorization/isolation, limits and deployment/TLS guidance.
- [ ] W17.4 Test multiple drives and mixed stores through real HTTP clients,
  including concurrency, restart, failures and Node/CLI configuration. The
  local memory/split-store subprocess now covers concurrency, range reads,
  truncate, aborted writes and process restart (`d526c18`); remote, Node and
  crash-recovery coverage remain open.
- [ ] W17.5 Define a cache integration boundary, but do not implement/select the
  distributed cache until the W22 discussion and primary acceptance.
- [x] `4af2a30` records a runnable cross-platform local demo using independent
  SQLite metadata/blocks, live PUT/GET, authorization isolation and process-
  reopen persistence. Cloud/TLS/deployment acceptance remains open.

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
- [x] Live Cloudflare R2 smoke benchmark passed on 2026-09-20 through the public
  N-API split-store path with PGlite metadata, R2 blocks, fixed 64 KiB chunks,
  verified full-byte readback, delete and remote-prefix cleanup. This is one
  macOS smoke measurement, not completion of the full matrix.

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

- [ ] Windows pinned-oracle N-API parity and the portable HTTP tests are now in
  the `windows-node` job (`d526c18`); hosted execution and Windows Rust/host
  runtime qualification remain required.
- [ ] W20.1 Obtain revision-matched green required hosted macOS/Linux jobs.
  Latest jobs were queued/in progress at this update; earlier Node/PGlite
  failures are not closed by local fixes alone.
- [ ] W20.2 Verify new RustFS and CLI native gates actually execute and pass.
  Main reran all three macOS CLI native tests after collision-resistant paths
  and panic-safe cleanup: passed, including actual SQLite DELETE/FULL reopen.
  Linux crash harness now serializes native cases and tries regular unmount
  before lazy fallback; its previously failed hosted gate remains unverified.
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
- [ ] W21.9 Review Rivet Actors' FoundationDB/SQLite VFS implementation at a
  pinned revision. Record exact source paths, transaction/locking/durability
  assumptions, ambiguous-commit behavior and recovery tests; map adopt/adapt/
  reject decisions to W07/W12/W15/W28 and preserve applicable attribution.
  Source review recorded at [Rivet Actors review](docs/reviews/rivet-actors.md),
  pinned to `78336a1a0ee33bb45e6b15e89963cbe91713353b`: Apache-2.0, no code
  copied. Relevant implementation is UDB/Postgres/RocksDB rather than a direct
  FoundationDB client. Stable operation IDs, fencing and explicit sync/commit
  boundaries are applicable; single-actor no-op SQLite locks are not. Mapping
  all lessons into implemented acceptance tests remains open.

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

- [x] W24.1 AWS MCP verified the Route 53 hosted zone for `mount-rs.com` and
  configured the apex and `www` A records to Vercel at `76.76.21.21`.
- [x] W24.2 Built and deployed the combined marketing/docs site with TanStack
  Start to the user's Vercel Hobby plan, explicitly authorized 2026-09-20. No
  paid plan upgrade or paid resources. Claims and support status are visible.
- [x] Site child task delivered Vercel prebuilt-output/header/route hardening in
  `c9088aa` and `8cf0c5d`; these commits are site-only and do not prove a live
  Vercel deployment.
- [x] W24.3 Public DNS, HTTPS and the actual deployment are verified for
  `mount-rs.com` and `www.mount-rs.com`. Route 53 change
  `C0259256PYIMTC38BKLA` reached `INSYNC`; both hosts returned HTTP 200 from
  Vercel with the expected homepage and security headers. Operational handoff
  is complete; this backlog entry did not authorize spending.

## W25 — Actual AWS S3 integration

- [x] W25.1 AWS MCP became available after the app restart. STS identity and
  account-owned bucket inventory verified; testing region is `ap-southeast-2`.
- [x] Create and read back private bucket
  `mount-rs-integration-106427005394-ap-southeast-2`: all four public-access
  blocks enabled, bucket-owner-enforced ownership, AES256 server-side
  encryption, test-resource tags, seven-day expiry under `mount-rs-tests/`,
  and one-day incomplete multipart cleanup. No access keys were created.
  Local `myroot` SSO credentials are expired; secure local test authentication
  and least-privilege test access remain pending. MCP provisioning is not a
  Rust integration test result.
- [ ] W25.2 Provision private test bucket, narrowly scoped access, and test-data
  cleanup/retention policy. Keep credentials outside chat and source control.
- [ ] W25.3 Execute actual AWS S3 block and composed-filesystem integration
  tests with restart/reopen, ranges, conditional immutable writes and cleanup.
  AWS S3 evidence does not replace Cloudflare R2 or RustFS acceptance.

## W26 — Apache Ozone S3 backend

- [x] Land isolated, digest-pinned Apache Ozone 2.2.1 gateway harness and
  an Ubuntu CI gate. Main's real Linux-arm64 Docker run passed immutable
  blocks, conditional create/read/write and CAS, concurrent publication,
  service restart/reopen, and owned-resource cleanup. Hosted Linux-amd64
  results and mixed metadata-provider/Node/CLI coverage remain open. The
  all-in-one non-secure test deployment is loopback-only, not production auth
  or replicated-durability acceptance.
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

## W27 — Native Windows support and CI

- [ ] W27.1 Run native `windows-latest` Rust formatting, all-feature Clippy and
  workspace tests; repair platform compilation and behavior failures rather
  than adding continue-on-error. Windows CI is added, not yet verified green.
- [ ] W27.2 Build the Windows napi-rs addon and run native Node factories,
  chunked storage, just-bash/Mastra consumers and TypeScript checks in CI.
- [ ] W27.3 Expand to pinned mountx differential parity, real SQLite/PGlite,
  authenticated R2 and other required backend/service tests on Windows.
- [ ] W27.4 Qualify path/drive-letter handling, open-handle deletion, locks,
  process/service lifecycle, restart recovery and artifact installation.
- [ ] W27.5 Define and implement Windows mount support separately from Unix
  FUSE/NFS and macOS FSKit. Explicit unsupported operations are not proof of
  Windows mounting acceptance; retain capability and evidence matrices.
- [x] `d6b80f4` adds the HostFs Windows packet for unprivileged symlink flags,
  rooted absolute-target inference and metadata/lifecycle behavior. The local
  suite passed 14/14 where runnable; native `windows-latest` execution remains
  required.

## W28 — Deterministic fault injection

- [x] Land isolated storage-wrapper crate with explicit occurrence-based plans,
  redacted pending/completed/cancelled traces and optional delays. Main passed
  seven all-feature tests and strict Clippy locally. Seed labels evidence, not
  randomized scheduling. Dedicated Linux/macOS/Windows CI added; hosted results
  remain pending.
- [x] Register the crate in the root workspace and shared lockfile. Add four
  composed filesystem tests, each exercised with memory and SQLite metadata/block
  stores: pre-write ENOSPC, lost publish acknowledgement, failed metadata flush,
  and invalidated writer lease. Assert namespace/bytes on reopen and explicit
  temporary-directory cleanup. Local all-feature suite: 11 passed. These are
  in-process wrapper faults, not power-loss or remote-service qualification.
- [ ] W28.1 Add a separate minimal-dependency fault-injection crate with explicit
  opt-in plans, operation/occurrence selectors, seeded replay and event evidence.
  Wrap metadata and block stores without changing production defaults.
- [ ] W28.2 Cover before/after-operation failures, lost acknowledgments, IO/full/
  permission errors, delays/timeouts, missing/corrupt/torn blocks, lease expiry,
  stale fencing, CAS conflicts and failed sync barriers. Record unsupported
  hooks and add transport/process controls rather than pretending wrappers
  simulate kernel, network or power-loss behavior.
- [ ] W28.3 Wire plans through test CLI/Node/VFS/HTTP entry points; isolate test
  resources, redact data/credentials, bound execution and verify cleanup.
- [ ] W28.4 Sweep defined fault points in the SQLite reliability matrix, retain
  seed/plan and minimized failing trace, check integrity plus exact transaction
  history and acknowledged-commit durability for the selected configuration.
- [ ] W28.5 Require bounded PR fault suites and broader scheduled matrices on
  Linux/macOS/Windows; publish coverage and remaining gaps, not an unbounded
  claim that every possible fault has been tested.

## W29 — User-configurable lifecycle hooks (later)

- [ ] W29.1 Define an extensible event catalog for files/folders: creation,
  opening/closing, writes, truncation, metadata changes, rename/move and deletion;
  and drive/server/connection lifecycle: starting, started, stopping, stopped,
  connection opened, dropped, reconnecting, reconnected and failed. Distinguish
  requested operations, successful completion and failure events.
- [ ] W29.2 Design registration, filtering by drive/path/event, removal and
  event payloads for user-supplied after-event hooks. Review Rust, Node, CLI
  configuration and mount-free HTTP integration surfaces; keep integrations
  separate from core and dependency costs minimal.
- [ ] W29.3 Specify ordering, concurrency, delivery/retry/deduplication behavior,
  cancellation, bounded queues/backpressure and shutdown draining. Clearly
  define after-write versus after-durable-commit; do not imply exactly-once
  delivery or crash-surviving hooks without an implemented durable mechanism.
- [ ] W29.4 Define hook timeouts, error isolation, reentrancy/recursive-event
  prevention and permissions. Hooks must not silently corrupt file operations,
  SQLite durability, lease/fencing or transaction outcomes. Redact credentials
  and avoid exposing file contents by default.
- [ ] W29.5 Implement the agreed API later and test event payloads/order,
  registration/removal, hook failures, connection drops/reconnects, startup and
  shutdown, concurrent operations and process failures across supported entry
  points/backends/platforms. Integrate W28 fault injection and document gaps.

## W30 — OpenTelemetry observability (later)

- [ ] W30.1 Define trace spans, metric instruments and structured log events
  across filesystem operations, metadata/block providers, chunking/compression,
  SQLite VFS, mounts, Node, CLI and HTTP services. Include lifecycle, latency,
  errors, retries, lease/fencing, cache behavior and durability boundaries.
- [ ] W30.2 Implement optional instrumentation and a separate integration crate
  where practical; keep exporters/SDK dependencies out of the minimal core and
  preserve a low-overhead disabled mode. Applications own provider/exporter setup.
- [ ] W30.3 Propagate context across Rust async tasks, napi-rs/Node, HTTP and
  backend calls; correlate traces and logs. Define sampling, resource identity,
  versioned event/attribute conventions and bounded metric cardinality.
- [ ] W30.4 Provide configurable OTLP export for traces, metrics and logs with
  bounded queues, timeouts, flush/shutdown and exporter-failure isolation. Avoid
  secrets, file contents and unbounded/sensitive paths in telemetry by default.
- [ ] W30.5 Add collector-backed integration tests for all three signals,
  context propagation, redaction, disabled mode, dropped connections, exporter
  failures and shutdown. Benchmark overhead and qualify macOS/Linux/Windows;
  document setup and dashboards/examples without claiming unverified coverage.

## W31 — Per-drive mounts from one backing datastore (future)

This is a future workstream for exposing multiple independently mounted
drives, each with its own drive identity and namespace, while sharing one
metadata/block backing datastore. It must not be implemented by treating a
shared provider handle as an implicit global filesystem or by weakening
cross-drive isolation.

- [ ] W31.1 Define drive identity, namespace roots, ownership, quotas and
  lifecycle when several drives share one metadata and/or block provider.
- [ ] W31.2 Specify metadata schema/indexing and block reachability so each
  drive can be opened, snapshotted, copied, retained and garbage-collected
  independently without deleting another drive's live blocks.
- [ ] W31.3 Define locking, leases, revision/CAS boundaries and crash/reopen
  behavior for concurrent writers on the same drive and on different drives.
- [ ] W31.4 Expose the drive registry through the CLI config, HTTP multi-drive
  API, Node/mount-free APIs, FUSE/NFS/9P/FSKit transports and future native
  mounts without silently collapsing all drives into one namespace.
- [ ] W31.5 Test per-drive authorization, path isolation, shared-store
  cleanup, quotas, provider failures, restart, versioned views and future
  distributed-cache/copy-on-write interactions across memory, SQLite, PGlite,
  R2/RustFS and the database metadata providers.
- [ ] W31.6 Benchmark shared-store efficiency against separate backing stores;
  document supported combinations, migration/format versioning and safe
  deletion rules before enabling automatic cleanup.

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
| `95aca9c` | FSKit Rust/Swift/XPC bridge checkpoint | Local tests and unsigned arm64 Xcode builds; signing/activation/mount pending |
| `ca57758` | TiDB metadata/block providers and pinned harness | Real single-node v8.5.7 ARM64 qualification passed; durable topology and RustFS composition remain open |
| `5d9e513` | SQLite VFS/WAL reliability checkpoint | 4 unit, 15 SQLite-engine and 15 storage-bridge tests plus strict Clippy; Windows/remote/Node acceptance remains open |
| `67498a2` | TiDB schema-test lint follow-up | Focused lint correction; no new service qualification |
| `d526c18` | CLI HTTP edge cases and Windows pinned-oracle CI | Local CLI/HTTP tests and Clippy passed; hosted Windows/oracle execution remains pending |
| `afc55cb` / `c9088aa` / `8cf0c5d` | TanStack Start site and Vercel output hardening | Production deployment and custom-domain DNS/HTTPS verified; broader project release readiness remains open |
| `a5d1dd2` | Windows HostFs and FUSE protocol parity | Focused macOS tests/Clippy; hosted Windows qualification pending |
| `7508a56` | Scoped Cloudflare R2 CLI gate and credential redaction | Runner added; object-count compatibility was fixed in `00e96ce` |
| `00e96ce` | Cloudflare R2 CLI object-count compatibility | Live bucket-scoped CLI, N-API factory, parity and smoke benchmark passed; cleanup readback passed |
| `ae7c4cb` | Isolate inherited PGlite URL from the preflight trace lane | Focused stale-URL regression passed; pushed to `origin/main` |
| `73c33e0` | Isolate PGlite lifecycle from all preflight suites | Full macOS acceptance with live PGlite/R2 exited 0; native/hosted gates remain open |
| `0de1832` | W01 core in-memory parity harness | 56-step pinned-oracle trace passed; later/provider/concurrency behavior remains open |
| `ae2f12d` | W01 skip inventory and deterministic trace evidence | 88 skips classified; five memory seeds passed; full matrix remains open |
| `dd65770` | Rust-backed N-API FUSE codec subpath | Rebuilt-addon smoke, TypeScript declarations and codec test passed; native session remains open |
| `0d7f1f4` | Rust CLI native end-to-end demo | Actual macOS NFS plus Rust/Node mounted-path I/O and cleanup passed |
| `0dca1d1` | Node SDK CLI example and integration test | Argument checks plus opt-in actual macOS NFS SDK self-test passed |
| `b6800f7` | W01 concurrency, provider/consumer and SQLite acceptance packets | Full root gate exit 0; PGlite rows passed; R2/live native and hosted platform gates remain open |
| `29337f7` | Public Rust SDK facade, Rust CLI routing and Node SDK CLI self-test | SDK unit/example, Rust/Node/provider matrix and CLI self-tests passed locally; live remote/native/hosted lanes remain open |
| `d6b80f4` | Chunked persistence, SQLite VFS failure handling and Windows HostFs acceptance packets | Focused local chunked, SQLite and HostFs gates passed; hosted Windows and remote-provider lanes remain open |
| `3042d09` | Rust-backed FUSE inode state in napi-rs | Rust/Node inode parity, generated package checks and complete local N-API suite passed; native mount remains open |
