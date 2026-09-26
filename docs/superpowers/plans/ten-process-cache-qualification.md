# Ten-process Cache Qualification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Qualify the existing public CLI's SQLite MRC5 distributed cache with ten real server processes, attributable logical backing-fetch savings, and independent persisted byte, metadata, and lifecycle checks.

**Architecture:** An explicitly invoked integration test supervises a separate worker in an owned process group. The worker starts public `mount-rs serve-remote` binaries, makes actual signed QUIC requests, and retains shutdown counters for each process generation. Fresh SQLite reads and an undecorated SDK reopen independently verify persisted contents; watchdog termination, process reaping, cache-lock release, and socket reuse are separate receipts.

**Tech Stack:** Rust integration tests, existing CLI debug/local OIDC fixture, ES256/ring, rcgen cluster CA and pinned leaf certificates, SQLite, existing SDK/remote client/cache discovery APIs, native macOS/GNU Linux process and filesystem calls.

## Global Constraints

- This is initially a **source-only, uncompiled draft**. No Cargo, formatter, binary, process, native fixture, Redis, Docker, proxy, or external service action is authorized by this plan's creation.
- Edit only the six new test/support sources listed below and this new plan. No production source, manifest, lockfile, workflow, service, SDK, core, TiDB, NAPI, or protected historical test edits.
- Native scope: ten distinct simultaneously observed CLI server PIDs, five partitions, ten drives, sixteen 4096-byte files per drive. This is lifecycle/cache qualification, not the 10,000-client capacity target.
- Native platform gate: macOS or GNU Linux. The CLI binary must be debug with `local-oidc-fixture`; no release-build or production OIDC discovery claim.
- Singleton setup budget **600 seconds**; work budget **1800 seconds**; each phase **600 seconds** clipped to work; each request/operation **30 seconds** clipped to its parent phase.
- Each generation stop and terminal fleet cleanup have **95 seconds**, clipped to the remaining work/parent budget where applicable; final **5 seconds** are force/reap. Final audit budget **30 seconds**. Outer watchdog total is **2525 seconds**.
- Sample each owned live PID under its retained `Child`/generation: RSS cap **24 GiB per PID**, plus **24 GiB for the observed sum of controller + worker + every retained live CLI server/catalog helper**. Equality breaches either cap. Require host free disk **64 GiB**. Samples are sequential/skewed cooperative observations, not a hard or continuous memory bound.
- Direct file-backed stdout/stderr: **8 MiB per file**, **16 KiB per line**, **160 MiB total retained log bytes**, cooperative **100 ms** polling. Overflow, missing sampling, malformed/partial required output, or late evidence fails and triggers bounded cleanup. These are not hard emission bounds.
- Default cache cell: node-wide RAM **64 KiB**, disk **1 MiB**, max blob **64 KiB**, max inflight **128**, peer transfer budget **8 MiB**, max entries **4096**, peer query limit **3**, deadline **500 ms**, maintenance capacity **64**, placement concurrency **4**, hedge delay **25 ms**.
- No fresh output path reuse. Require `MOUNT_RS_TEN_PROCESS_RUN=1` and a fresh absolute `MOUNT_RS_TEN_PROCESS_OUTPUT`; create it with private permissions and retain it from acquisition, including on failure.
- Force, missing reap, missing resource sample, or watchdog intervention means incomplete evidence. Never infer grandchild reaps from the worker's exit or infer worker drain from shutdown counters.
- Only private loopback endpoints, newly generated fixture credentials/certificates, owned directories, and owned children. Retain an unrelated UDP sentinel and prove it remains owned.
- Exact private credential inventory: `token-0.jwt` through `token-9.jwt`, `invalid-signature.jwt`, and `key-0.pem` through `key-9.pem`. After all owned children are reaped and oracles complete, retain safe identity hashes and remove only those owned files; removal failure means incomplete cleanup. Retain public certificates/JWKS. Uncertain ownership retains files under private permissions with explicit pending-cleanup/no-export status; never export keys or bearer tokens with evidence.
- No new production phase IPC or unauthenticated endpoint. No unsupported HTTP proxy, TiDB/RustFS substitution, Redis fixture, ENOSPC claim, or actual lost-reply replay/relay in this first cell.
- All future Rust commands use `scripts/cargo-shared` with explicit target `/private/tmp/mount-rs-public-compact-selection-cargo-target`, and require root's exclusive build/native lease.

## Owned files and interfaces

