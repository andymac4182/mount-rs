# QUIC wire protocol optimization review

Reviewed 2026-09-25 against production source at `6baebcac`. This review adds a
reproducible diagnostic example; it does not change either production protocol.

## Priorities

| Priority | Surface | Change | Expected benefit | Compatibility / cost |
| --- | --- | --- | --- | --- |
| 1 | Client/server | Typed I/O headers and raw byte payloads; remove numeric JSON arrays and `Value` intermediates | About 72% fewer application bytes for uniform 4 KiB data, much lower allocator churn | Negotiate a new codec/version; support only the latest client version and retain the same authorization checks |
| 2 | Peer | Shared immutable payload buffers, separate header/payload writes, direct payload reads | Remove several complete payload copies without changing v1 bytes | Add shared-buffer internal APIs; retain existing `Vec` provider contract initially |
| 3 | Both | Explicit connection windows plus server-wide in-flight byte budgets | Predictable memory under many active clients | Tune to RTT/throughput; overly small windows cause stalls |
| 4 | Peer | Bounded concurrent placement; delayed hedged lookup | Avoid serial disk/peer latency and warm replicas faster | More instantaneous peer traffic; preserve total fanout, bytes and deadlines |
| 5 | Both | Bounded negotiated drive/scope handles and compact digest IDs | Fewer string allocations, hashes and header bytes | New version/control messages; generation and membership validation required |
| 6 | Both | Measure small-stream scheduling, batching and buffer reuse | Potential reductions in stream/task and datagram costs | Workload dependent; latency and fairness can worsen |

## Fresh codec measurements

Run:

```sh
CARGO_TARGET_DIR=/private/tmp/mount-rs-remote-drive-target \
  ./scripts/cargo-shared run -p mount-rs-remote-protocol \
  --example wire_profile --release --offline --locked
```

The example uses actual `Message`/`Operation` types and Serde codecs. Input
preparation is excluded. The data repeats bytes 0 through 255. Frames below
include the existing four-byte length prefix and use request ID 1 and drive
`sandbox-drive`. IDs and positions change header lengths in v1.

| Raw payload | Read frame | Write frame | Numeric `Value` array storage | Read encode allocation requests / bytes | Read decode allocation requests / bytes |
| --- | ---: | ---: | ---: | ---: | ---: |
| 4 KiB | 14,680 | 14,766 | 131,072 | 9 / 163,712 | 25 / 528,640 |
| 64 KiB | 234,040 | 234,126 | 2,097,152 | 13 / 2,621,312 | 33 / 8,454,400 |
| 1 MiB | 3,743,800 | 3,743,886 | 33,554,432 | 17 / 41,942,912 | 41 / 135,266,560 |

`Value` is 32 bytes in this build. Requested allocation bytes include
reallocations and transient intermediate representations; they are neither
peak live memory nor RSS. Decode includes the final conversion to `Vec<u8>`.
It excludes QUIC's receive body buffer, the caller-buffer copy, filesystem,
metadata, storage, networking and async task/future allocations. Encode
includes constructing the generic JSON value and serializing the message,
but excludes the original filesystem read buffer. Write encode/decode requests
for 4 KiB were 14 / 164,375 bytes and 31 / 529,559 bytes respectively.

Expansion depends on the data: 4 KiB all-zero reads produce 8,248-byte frames;
all-255 reads produce 16,440-byte frames. This is encoding overhead, independent
of data compressibility. A hypothetical raw response with a 32-byte header
would occupy 4,128 bytes, about 72% below the uniform-byte v1 read frame. That
header size is an illustrative design budget, not an implemented format.

At 100,000 aggregate 4 KiB reads/s, v1 application response frames alone would
carry about 1.468 GB/s (11.74 Gb/s); raw payloads alone carry 409.6 MB/s
(3.28 Gb/s). These are arithmetic bandwidth requirements, not measured capacity,
and exclude QUIC/IP/UDP overhead and requests.

Timing counters are emitted for reproduction, but the allocator instrumentation
affects timing, the large-payload sample is short, and this single run does not
establish a CPU speedup. Prior retained network observations in
[remote-resource-profile.md](remote-resource-profile.md) are consistent with
the encoding amplification, but belong to an earlier workload/source version.

## Client/server findings

### Remove generic payload representations

`remote-client/src/driver.rs` builds `json!({"data": chunk})`; the service's
`dispatch.rs` converts every number back to a byte. Reads go from a service
`Vec<u8>` through `serde_json::to_value`, message serialization, frame body
allocation, generic message decoding, `from_value::<Vec<u8>>`, then a copy into
the caller's read buffer. Keeping JSON while merely changing field names or
removing the four-byte prefix cannot address this cost.

