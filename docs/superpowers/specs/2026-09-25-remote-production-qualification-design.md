# Remote production qualification

Approved scope: complete the four follow-ups requested after PR27, with end-to-end evidence.

## Contracts

Linux and macOS clients mount the existing remote provider. A session selects exactly one Partition and can access several Drives according to authoritative per-Drive OIDC grants. Backing storage remains the durability authority; caches do not acknowledge uncommitted writes. Metadata authorization errors fail closed. Protocol negotiation remains mandatory and supports only the current version.

## Transport

Implement TLS WebSocket client/server transport using the existing protocol and dispatcher. Configure QUIC, WebSocket, or automatic connection selection with an explicit fallback endpoint. TLS/certificate and authentication failures must fail closed. Automatic selection is restricted to initial connection establishment before an authenticated operation: an uncertain request never triggers transport switching or replay. Bound handshake, operation, frames, admission, and shutdown. Preserve renewal, handle lifecycle, revision fencing, payload validation, and partition isolation. Initial WebSocket RPC serialization is acceptable provided it is documented and tested; QUIC keeps concurrent streams.

## Performance

Reproduce the short lifecycle storage benchmark and separate-drive QUIC workloads on merged main. Record exact revision, configuration, repetitions, payload/chunk sizes, and storage layout. Trace operation counts through filesystem, metadata, immutable blocks, SQL, CPU, allocations, network, and available VM/disk counters. Do not infer physical laptop SSD IOPS from VM counters. Test legacy and per-inode layouts separately, preserving the existing throughput floor. Implement only fixes supported by reproductions, including shared bounded pools and first-time initialization if confirmed. Validate close ownership and ambiguous commit behavior.

## Scaling

Ramp ten servers with mostly separate Drives from small points to 10,000 connected clients; test 100 active clients and all clients continuously performing I/O. Use authentic TLS transport, per-sandbox authorization checks, immutable expected-byte ledgers, and fresh-driver verification. Retain failure artifacts, setup time, peak resources, latency, datastore sessions, amplification, and cleanup. Local loopback qualification does not establish cross-host production capacity. Fix demonstrated blockers, rerun the affected point, and attempt the target within measured machine resources.

## Cache

Exercise actual authenticated QUIC peer transfer with bounded RAM plus disk, backing-store counters, concurrent misses, peer loss/restart, stale discovery, integrity failure, and disk capacity pressure. Verify scope isolation and durable acknowledgment while measuring reduced backing reads. Reuse existing coverage where it already establishes the contract. Negative tests must detect real faults and ensure termination/cleanup.

## Delivery

Use meaningful red/green regressions and end-to-end tests; run touched formatting, strict Clippy, workspace checks, and applicable native/live-service qualification. Retain reproducible artifacts and independently review task changes and full branch. Submit a follow-up PR. Report any unmet throughput or deployment qualification explicitly; a test harness alone is not a successful 10,000-client result.
