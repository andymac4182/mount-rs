# QUIC protocol and transfer optimization

Client and server advertise only `mount-rs/2`. ALPN selection binds the codec and hello checks the same version; unsupported versions are rejected. Negotiation remains extensible for future versions. There is no v1 codec or numeric I/O fallback in the production client. The transport trait can use the same envelope for a future binary WebSocket adapter; fallback transport is not implemented.

## File I/O

A fixed 32-byte header carries kind, request ID, control length, raw payload length and result count. Typed handle read/write requests serialize small `IoRequest` metadata directly and transfer bytes separately. Responses read directly into the caller's buffer after validating ID, kind and length. All envelope fields and reserved bytes are checked before body allocation. Each request occupies one QUIC stream and FIN is required before dispatch. Unknown write outcomes close the connection and are never replayed. Authorization, grant revision, handle binding, catalog freshness and auditing share the existing dispatch implementation.

## Measured codec cost

Release `wire_profile`, uniform bytes 0..255, 1,000 iterations at 4 KiB. Preparation is excluded; the binary server read encode explicitly includes one provider-vector clone. Async in-memory sinks model codec work and exclude Quinn copying, TLS, scheduling, providers and network. Allocation instrumentation counts allocation/reallocation requests, not retained RSS. Numeric JSON is a diagnostic comparator, not a supported old client protocol.

| 4 KiB operation | Numeric JSON allocations / requested bytes | Binary allocations / requested bytes |
| --- | --- | --- |
| Read encode | 9 / 163,712 | 1 / 4,096 (provider vector clone included) |
| Read decode | 25 / 528,640 | 0 / 0 (preallocated caller buffer) |
| Write encode | 14 / 164,375 | 4 / 120 (borrowed payload) |
| Write decode | 31 / 529,559 | 3 / 4,161 |

Read replies are 4,128 bytes instead of the historical 14,680-byte numeric JSON frame (71.9% reduction); typed write requests are 4,180 instead of 14,766 bytes for the measured metadata. The retained profile includes 64 KiB and 1 MiB sizes. At 1 MiB, binary read decode still allocates zero while JSON requests 135,266,560 allocation bytes. This is cumulative allocator traffic, not peak memory.

## Admission boundaries

`RemoteServer::bind_with_transfer_limits` configures server-wide independent ingress and egress pools. Defaults are 1 GiB data and 64 MiB control in each direction, with 64 data and 32 control operations. Admission is fail fast before body allocation. Parsed generic data requests move from the reserved control lane to data admission. A per-connection cap admits at most 32 data operations after parsing headers; 40 transport streams provide control and Quinn stream-credit batching slack. This reserves capacity for normal saturated data workloads, not for a peer deliberately occupying every stream without sending headers. Response capacity is reserved before dispatch; owned queued response bytes hold the outbound charge until Quinn releases them. Generic JSON charges account conservatively for `Value` expansion.

Transport windows bound buffered but unconsumed stream data separately. Limits cover wire bodies, decoded control envelopes and queued responses; provider caches, provider-returned metadata, connection bookkeeping and transport windows are additional memory. These limits are not a whole-process RSS bound.

Peers use shared RAM payloads, separately owned headers, direct status/body receive and borrowed decoding. Transfer ownership retains quotas through queued chunks and noncancellable disk work. Placement and read hedging have explicit concurrency/deadline limits; see [distributed-blob-cache.md](distributed-blob-cache.md).

Evidence: [codec profile](benchmarks/remote-wire-20260925/codec-profile.txt).

## Matched real QUIC diagnostic

Two release runs used 100 continuously active clients, ten independent service coordinators, one shared SQLite drive, a 12.5 MiB randomized warm dataset, depth one, one-second warmup and five-second stages. Audit logging and process resource collection were enabled; allocator instrumentation was disabled. Payload generation was excluded equally; request construction/encoding and response content validation were timed. Both runs passed exact count/content checks and fresh driver reopen verification with zero request failures.

