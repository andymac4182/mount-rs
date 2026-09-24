# Shared directory ownership plan

**Goal:** Provider-fenced directory checkout/checkin with safe SQLite ownership and fresh-mount handoff.
**Architecture:** New MRC3 protocol fences legacy and MRC2 writers. Provider transactions validate complete namespace deltas against grants. Shared delegated operation remains write-through.
**Constraints:** No client-clock expiry, automatic force takeover, cross-boundary hardlinks/rename, or live NFS/FSKit handoff claims. Preserve phase1 defaults and evidence.

- [x] Task1: Core grant/state/token types, adversarial subtree delta validator and metadata extension contract, ENOTSUP defaults; test-first authorization, hardlinks, overlaps, allocation and stale fences.
- [x] Task2: Durable SQLite protocol/enrollment/claim/publish/checkin/recovery using immediate transactions and physical backing checks; old-client fencing and fault tests.
- [x] Task3: Remote metadata implementations PGlite/TiDB/FoundationDB with atomic equivalent checks and contract tests; fail unsupported paths closed.
- [x] Task4: Shared engine authority/handle-generation paths and clean drain/checkin, per-directory configuration and SDK/CLI/NAPI propagation. Preserve MRC2 and exclusive modes.
- [x] Task5: Fresh native Linux two-client directory SQLite ownership/handoff qualification; delegated overhead benchmarks; independent reviews plus full checks, documentation and goal completion.

Core review caught and fixed sparse allocator exhaustion; contiguous allocated IDs are now required. Core35 tests and SQLite69 tests passed. Other tasks remain in progress.

Remote qualification: isolated PGlite29 tests passed. Actual TiDB8.5.7 single-node topology passed delegation and direct/concurrent/load/ambiguous-commit suites. Actual FoundationDB7.4.7 arm64 single ssd-2 server passed standalone delegation with exit0. These are server contract smoke lanes, not replication or power-loss acceptance. Provider review approved after malformed Legacy-fence enrollment fixes. Shared native WAL remains under investigation, so Task5/goal remain active.

Finalengine51unit+44legacyCAS+9delegated+1example tests passed, strictClippy green. CLI79tests plus portableintegration and realofflineSQLite dispatch passed. NAPI32Rusttests include nativecreation cancellation/failure guards; finalJSartifactchecks pending. Independentcore/provider/engine/CLI/NAPI reviews approved after corrections. Finalcoherent nativeShared LinuxFUSE3consecutivepasses14.69/18.82/20.66s, unchangedsourcehash2045, zeroinvalidorphantraces. Exactsourcewholeworkspace/fullJS checks andquietfinalbenchmarks pending.

Completed: finalworkspace1156passed/89ignored/0fail; strictworkspaceClippy/fmt/diff green. NAPIreleasefullsuite defaultdeadlines green. Final22-casebenchmark154measured+22warmups allverify exactsize/bytes,896 overlapdenials,25provenancehashesmatch. ExclusiveSQLitebatch16pages1.68x,namespace26.13x; sync1pagesnogain; delegateddisjointmedian436.936msvsMRC2 297.233ms, safehandoff2.989ms. Allownedtestservices/VMquiet; allreviewfindingsresolved. See docs/mount-ownership-validation.md for final evidence and explicit qualification bounds. No commits/deployment.
