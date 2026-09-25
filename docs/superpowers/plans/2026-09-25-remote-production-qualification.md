# Remote production qualification implementation plan

> For agentic workers: use subagent-driven-development for bounded implementation tasks and independent review.

**Goal:** Complete secure WebSocket fallback and diagnose, improve, and qualify storage/cache performance toward ten servers and 10,000 separate-drive clients.

**Architecture:** Preserve the existing provider, Partition/Drive authorization, immutable backing authority, negotiated binary protocol, and uncertain-write behavior. Add a second transport and improve measured resource bottlenecks through existing SDK/provider seams.

**Tech Stack:** Rust, Tokio, Quinn, rustls, SQLite, PGlite, TiDB, FoundationDB, Node benchmark harness, Docker live-service fixtures.

## Global constraints

- Linux/macOS mounting behavior is unchanged; a connection spans only one Partition.
- Per-Drive authoritative read/write OIDC checks and revocation remain mandatory.
- One current negotiated protocol version; no compatibility implementation.
- Writes commit to backing storage before acknowledgment; uncertain writes are never replayed.
- Keep the existing 1,000 lifecycle IOPS threshold; record legacy versus inode layouts explicitly.
- Ten servers, up to 10,000 mostly separate-drive clients, both mostly idle and all active.
- Keep caches bounded in RAM, disk, network, and concurrent misses; no cache durability claims.
- Serialize Cargo and timed loads; use scripts/cargo-shared and a checkout-specific external target.
- Preserve unrelated dirty work. Distinguish local, VM, native, cross-host, and production evidence.

## Task 1: WebSocket fallback

Files: crates/mount-rs-remote-client/src/{connection.rs,websocket.rs,lib.rs}, crates/mount-rs-service/src/{server.rs,websocket.rs,lib.rs,transfer.rs}, apps/mount-rs-cli/src/remote.rs, relevant Cargo manifests/lock, remote-client integration tests, docs/remote-drives.md.

Consumes Transport::exchange/read/write/close, Message/Incoming binary codecs, Authenticator::authenticate, DriveDispatcher, SessionHandles, admission helpers.
Produces TLS WebSocket listener/client and explicit CLI transport/fallback configuration preserving existing QUIC API.

- [ ] Read complete transport/session and CLI contracts; document exact public API decisions before editing.
- [ ] Add meaningful failing real-TLS tests for WebSocket filesystem roundtrip, initial UDP-unavailable fallback, revoked/read-only/cross-Partition access, rejected negotiation, malformed/oversize frames, shutdown, and uncertain write nonreplay.
- [ ] Implement bounded negotiated transport and CLI wiring with existing dispatcher authorization and handle lifecycle; classify fallback conditions conservatively.
- [ ] Run remote-protocol/client/service and CLI focused tests and strict touched Clippy; retain commands/results in task report.
- [ ] Commit touched source/tests/docs and obtain independent task review.

## Task 2: Controlled storage diagnosis and fixes

Files: benchmarks/storage/{runner.mjs,providers.mjs,test.mjs}, bindings/mount-rs-napi/src/memory_factory.rs, providers/mount-rs-tidb/src/storage.rs, crates/mount-rs-sdk/src/{stores.rs,providers.rs,filesystem.rs}, filesystems/mount-rs-chunked/src/lib.rs, focused concurrency/lifecycle tests, measurement scripts/docs.

Consumes existing lifecycle benchmark, provider diagnostics, legacy and inode publication APIs.
Produces controlled comparison artifacts, diagnosed amplification, bounded pool/startup improvements where supported by evidence.

- [ ] Retain baseline exact source identity; run serial controlled workloads with operation/resource counters.
- [ ] Trace counts and waits; reproduce each proposed defect with failing regression before modifying production code.
- [ ] Implement supported fixes with correct per-volume authority, pool lifetime, and nonreplay of ambiguous commits.
- [ ] Run affected provider/SDK/filesystem tests and actual datastore tests; repeat identical workload and compare profiles.
- [ ] Preserve existing floor and oracle; record achieved results and uncertainty; commit and independently review.

## Task 3: Distributed-cache qualification

Files: crates/mount-rs-blob-cache/{src/{store.rs,local.rs,peer.rs},tests/distributed_failure.rs}, scripts/test-cache-redis.py, docs/distributed-blob-cache.md, retained compact evidence.

Consumes Discovery, PeerTransport, CachedBlockStore, LocalCache and actual QUIC peer API.
Produces failure qualification and measured backing-read savings, plus fixes demonstrated by failing tests.

- [ ] Inventory existing tests, avoiding duplication; select missing end-to-end failure seams.
- [ ] Run real peer reads/writes and assert bytes plus backing counters under cold/warm/local/peer conditions and concurrent misses.
- [ ] Inject peer loss/restart, stale hint, corrupt cache, and disk pressure; verify isolation, bounded fallback, durable writes, cleanup.
- [ ] Fix only reproduced gaps, run focused/live tests and touched Clippy, retain evidence, commit and independently review.

## Task 4: Ten-server scaling and final delivery

Files: crates/mount-rs-service/tests/{quic_tidb_saturation.rs,support/saturation_backend.rs,support/resource_profile.rs}, scripts/{bench-remote-scaling.sh,summarize-remote-scaling.py}, docs/remote-client-scaling.md, docs/remote-production-qualification.md, CI workflow as required.

Consumes Task 2 storage fixes and existing byte oracle; covers Task 1 fallback and Task 3 cache in end-to-end matrix.
Produces idle/all-active separate-drive ramp results through 10,000 clients when achievable, explicit machine boundaries, complete follow-up PR.

- [ ] Verify small ten-server separate-drive run first; retain auth/oracle/cleanup results.
- [ ] Ramp client counts for 100-active and all-active cases, collecting peak resources and datastore counters; stop each failed ramp point and diagnose before continuing.
- [ ] Fix demonstrated resource blockers, rerun failed point, attempt 10,000 clients, retain both success and failure evidence.
- [ ] Run QUIC and WebSocket end-to-end auth/filesystem/cache tests plus applicable native mounting/live backends; run full formatting, strict Clippy, workspace tests, and relevant CI.
- [ ] Independently review full branch and correct findings; create/attach PR and report verified results and remaining environment limits.