| Codec on current v2 server | Read IOPS | Write IOPS | Read p50 upper bound |
| --- | ---: | ---: | ---: |
| Numeric JSON diagnostic control lane | 6,227 | 339 | 16,384 us |
| Typed raw binary | 12,463 | 343 | 8,192 us |

The read result is 2.00 times the diagnostic baseline. Writes change only about 1% in this sample; the shared SQLite namespace/provider remains the dominant contention case. Client-side received QUIC bytes per read fall from about 15,073 to 4,238 bytes; sent bytes per write fall from about 15,154 to 4,290 bytes. Process CPU per read falls only slightly (about 889 to 852 us), with lower user CPU offset by higher system CPU and scheduling costs. Thus smaller wire payloads alone do not remove scheduling/network costs. These short loopback runs include client and server CPU in one process and are not a production capacity test. SQLite separate-drive topology is unsupported by this fixture; mostly separate production drives and 10,000 clients need separate TiDB qualification.

The pair used 33 transport streams per connection before the subsequent renewal-capacity review correction; depth one never approached that limit. Retained artifacts state the actual measured configuration. Current production adds per-connection data admission and transport credit slack.

Reproduce with `scripts/bench-remote-resources.sh sqlite`, an absolute `MOUNT_RS_RESOURCE_OUTPUT_DIR`, `MOUNT_RS_REMOTE_SATURATION_CODEC=numeric-json` or `binary`, and stage/warmup settings above. Artifacts: [numeric JSON](benchmarks/remote-wire-20260925/numeric-json/sqlite.json), [binary](benchmarks/remote-wire-20260925/binary/sqlite.json).

The resource artifacts also retain per-stage storage-call profiles, SQLite SQL counts and pager counters. OS block input/output counters were zero for these warm buffered stages, so SQL/pager operations are not physical device IOPS. Gate and catalog pool wait increase as the binary read lane pushes more requests; concurrency tuning and catalog/backing verification are remaining profiling targets.

SQLite counters reset at stage start: `sqlite_io_begin` is the sample **before** reset and must not be subtracted from `sqlite_io_end`; the end sample already measures the stage. The numeric/binary read stages performed 3 SQL statements, effectively zero pager misses (5 and 1 total respectively), and zero page writes per logical read. Writes performed about 17.85/18.19 SQL statements, 212.31/221.46 pager misses and 11.85/11.98 page writes per logical write. These buffered pager counts expose write amplification in this shared monolithic SQLite fixture; they do not measure device IOPS or split-storage production amplification.

## Qualification

- Workspace all-target test gate: **1,315 passed across 161 targets**, 106 ignored opt-in/platform fixtures; the run used serial fixture execution and local socket access after transient macOS HTTP address-allocation errors. The 12 HTTP fixtures also passed their focused serial rerun.
- Real Redis directory and native TLS/auth/certificate fixtures: both passed.
- Codec, signed OIDC grants/permissions, token renewal, uncertain committed-write nonreplay, cross-connection budgets, 32-data-operation renewal saturation, 7 MiB control streaming, 1 MiB raw I/O, peer trust/isolation and hedged replica/parallel placement regressions passed.
- Kani 0.68.0 / CBMC 6.11.0: frame length decisions **0 of 161 failed checks (1 unreachable), 3/3 covers**; control charge arithmetic **0 of 10 failed checks, 1/1 cover**. These full-width input proofs cover pure arithmetic/acceptance decisions, not the async allocator, transport/TLS, concurrency, backing persistence or provider durability. `scripts/verify-remote-formal` includes the new harnesses.
- Independent specification and quality review passed after correcting per-connection control capacity.

Generic control operations, including whole-file or guarded batch data where used, retain bounded JSON envelopes. They do not gain the typed handle I/O codec's allocation profile. Provider `Vec` buffers, request metadata, async futures and Quinn/TLS still allocate; the zero-allocation result applies specifically to successful binary read decoding into an existing caller buffer.
