# Distributed blob cache implementation

## Goal
Provide a bounded RAM and disk cache shared across server drives, authenticated QUIC peer reads, and compiled-in selectable discovery strategies: deterministic owners with a replica, bounded peer queries, and a directory with Redis support.

## Architecture
A new `mount-rs-blob-cache` crate decorates the existing immutable BlockStore contract. The SDK exposes an additive decoration hook; service configuration constructs one cache coordinator for all drives. Local mounting and storage providers retain their APIs. Cache scope includes cluster, partition, drive and verified backing identity. Discovery only supplies hints: the coordinator enforces trust, bounds and backing fallback.

## Global constraints
Backing writes and flush barriers remain authoritative. Migration bypasses caches. Providers have opaque or content-derived block IDs; no universal content hash assumption. Peers serve local cache entries only. Mutual TLS, configured certificate identities and partition allowlists restrict peer requests. No unbounded fanout, allocation, advertisement queues or disk growth. Redis is optional; outages fall back. Use the existing isolated checkout and `/private/tmp/mount-rs-remote-drive-target`; serialize Cargo gates. No deployment or push requested.

## Tasks

- [x] 1. Cache crate: scope types, integrity policies, bounded RAM/disk, single-flight, BlockStore wrapper and discovery trait; deterministic and query implementations. Test isolation, corruption, capacity, flush failure and backing read counts before implementation.
- [x] 2. Peer transport and Redis directory: bounded binary QUIC protocol, mTLS node pinning/partition checks, persistent connections and cache-only serving; expiring Redis holder hints with fixed membership validation and outage fallback. Real socket tests and negative trust tests.
- [x] 3. SDK/CLI integration: additive decorator entry point, shared service cache configuration and lifecycle, per-provider identity policy. Test ordinary mounts unchanged and configured cache service startup.
- [x] 4. Qualification and docs: focused and workspace checks, strict Clippy and formatting; controlled cache-hit/backing-GET experiments and security/failure cases. Document configuration, strategy tradeoffs and actual verification limits. Review immutable patch before completion.

## Progress
Base commit: 3d6b8b38. Design approved by user; compiled-in plugins explicitly confirmed.

## Results

Workspace: 1,289 passed, 106 ignored across 157 targets. Final focused cache gate after review corrections: 29 passed, two Redis fixtures ignored; both fixtures passed explicitly. Final workspace formatting and strict Clippy passed. Kani staging arithmetic: 0/74 failed checks, 3/3 covers. Independent final specification and quality review passed with no open material findings. The RAM profile served 100,000 reads with zero backing GETs, two allocations/read, and 4,200 allocated bytes/read. Production capacity and cross-host/power-loss behavior remain unqualified; see the cache documentation.