| New path | Responsibility |
|---|---|
| `apps/mount-rs-cli/tests/ten_process_cache.rs` | Ignored explicit native supervisor and private worker entries |
| `apps/mount-rs-cli/tests/ten_process_cache_support/mod.rs` | Pure/default vs native feature/platform gates |
| `apps/mount-rs-cli/tests/ten_process_cache_support/contracts.rs` | Fixed limits, bounded parser, logical-bank oracle, unknown ledger, process receipt and fixture-free sensitivity tests |
| `apps/mount-rs-cli/tests/ten_process_cache_support/config.rs` | Public CLI JSON, ten leaf identities, ES256 workload tokens, private paths, reserved peer addresses |
| `apps/mount-rs-cli/tests/ten_process_cache_support/process.rs` | Retained `Child` ownership, polling/caps, monotonic generations, cleanup, native resource samples, outer process-group watchdog |
| `apps/mount-rs-cli/tests/ten_process_cache_support/scenario.rs` | Signed RPCs, MRC5/raw-byte/SDK oracle, cache counterfactuals, authorization cells, immutable JSON receipt |

`contracts::parse_banks(&str) -> Result<BTreeMap<String, Bank>, String>` accepts exactly one terminal row per registered partition/drive. `Bank::expect(local, peer, backing, bytes)` checks the entire named generation's four logical totals; cache errors and maintenance drops remain reported separately. `ProcessReceipt::qualify(node, generation, pid)` rejects identity mismatch, missing exact config/binary/catalog launch binding, force, missing reap/success/resource observation/socket/lock proof. Every process/bank retains canonical launched config path/hash, expected CLI path/hash, catalog physical device/inode plus actual revision/document hash at launch and completion, and the catalog helper's expected CAS revision/input document hash. Initial catalog creation has an explicitly absent prior identity; final identity is required. No credential or environment dump is retained. `Submission::classify` distinguishes signed ACK, independently committed unknown, and unresolved; it rejects attempts other than one and ACK without an independent durable oracle. The submission helper has no composed lost-reply qualification.

`Fixture::service_config` writes the existing public configuration schema. `Fleet::launch/apply/stop_node/stop_all` retain every handle until actual bounded reap; helper failures remain owned during cleanup. `scenario::checked` polls live children/output/resources while awaiting bounded RPCs. Synchronous OS/database/filesystem calls remain cooperatively bounded by the separate outer watchdog; forced interruption cannot qualify.

## Aggregate resource protocol

Before runtime or fixture construction, `Fleet::new` samples the verified controller parent and worker itself with no CLI children, publishes `resource.json`, and retains `resource-initial.json`. The controller must observe that exact empty-child initial frame within **30 seconds**, included in the existing **600-second setup**; setup and the **2525-second outer deadline** start at the controller's shared-clock acquisition. Every sample/publication uses system **CLOCK_MONOTONIC** nanoseconds in both processes. Serialized `Instant`, filesystem timestamps and wall time are not freshness evidence.

Private atomic resource packets are capped at **16 KiB** and **13 identities** (controller, worker, ten servers, one catalog helper). They bind the fresh output path, controller/worker/group PIDs, monotonic sequence, exact node/role/PID/generation roster, sample envelopes, optional RSS/missing reasons, full and child-only subtotals, highest observed total and sticky failure. The worker revalidates `getppid` before and after controller sampling; only retained unreaped `Child` PIDs are used for CLI samples. No process discovery or unrelated process sampling occurs. Launch and actual-reap cutovers publish immediately, including the final empty-child terminal frame; ordinary supervised work publishes at 100 ms intervals.

`rss_totals` rejects missing, duplicate, foreign/replaced identities, unavailable values, capture errors and checked-sum overflow, then enforces individual and aggregate caps. `ResourceFrame::validate` requires exact run identity, no sequence regression, valid supervisor coverage/subtotals and an envelope no older than **one second**. Reading a prior packet never refreshes its timestamp. Outer recomposition uses newly measured controller/owned-worker values and only the fresh worker's child subtotal/observations; producer supervisor values are replaced, so they are not counted twice. A fully measured outer breach retains its actual sum/observations before refusal. Missing/stale observations remain incomplete, never zero.

