# Zero-allocation I/O metadata

User authorization: remove remaining metadata allocations from the binary I/O path and measure the result. Retain future negotiation and only one supported client protocol; the code is unreleased so no old layout is retained.

## Design

Replace typed I/O JSON metadata with a canonical fixed binary prefix and bounded ASCII Drive ID (catalog IDs are 1..64 bytes, alphanumeric, dash, underscore, dot). Borrow outbound IDs; decode into inline owned storage, so both encoding and decoding allocate zero metadata bytes. Keep raw payload allocation separate. Reuse the existing authoritative authorization/handle checks without constructing a generic Value tree or synthetic owned Message for raw I/O. Do not bypass catalog freshness, grant revocation, backing identity or audit.

Alternatives considered: stack JSON buffers remove temporary buffers but retain string ownership/serde parsing; negotiated numeric drive slots add connection lifecycle/binding complexity. Fixed metadata plus borrowed/inline IDs directly removes all codec metadata allocations without per-session mapping state.

## Tasks

- [x] Typed codec: meaningful allocator RED test, canonical bounds/truncation/position tests, zero-allocation borrowed encode and inline decode; retain current-version negotiation and reject obsolete metadata layout.
- [x] Client/service integration: borrow client metadata, eliminate synthetic Message and metadata Value on raw dispatch; verify permissions, revisions, renewal, cancellation and uncertain-write nonreplay. Share immutable session identities, borrow policy/grant/drive lookup data and serialize audit fields through bounded buffers without metadata trees. Add shared SQLite catalog snapshot reads that compare authoritative revision AND every document byte before reusing a decoded snapshot; retain backing identity verification, and revalidate/decode on every changed document. Async task/future and payload allocations remain separately measured.
- [x] Allocation profile: measure successful metadata encode/decode and separately raw buffers, document end-to-end allocations honestly; regress zero allocations with a counting allocator and nonallocating writer.
- [x] Qualification: focused then workspace formatting, strict Clippy and all-target tests, bounded arithmetic proof, immutable spec/quality review; report exact supported scope.

Cargo: scripts/cargo-shared, CARGO_TARGET_DIR=/private/tmp/mount-rs-remote-drive-target. Serialize compilation/proofs/workloads. Preserve unrelated work. Base f574e901.

## Evidence

- Allocator RED: old typed encoding 4 allocations; metadata decoding 2. GREEN: both zero; full write decode retains one raw payload allocation.
- Service allocator regressions: audit serialization, borrowed policy matching, JSON-pointer lookup and unchanged catalog data reuse zero; owned snapshot positive control allocates. Catalog cache tests cover external revocation, same-revision replacement, malformed/type/empty/revision failures and backing-path replacement.
- Focused protocol/client/service all-target tests passed. Full workspace all-target tests (`--offline --locked -- --test-threads=1`, socket fixtures permitted) passed: 1,328 passed, 106 ignored, 162 result targets, zero failed. Ignored live-service tests are not qualification claims.
- Strict workspace all-target Clippy passed; workspace formatting and diff whitespace checks passed.
- Kani updated typed header bounds: 0 of 164 failed (1 unreachable), 3 of 3 cover properties satisfied. This proves header arithmetic/bounds, not the entire protocol or provider system.
- Independent immutable spec/quality review found one operation-timeout regression; corrected by shared 30-second dispatch constant and independently re-reviewed with no remaining actionable finding.
- Release codec and paired 100-client/10-server SQLite resource artifacts retained under docs/benchmarks/zero-metadata-20260925. Full RPC/provider paths continue to allocate; scope and measurements documented in docs/zero-allocation-metadata.md.
