# Server blob cache

The server cache decorates the existing immutable block storage providers. A cache miss checks local RAM, local disk, eligible QUIC peers and finally the backing provider. Metadata remains authoritative for drive access and file references. Cache keys include the cluster, partition, drive and verified backing identity; equal block IDs from different drives do not share entries.

The local disk cache currently requires Linux or macOS descriptor-relative filesystem operations. Cache construction on unsupported platforms returns `ENOTSUP` before creating its directory.

## Discovery strategies

Strategies are compiled into the server and selected through configuration. Applications can implement the discovery trait to add integrations without changing cache admission, transport or durability rules.

| Strategy | Location lookup | Strength | Cost / failure behavior |
| --- | --- | --- | --- |
| Deterministic owners | Hash block scope onto configured members; select an owner and replica | Small predictable fanout and no external directory | Placement traffic and uneven hot keys; membership changes cause cold misses until new owners warm |
| Bounded peer query | Query a configured number of eligible peers | Simple deployment and no directory dependency | Miss fanout increases peer requests; strict bounds trade hit rate for latency |
| Directory | Ask Redis for expiring holder hints | Find opportunistically cached blocks without querying every peer | Extra lookup and advertisement traffic; stale hints are ordinary peer misses, outages fall back |

For mostly separate sandbox drives, deterministic ownership gives predictable traffic across ten servers. Bounded queries suit small clusters where replicas are already warm. A directory is useful when accesses move between servers and opportunistic holders are hard to predict. A directory does not increase cache capacity or guarantee a hit.

## Trust and durability

Peer traffic uses a separate QUIC endpoint with mutual TLS. Configured certificate identities and partition permissions determine eligible peers. Directory results only select configured peers; they cannot introduce arbitrary network destinations. Peer requests serve local cache entries and never fetch backing storage or forward requests.

Writes are stored in backing storage first. Cache admission and peer placement occur only after the backing flush barrier succeeds. Cache disk files are expendable copies, not a durability tier. Losing every cache node cannot lose an acknowledged backing write. Migration reads bypass the cache.

Content identity depends on the provider. TiDB and current object-store IDs carry SHA256 digests; FoundationDB uses its own SHA256 prefix. SQLite IDs are opaque and PgLite has a different digest convention. Disk and transfer integrity checks detect corruption, while opaque IDs require trusting the authenticated cache server, just as backing storage is trusted.

RAM, disk, entry counts, block sizes, peer requests and maintenance queues have explicit limits shared across drives. Eviction and failed advertisements affect hit rate; they do not change backing authority. Directory advertisements expire so crashes and lost withdrawals leave bounded stale hints.

## Qualification

The focused gates include real mTLS QUIC reads/writes, denied partition access, rejected leaf pins/client CAs, Redis hint expiry and injection checks, native Redis TLS/auth through an owned local fixture, corruption/eviction/restart checks and SQLite filesystem reopen through the SDK decorator. The previous client-scaling results remain in [remote-client-scaling.md](remote-client-scaling.md). Cache hit tests do not establish the production 10,000-client capacity target, datastore durability under power loss, or cross-host network performance.

The workspace gate passed **1,289 tests across 157 targets**, with 106 explicitly ignored fixtures/platform tests; strict workspace Clippy and formatting also passed. The final cache suite after review corrections passed 29 tests, with its two Redis fixtures ignored in the standard gate. Independent specification and quality review passed with no open material findings. Both explicit real Redis fixtures also passed, including native TLS/auth and certificate validation.

## Service configuration

Add `cache` to the existing version-1 service JSON. All paths are resolved relative to that file. This example is node 1 in a two-node cluster; every node lists the other nodes, with the same cluster ID and membership. Replace the sample certificate pin with SHA256 of node 2's DER leaf certificate.