An outer refusal writes a bounded, exact-run private `stop.json`; worker polling returns through the retained operation/error path into normal fleet cleanup. It does not signal libtest directly. The first refusal starts one **95-second** allowance clipped to the remaining outer budget; later failures never reset it. SIGINT is first, with resource/output/disk sampling during cleanup and force reserved for the final five seconds; output overflow retains immediate termination. Physical `owned_cleanup_closed` means actual reaps, socket/flock release and an empty terminal roster independently of resource/function success. Thus a cap failure can close ownership normally while `complete=false`. Missing physical closure retains the original WNOWAIT leader through any group action; actual reap permanently disarms numeric group signals.

The aggregate correction adds fixture-free threshold, missing/duplicate/foreign/restart, checked-overflow, common-clock freshness, supervisor coverage, no-double-count recomposition and stop-budget tests. Native RSS/IPC/cleanup behavior remains unexecuted until the separate native lease.

## Native cells and attribution

1. **Signed seed and fresh backing oracle.** Apply one shared SQLite catalog using the public CLI, sequentially initialize each MRC5 backing, then observe ten live server PIDs. Issue 160 signed file writes with one drive grant per sandbox and explicit `syncfs`, close connections, and reap all servers. Fresh typed compact metadata must contain exactly 17 members/nodes and 16 named regular files per drive; every file has one 4096-byte extent. Verify physical `write_mode=MRC5`, block authority identity, raw stored bytes for every extent, and bytes through a fresh undecorated SDK instance. This qualifies acknowledged writes and fresh persisted data, not a suppressed response after commit.
2. **Cold peer, in both deterministic and peer-query modes.** Start ten new empty private RAM+disk cache generations. Use only drive0/file0 as a 4096-byte working set; compute actual ranked placement via `FixedDiscovery`. Read from a source outside the holders, observe a complete atomic disk body on a holder, then read from a different requester outside source/holders with a private empty cache and cold provider. Its retired generation must have `local=0, peer=1, backing=0, hit_bytes=4096`; backing scope and block ID remain unchanged. This demonstrates one logical backing-fetch saving for that generation. It does not measure HTTP attempts, physical IOPS, or peer UDP bytes.
3. **RAM-only counterfactual.** A new requester has no eligible peers and disk=0. One read plus twenty repeats of the single 4096-byte resident working set must produce `local=20, peer=0, backing=1, hit_bytes=81920`. RAM is node-wide across all ten registered drive scopes; no other data read or accepted incoming placement shares this isolated generation.
4. **Disk-only persistence and corruption.** A requester has RAM=0 and no eligible peers. Observe complete owned disk admission before reaping. Restart at the same cache path, read exact bytes, and require `local=1, peer=0, backing=0`. After reap, flip one payload byte while leaving its stored checksum unchanged. Restart again; exact backing bytes and `local=0, peer=0, backing=1` prove corrupt cache fallback cannot be masked by another peer. Cache checksum detection is not proof against a malicious authorized peer.
5. **Observed eviction.** RAM=0, no peers, disk budget=4128 bytes (4096 body +32 checksum). Observe the complete first entry before reading a second distinct block. Observe the replacement entry, reap, and require first removal plus complete replacement retention. Restart and read the evicted first file; `peer=0, backing=1` distinguishes eviction from failed initial admission.
6. **Attributable holder restart and outage.** Reap one actual ranked holder; restart RAM=0 at its same peer socket and same disk path. A fresh requester is configured with exactly that one attested trusted peer. Its `peer=1, backing=0` is attributable to this restarted holder. Reap that holder, restart the requester with a new empty private cache and the same one-peer config, then require exact bytes and `peer=0, backing=1`. The earlier ten-live phase remains the ten-PID baseline; the fault cell intentionally has fewer live peers.
7. **Signed authorization and cached route controls.** A private requester rejects an invalid ES256 signature and a valid token for another partition with `ClientError::Authentication`. Prewarm valid bytes, then require sibling/cross-partition drive operations to return exact `EACCES`. A public catalog CAS changes the live grant to read-only; write must return `EACCES` while read succeeds. A second CAS revokes the drive; the existing cached route's next read must return `EACCES`. Final fresh persisted bytes, metadata, backing identities, and block IDs must be unchanged by all read/fault/denial cells.

All ten cache scopes export six aggregate counters at shutdown: local hits, peer hits, backing fetches, hit bytes, cache errors, maintenance dropped. Receipts retain these as **cumulative process-generation logical banks** with node/PID/generation identity. They do not prove exact phase intervals or maintenance quiescence: the current local shutdown ignores its two-second wait result and peer shutdown can wait indefinitely internally. Outer watchdog, actual `Child` reaps, fresh UDP binds, and nonblocking cache flock are separate checks.