Use typed read/write requests, typed success/error responses, and a separate
bounded raw payload. Feed reads directly into the caller's buffer after
validating response ID, status and declared length. Encode writes from the
existing input slice or a shared immutable buffer. A typed binary library must
encode bytes as a byte string rather than a sequence of integer values, and
must avoid preserving `Value` as an intermediate tree.

The same codec can serve QUIC and binary WebSocket messages. Transport fallback
must not replay an uncertain write. Negotiation should allow future versions
through ALPN/version and explicitly bind the selected codec; today only v2 is supported; do not guess the
codec after a parse error. A binary envelope with small JSON control metadata
is a possible staged migration, but leaves control-object allocations.

### Parse bounds before body allocation

The existing reader checks its 8 MiB frame limit before allocation, but only
dispatch checks the 1 MiB I/O limit, after generic JSON decoding. The writer
also serializes the complete body before checking the frame size. A typed
header allows operation-specific control and payload limits, and a byte permit,
to be checked before allocation, decoding or emitting bytes. The budget must
account for transient receive/send buffers and cancellation lifetimes.

### Keep concurrent request streams initially

Connections already persist; JWT authentication happens at hello/renewal, not
on every stream. A new bidirectional stream per RPC does not require a new TLS
handshake or an additional application handshake round trip. Streams provide
independent response/cancellation paths. Replacing them with one persistent
ordered RPC stream introduces cross-request blocking and a response-demultiplexing
queue. Measure stream/task costs after eliminating JSON before taking on that
change. If pooling streams is useful, use several bounded lanes and retain
request IDs, ordering rules and uncertain-write behavior.

## Server/server findings

### v1 is already compact on the wire

`blob-cache/src/peer.rs` sends one opcode, four u16-length-prefixed strings,
a 16-byte backing ID and optional raw payload. A request header is
`25 + cluster.len() + partition.len() + drive.len() + block_id.len()` bytes.
A hit response is one status byte plus the payload; a miss/PUT acknowledgement
is one byte. For `production-east` / `sandbox` / `sandbox-drive` and a 65-byte
SHA256 block ID, a GET header is 125 bytes and a 4 KiB hit response 4,097 bytes.
There is no JSON payload expansion to fix here.

### Remove full payload copies before changing peer bytes

Current hot peer GET path includes these source-visible copies:

1. RAM `Arc<[u8]>` into the `Vec` returned by `get_memory`.
2. That payload into a status-prefixed response `Vec`.
3. Slice writes into Quinn's owned send chunks.
4. Quinn `read_to_end` chunks into a contiguous receive `Vec`.
5. `response[1..].to_vec()` into the returned block.
6. The returned block into a new cache `Arc` on admission.

This list is source reasoning, not a measured memcpy/CPU attribution. QUIC
encryption and packet construction impose additional work; shared payloads do
not imply an entirely zero-copy network path. The PUT path likewise appends
the source payload to an encoded request, copies it from the decoded frame,
then copies again into cache ownership.

The fixed discovery plugin also recomputes `scope.digest()` and formats hex
inside its `sort_by_key` ranking closure. Precompute that exact digest once per
lookup and evaluate each peer rank once; retain the existing hash inputs so
placement does not silently change. This is a smaller CPU/allocation win outside
serialization that requires no wire change.