```json
{
  "version": 1,
  "catalog": "catalog.sqlite",
  "listen": "0.0.0.0:4433",
  "certificate": "client-service.pem",
  "private_key": "client-service-key.pem",
  "max_connections": 1024,
  "cache": {
    "cluster": "production-east",
    "node_id": "node-1",
    "disk_path": "blob-cache",
    "ram_bytes": 268435456,
    "disk_bytes": 10737418240,
    "max_entries": 100000,
    "max_blob_bytes": 1048576,
    "max_inflight": 64,
    "deadline_ms": 500,
    "peer_query_limit": 3,
    "maintenance_capacity": 64,
    "placement_concurrency": 4,
    "hedge_delay_ms": 25,
    "peer_transfer_bytes": 134217728,
    "peer_listen": "0.0.0.0:4434",
    "ca_certificate": "peer-ca.pem",
    "certificate": "node-1.pem",
    "private_key": "node-1-key.pem",
    "discovery": "deterministic",
    "peers": [{
      "id": "node-2",
      "address": "192.0.2.2:4434",
      "server_name": "node-2.internal",
      "certificate_sha256": "0000000000000000000000000000000000000000000000000000000000000000",
      "partitions": ["sandboxes"]
    }]
  }
}
```

The disk path must be a dedicated directory. The server uses an ownership marker, exclusive lock, private permissions and relative no-follow file opens; unrelated directories are rejected. Disk entries survive a process restart and are verified on read. Disk budget includes the checksum header. RAM limits count retained payload bytes; shared pending admission and maintenance payloads have a separate staging budget of `max(ram_bytes, max_blob_bytes)`, including a per-entry bookkeeping charge. Returned caller buffers and bounded metadata/task bookkeeping are additional memory, so these values are not a whole-process RSS cap.

The cache is used for split-storage drives with `concurrent_writes` enabled, which establishes a verified immutable backing identity. Ordinary memory, host and monolithic SQLite drivers keep their existing mounting behavior. An undersized block limit skips cache admission for larger backing blobs. The entry limit also bounds registered drive scopes; exhausted scope registration falls back to uncached backing access.

Peer certificates need server and client authentication usage, a DNS SAN matching the configured server name, and a trusted cluster CA. Every peer uses a unique certificate pin. Partition allowlists restrict both incoming requests and outgoing queries. Membership and trust are explicit configuration; restart nodes to change them. Ownership changes or certificate rotation can temporarily reduce cache hits; backing reads remain available.

For bounded query discovery set `"discovery": "peer-query"`. For a Redis directory set `"discovery": "directory"` and add:

```json
"redis": {
  "address": "redis.internal:6379",
  "namespace": "mount-rs-production-east",
  "username": "mount-cache",
  "password_file": "redis-password.txt",
  "ttl_ms": 30000,
  "tls": {
    "server_name": "redis.internal",
    "ca_certificate": "redis-ca.pem"
  }
}
```