Compiled deterministic/peer-query plugins are exercised. Directory discovery requires Redis and remains separately unqualified; the current peer-query plugin queries at most three candidates rather than every peer. Positive peer hits use actual cluster-CA mTLS and configured leaf pins. The existing crate's untrusted-peer and partition-denial tests are separate evidence; this ten-PID draft does not repeat those negative peer-auth fixtures.

## Tasks and future gates

### Task 1: Reviewed source draft and pure oracle sensitivity

- [x] Obtain read-only SPEC and QUALITY agreement for exact new paths, budgets, process ownership, and phase protocol.
- [x] Write fixture-free sensitivity tests before drafting their helper bodies: amplified backing counts, peer-masked fallback, duplicate/missing/unknown/partial banks, output limits, shared deadline clipping, replayed/false ACK ledger, forced/cross-generation lifecycle, and failed-admission vs eviction.
- [x] Freeze the exact seven-file source draft and source hash manifest outside the checkout. Preserve the protected historical manifest and classify this packet as source-only/uncompiled.
- [x] Have SPEC and QUALITY review the frozen draft. Correct material source issues within the same paths and freeze a new checkpoint.

There is no observed RED/GREEN yet. Once root grants an exclusive Cargo lease, run the pure tests first. For a meaningful oracle RED, retain an exact checkpoint where the named rejection is deliberately absent, run the identical amplified/masked/missing/replayed input test and retain the actual assertion failure, then restore the reviewed helper and retain GREEN. That demonstrates the test-oracle contract only; it is not evidence of an old production bug or a performance improvement. Do not use a test-only extension trait to invent a false old production API.

### Task 2: Exclusive compile, formatting, and focused regressions

- [x] Root grants the build lease after the catalog owner's source/Cargo window is released.
- [x] Run pure default tests, then feature-enabled ignored-target compilation without starting children:

```sh
CARGO_TARGET_DIR=/private/tmp/mount-rs-public-compact-selection-cargo-target scripts/cargo-shared test --locked -p mount-rs-cli --test ten_process_cache
CARGO_TARGET_DIR=/private/tmp/mount-rs-public-compact-selection-cargo-target scripts/cargo-shared test --locked -p mount-rs-cli --features local-oidc-fixture --test ten_process_cache --no-run
CARGO_TARGET_DIR=/private/tmp/mount-rs-public-compact-selection-cargo-target scripts/cargo-shared clippy --locked -p mount-rs-cli --features local-oidc-fixture --test ten_process_cache -- -D warnings
```

- [x] Apply/check formatting only to the six owned test sources. Freeze exact command logs, source/dependency manifests, and compiled test/CLI binary hashes before any overwrite.
- [x] Inspect direct dependency requirements. The draft uses `std` files/processes and existing async APIs, not direct `tokio::net`, `io-util`, or `sync`; no manifest change is requested. Any compiler-proven missing feature must be reported to root and postponed until explicitly authorized.
- [x] Preserve default/feature distinction. Pure tests start no native fixture; both native entries remain ignored.

### Task 3: Explicit capped native preflight

- [ ] Root grants a private loopback/SQLite ten-process native lease for exactly this frozen binary packet, after independent SPEC/QUALITY review.
- [ ] Check fresh output path, host disk floor, exclusive controller ownership, debug/local OIDC binary hashes, and no external fixture substitution.
- [ ] Run only the exact supervisor entry once:

```sh
MOUNT_RS_TEN_PROCESS_RUN=1 \
MOUNT_RS_TEN_PROCESS_OUTPUT=/private/tmp/mount-rs-ten-process-cache-native-UNIQUE \
CARGO_TARGET_DIR=/private/tmp/mount-rs-public-compact-selection-cargo-target \
scripts/cargo-shared test --locked -p mount-rs-cli --features local-oidc-fixture \
  --test ten_process_cache ten_process_sqlite_cache_qualification -- --ignored --exact --nocapture
```

The literal output above is a command template: replace `UNIQUE` with a previously absent packet-specific directory before authorizing execution. Do not invoke `native_worker` directly or use an unfiltered ignored test command. A bind collision fails the owned cell; no fallback topology, external port takeover, automatic replay, or broader fixture is permitted.