First expose an immutable shared cache read accessor. Send the status/header
and owned payload with [Quinn chunk writes](https://docs.rs/quinn/0.11.12/quinn/struct.SendStream.html#method.write_all_chunks),
which accept cheaply cloned `Bytes`; wrapping an existing `Vec` transfers
ownership, whereas constructing `Bytes` from a borrowed slice still copies.
Avoid assembling a second full response frame. On GET receive, read status
separately and receive the body directly into the returned buffer, removing
the slicing copy. Fixed-capacity header encoding removes reallocations without
a protocol change. The decoder can borrow text and validate trailing length
before creating owned strings/payloads.

[Chunk reads](https://docs.rs/quinn/0.11.12/quinn/struct.RecvStream.html#method.read_chunk)
can retain received buffers without an immediate copy, but chunk boundaries
are unrelated to sender writes. A contiguous `Vec` API still requires assembly
when data arrives in multiple chunks. Compare ordered direct reads against the
existing unordered `read_to_end`: the latter tolerates reordered arrivals and
can win under loss. Do not assume fewer copies always improves WAN latency.

### Separate cache admission from optional disk latency

Peer PUT currently awaits `cache.insert`, including optional disk work, before
returning its acknowledgement. Backing durability is already established by
the sending server. A new response contract can distinguish RAM-admitted,
queued and rejected placement; a successful holder advertisement should follow
actual admission. Admit verified data into RAM immediately, enqueue bounded
disk work, and acknowledge cache admission without waiting for disk. Preserve
the worker-owned byte/IO guards until noncancellable work actually ends.

### Reduce sequential latency with explicit bounds

`CachedBlockStore` probes eligible peers sequentially under one overall deadline;
one blackholed owner can consume the whole budget and hide a healthy replica.
Try the owner first, then issue at most one delayed hedge, retaining the existing
total peer/query and byte limits. Measure extra traffic versus p95/p99 latency
under miss, stale hint, slow peer and packet loss workloads. Do not broadcast
unconditionally.

The maintenance worker also serializes blocks and their owner/replica PUTs.
Use a small bounded set of placement tasks, bounded by the existing transport
and staging budgets, and perform owner/replica placement concurrently when the
budget permits. Keep heartbeat/advertising responsive. Batch only small
placement/control items with maximum bytes/count and a short latency bound;
large payload batches retain memory and delay unrelated reads.

### Compact identities only after copy work

A bounded connection-local scope handle can replace repeated cluster/partition/
drive/backing strings, and an algorithm tag plus raw digest can replace SHA256
hex text. Bind handles to the exact registered scope, backing identity and
authenticated peer partition permissions, invalidate them on connection/scope
changes, and keep opaque provider IDs distinct. A bare global block hash is
insufficient for tenant isolation. This requires a new peer version, adds
control/setup work, and saves much less bandwidth than the client binary codec.

## Transport tuning for 10,000 clients

Neither transport explicitly sets stream/connection receive or send windows.
The locked `quinn-proto` 0.11.18 defaults are a 1,250,000-byte stream receive
window, `VarInt::MAX` connection receive window and a 10,000,000-byte send window.
Those limits are not preallocated RSS, but a 10 MB send allowance for each of
1,000 clients on a server exposes up to 10 GB of retained application send
bytes before other memory. The service's per-connection 32-operation semaphore
is also not a server-wide decoded-body memory budget.

Set explicit asymmetric profiles: many sandbox client connections should have
modest per-connection budgets and a shared server transfer budget; a small
number of peer links can have larger windows appropriate to aggregate traffic.
Choose receive windows from expected bandwidth × RTT with a measured margin,
and reserve room for control/renewal and small requests. Quinn explicitly
documents these [window/memory tradeoffs](https://docs.rs/quinn/0.11.12/quinn/struct.TransportConfig.html#method.send_window).
Shared `Bytes` slices may retain an allocation larger than the logical slice;
budget retained ownership, not merely declared payload length.

Benchmark `send_fairness(false)` for small streams and a small number of priority
classes for control/foreground reads versus placement. Quinn documents reduced
fragmentation as a possible benefit, but disabling fairness can worsen mixed
large/small request latency. Keep path MTU discovery; raising the minimum MTU
for sandbox networks with VPNs/tunnels is not a general optimization.

`write_frame` calls `flush`, but Quinn's `AsyncWrite::poll_flush` immediately
returns success. Removing it is not a saved network RTT. The peers already
pool connections and both protocols already multiplex streams; their handshake
cost is primarily startup/reconnection. Do not introduce mutation 0-RTT or
unreliable datagram I/O as a shortcut to throughput.

## Suggested implementation sequence and acceptance evidence

1. Remove peer frame/slicing copies and unnecessary header allocations while
   preserving v1 bytes. Retain negative trust, partition, corruption, truncated
   frame and cancellation tests.
2. Add negotiated typed client I/O with raw payloads and direct caller-buffer
   reads, retaining negotiation for future versions and a shared codec for future binary WebSocket transports. Assert
   exact round trips, length rejection before allocation, permissions, renewal,
   frame version binding and no write replay after uncertain completion.
3. Add global transfer-byte admission and explicit transport windows, then
   measure many idle clients separately from continuously active clients.
4. Add bounded placement concurrency/hedging and compare single peer, healthy
   replica, stale directory, blackholed peer and lossy/reordered links.
5. Only then compare scope handles, stream reuse, batching, scheduling and
   optional compression with matched workload and allocation-disabled controls.

For each step retain application bytes, UDP bytes/datagrams, allocations and
requested/live bytes, CPU, RSS, peer/origin GETs, dropped placement, latency
percentiles and error counts. Exercise 4 KiB/64 KiB/1 MiB random, zero and
compressible data, both directions, mixed workloads and fresh connections.
No optimized throughput or production capacity is established by this review.

The diagnostic example's release run and round-trip assertion passed. The
protocol crate's all-target gate passed its five tests, strict all-target
Clippy passed, and workspace formatting passed. Production code is unchanged;
this review did not repeat the full workspace or datastore load suites.
