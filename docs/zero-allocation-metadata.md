# Zero-allocation typed I/O metadata

## Scope and measured result

Successful QUIC read/write request metadata encoding and decoding now allocate **0 heap objects and 0 bytes**. The client borrows its Drive ID; the server stores it inline. The fixed metadata prefix is 18 bytes, followed by a validated 1..64-byte ASCII Drive ID. The negotiated client protocol remains v2; obsolete JSON typed metadata is rejected. Generic control operations still use JSON.

The server also avoids owned session identity clones, per-request drive lookup keys, policy trees, grant vectors, temporary JSON-pointer strings and audit JSON trees. Allocator regressions measure zero for borrowed policy matching, claim lookup, audit serialization and unchanged catalog data selection. Changed catalogs and control/session setup allocate. Async scheduling, provider futures, transport buffers and payload ownership are outside those zero-allocation measurements.

SQLite checks the backing identity and reads the authoritative revision and complete document bytes on every request. Only an exact revision AND byte match reuses the decoded Arc snapshot; same-revision changes, malformed documents and revocations are tested. This removes repeated catalog decoding without a freshness window.

## Codec allocations per operation

| Operation | Before | Now |
| --- | ---: | ---: |
| Borrowed write request encoding | 4 | 0 |
| Typed metadata decoding | 2 | 0 |
| Full write decoding | 3 | 1 (raw payload only) |
| Read decoding into caller buffer | 0 | 0 |

Release measurements cover 4 KiB, 64 KiB and 1 MiB payloads. Allocation counts are unchanged across these sizes. The provider Vec clone in the read encoding fixture still costs one payload allocation. See [codec measurements](benchmarks/zero-metadata-20260925/codec-profile.txt).

## Full request measurements

100 clients, 10 servers, one shared drive, local SQLite 3.51.3, 4 KiB I/O, depth 1, 1-second warmup and 5-second stages, audit enabled. These are short laptop runs, not a production capacity claim. The write fixture uses the legacy namespace-document path (`inode_updates=false`), so it does not qualify the separate-drive inode-update production layout.

| Stage | Allocator disabled IOPS | Allocator instrumented IOPS | Rust allocations/completion | Requested Rust bytes/completion |
| --- | ---: | ---: | ---: | ---: |
| Read | 11,525 | 11,131 | 49.70 | 25,358 |
| Write | 356 | 345 | 17,332.37 | 3,926,835 |

Both runs had zero failures. Instrumentation includes clients, servers, background work and verification in one process; foreign C/SQLite allocations are excluded. Requested bytes are allocation traffic, not retained memory. Atomic instrumentation affects throughput, so its IOPS are reported separately. The remaining write allocation traffic demonstrates that the full provider/RPC path is not allocation-free.

Raw artifacts: [control](benchmarks/zero-metadata-20260925/control/sqlite.json), [instrumented](benchmarks/zero-metadata-20260925/instrumented/sqlite.json). These paired runs preceded restoration of the extracted typed dispatcher timeout to the original 30 seconds; no measured operations reached the shorter timeout. Generic and typed dispatch now share the operation timeout constant.

## Qualification

Counting allocator tests cover codec metadata and the server metadata helpers, with payload allocations separated. Wire tests cover bounds, truncation, invalid flags/IDs, absent-position canonicalization and obsolete layout rejection. Existing integration tests cover permissions, revocation, renewal, admission and uncertain-write nonreplay. Final workspace and bounded-header proof results are recorded in the implementation plan.