- [ ] Require receipt `complete=true`, successful nonforced worker exit/reap, all owned CLI/helper reaps, original process group absent, unrelated UDP sentinel retained, and every named logical/fresh byte/metadata/socket/lock oracle passed before the deadline.
- [ ] Independently audit retained source/binary/catalog/backing identities, every generation bank, output byte counts and RSS sample coverage. Missing observations remain incomplete; no fake zero values or worker-drain claim.
- [ ] Root obtains final SPEC/QUALITY review of the immutable native packet before accepting this bounded scope.

### Task 4: Separate future qualification lanes

- [ ] Keep exact phase cache counters, peer/client transport bytes, actual backing HTTP attempts/returned bodies, raw adapter registry, physical device IOPS, allocation/CPU profiles, TiDB/RustFS, Redis directory, ENOSPC, untrusted peer composition, and real reply-loss-after-commit qualification explicitly open.
- [ ] An eventual S3 counting/fault proxy must count actual HTTP attempts/returned bodies, separate proxy callers/upstream backing/remote/peer bytes, and have owned TLS/credentials/endpoints. Trait decorators are not an HTTP or device oracle.
- [ ] An eventual suppressed-reply relay needs one exact submission, an uncertain ledger with no fallback/replay after progress, and independent durable bytes plus metadata. Timeout inference alone is insufficient.
- [ ] Keep this SQLite packet's lineage separate from those later fixtures and the 10,000-client target.

## Current evidence status

Checkpoint02 had independent SPEC and QUALITY PASS for source-only scope. A later explicit fixture-free lease compiled the draft: seven pure tests pass in both default and debug/local-OIDC builds, two native entries remain ignored, CLI-scoped default and feature all-target strict Clippy pass, and six-file formatting checks pass. The first feature compile caught four absent `Arc<RemoteFsDriver>::read_file` helpers; test reads now use the existing `Loopback` wrapper. Native-only constants are gated; named `CacheSettings`/`ProcessSpec` group launch arguments. No dev dependency or feature change was required. Actual command logs and retained binaries are in `/private/tmp/mount-rs-ten-process-cache-fixture-free-20260926`. No server, database fixture, native control, Redis, Docker, proxy, or resource query has run in this lane. This is compile/pure-test acceptance, not native qualification; the separate ten-process lease and packet review remain open. The prior read-only audit is `/private/tmp/mount-rs-ten-process-cache-qualification-current-audit-20260926.md` (SHA256 `0048aa2068f73d5e563d4dc953d8001e38b25b4b39d7a68ceb3cbf36fe0ec7a9`). Future commands, receipts, and independent reviews must be bound to the actual new frozen source/binary generation.

The subsequent aggregate correction retained that prior packet unchanged. Five new oracle tests produced actual assertion failures against a forwarding-only helper scaffold, then passed after validation; the freshness boundary was aligned to the full capture envelope's start as specified, rather than its finish. Two further pure checks cover private stop-budget binding and outer recomposition without supervisor double counting. Final default tests are **14 PASS**; final debug/local-OIDC tests are **14 PASS, 2 ignored**. Default and feature CLI all-target strict Clippy and touched six-source formatting pass; one nested-if style diagnostic was fixed before the final gates. The retained correction packet is `/private/tmp/mount-rs-ten-process-cache-aggregate-20260926`. These are oracle feature/compile/style receipts, not an old production race, performance improvement, native RSS measurement, IPC/cleanup execution or ten-process qualification. Native dispatch is still held.

Independent review then found two deadline gaps. The current correction checks the original initial deadline before reading and after validating a positive receipt; its actual-used pure predicate rejects equality and late observations. Fixed common-clock deadlines now capture the `Instant` anchor before reading CLOCK_MONOTONIC and use the actual-tested anchored conversion, rejecting equal/past stamps. Two forwarding-scaffold regression assertions failed before these helper bodies were implemented; the same inputs now pass, with no tolerance/precision change. The pure mock root uses an absolute `CARGO_MANIFEST_DIR` path without creating it, so POSIX fixture syntax does not invalidate Windows helper tests; Windows runtime execution was not performed. Current default tests are **16 PASS**, debug/local-OIDC tests **16 PASS, 2 ignored**, both CLI all-target strict Clippy and six-source format checks PASS. Source-at-RED, final sources, binaries and HEAD transition are retained separately in `/private/tmp/mount-rs-ten-process-cache-aggregate-deadline-correction-20260927`; prior packets remain unchanged. This closes the pure predicates and source call ordering only. Native CLOCK_MONOTONIC sampling, process cleanup and ten-process qualification remain unexecuted in this lane and require the separate lease/review.