The directory uses a persistent connection and batches configured holder and node-lease reads with [Redis MGET](https://redis.io/docs/latest/commands/mget/). Hints and leases use [SET with PX expiry](https://redis.io/docs/latest/commands/set/); hot cache hits renew advertisements asynchronously with per-entry rate limits. Remote directories require native TLS; cleartext is allowed only for an explicit loopback socket. Use a standalone Redis endpoint or a compatible proxy; Redis Cluster redirection is not implemented. Credentials are read from a bounded file and omitted from errors. Give its ACL only the relevant key namespace and GET/MGET/SET/DEL operations. The directory stores IDs, not blob data.

Writes batch pending admission until the existing backing flush. After that barrier, RAM admission is immediate and disk fills run in bounded background workers; write acknowledgement does not wait for optional disk admission. Background placement and advertising are best effort: full queues drop hints, and shutdown discards unfinished maintenance after the durable stores close. Blocking disk workers retain their permits and staging charges through cancellation; shutdown waits at most two seconds for them. Administrative delete/reconcile operations still wait for local invalidation and are outside the measured write acknowledgement path. Shutdown logs per-drive local/peer hits, backing fetches, bytes saved and dropped maintenance. `Discovery` and `PeerTransport` are public Rust traits for additional compiled-in implementations.


The production `checked_buffer_reservation` arithmetic used by shared staging admission was verified with Kani 0.68.0 / CBMC 6.11.0: **0 of 74 failed checks, 3 of 3 covers satisfied**. It uses arbitrary full-width 64-bit `usize` inputs and verifies exact acceptance, no overflow, positive charge for empty payloads, and the budget bound. Run `scripts/verify-cache-formal`; this does not prove QUIC/TLS, Redis, filesystem failure semantics, atomic scheduling, or backing durability.

The isolated 4KiB RAM-hit allocator profile served **100,000 reads with zero backing GETs**. Both the uncached synthetic memory provider and warm cache perform **2 allocations/read** under the existing boxed-future/`Vec` contract. Separating the ready hit future from the cold-path future reduced cached allocated bytes from **4,656 to 4,200/read** (the provider baseline is 4,112). The extra 88 bytes are future/result representation, not another allocation. The final run observed about 2.27 million warm reads/s on this laptop (the preceding run was about 2.75 million); this is a component microbenchmark, excludes metadata/RPC/network/storage, and is not evidence of end-to-end 100,000 IOPS or production capacity. Run `cargo-shared run -p mount-rs-blob-cache --example cache_profile --release --offline --locked`.

Run `CARGO_TARGET_DIR=/path/to/isolated-target scripts/test-cache-redis.py` for the explicit plain directory and TLS/auth tests. The script starts only isolated loopback Redis processes, generates fixture credentials, and terminates owned processes on success or failure. Standard Cargo tests ignore these two fixtures so development does not require Redis installed.

## QUIC transfer and scheduling bounds

Peer requests use separately owned headers and shared immutable payloads. Successful GET replies read the status separately and retain the received payload without a slicing copy. The ordinary block provider still returns a `Vec`, so it requires a caller copy. Deterministic ranking hashes the scope once and evaluates each peer once.

Placement runs at most `placement_concurrency` jobs (default 4, maximum 32), with at most two replica PUTs per job. The maintenance queue and pending-byte reservation remain bounded. Reads start one peer query, then hedge after `hedge_delay_ms` (default 25 ms, clamped to one quarter of the overall deadline); at most two queries run simultaneously and `peer_query_limit` bounds total attempts. The first valid response wins and cancels outstanding queries. Directory hints and individual corrupt/missing replicas remain best effort.

`peer_transfer_bytes` defaults to 128 MiB and is split equally between incoming and outgoing work. Each admitted transfer reserves twice the maximum blob plus header bound before allocating a body, and queued QUIC payload owners retain their charges. The budget must fit one such reservation in each half and cannot exceed 1 GiB. Cache RAM, staging, transport receive windows, provider buffers and bookkeeping are separate limits; this is not a whole-process RSS cap.

## Authenticated failure qualification

`crates/mount-rs-blob-cache/tests/distributed_failure.rs` composes real pinned
mTLS QUIC peers, `CachedBlockStore`, bounded RAM/disk, and a counted backing
store. It checks exact binary bytes through cold, RAM, disk-only, peer, and
100 simultaneous miss paths; the real peer response is held until every cold
reader has entered, and disabling the miss lock makes the exact-one peer-request
oracle fail with 100 requests. The suite stops and restarts a peer at the same socket
with persisted disk entries; and forces stale hints, checksum corruption and
budget eviction. The repeated 28-byte fixture performs 125 logical reads
with two backing GETs (56 bytes), including the forced outage fallback.
This measures backing-read savings for that fixture, not production throughput.

The suite also checks identical opaque IDs in distinct registered Drive,
cluster and backing scopes, rejected unregistered sibling scopes and forbidden
Partitions with no denied PUT side effects, and a controlled successful/failed
backing flush before real peer placement. Peer trust grants Partitions; exact
scope registration determines cache availability. Client per-Drive OIDC grants
are enforced separately by the service. A split SQLite SDK write is acknowledged,
all cache/peer owners are closed, and a fresh undecorated SDK reopens the exact
persisted file bytes. Cancellation checks owned directory removal and UDP socket
reuse. Tests have 15-second outer bounds and explicit cleanup; cache workers are
drained before invalidation so a late fill cannot hide a peer failure.

Run `scripts/cargo-shared test -p mount-rs-blob-cache --all-targets --locked`.
These are loopback qualification tests. Capacity eviction is not OS ENOSPC;
the synthetic flush barrier is not power-loss testing; the SQLite check is an
orderly fresh reopen; and Redis's separate live fixtures do not compose Redis
with the real-peer failure suite. Cross-host and production capacity remain
separate qualification work.
