# Owned cache and WebSocket failure qualification

## Scope

Complete the Redis-directory/real-QUIC composition and durable WebSocket lost-reply controls. Keep the production authenticator, negotiated protocol, dispatcher, durable driver and cache implementations in the path. Retain backend/device and production-capacity limits separately.

## Required observations

- Redis composition: healthy holder, stale holder, peer unavailable, directory unavailable with persisted peer disk data, and combined directory/peer failure. Distinct cold blocks and a fallback selecting the requester make directory discovery decisive. Compare complete bytes and exact peer/backing GET deltas in every phase.
- Redis cleanup: authenticated readiness of an owned credentialed process; cancellation observes child reap before directory removal. Runtime cancellation retains directories when cleanup is unknown.
- WebSocket: signed OIDC and trusted TLS, correct negotiated version, cross-partition/read-only denial, an actual completed durable write response intercepted before any response frame reaches the client, uncertain caller result, no replay/reconnect/QUIC switch, and complete fresh SQLite metadata/bytes/EOF.
- Cleanup: no fixture directory can be removed while server, cache or child ownership is unknown. Whole-process containment is separate from graceful fixture cleanup.

## Implementation and validation

- [x] Correct RedisFixture and Pair asynchronous Drop directory ownership; add explicit reap completion to cancellation control.
- [x] Retain the WebSocket fixture directory synchronously before async work. The private gate owns removal after process termination; a timed-out close wrapper does not prove the actual server stopped.
- [x] Derive and independently review a fault-only bounded parent. Pin the leader through owned-group TERM/KILL decisions, then observe reap, output EOF and group absence. Pin the explicit Redis executable before/after. Containment does not replace in-test cleanup assertions.
- [x] Run the exact two Redis controls and durable WebSocket control serially with frozen source, fixed commands and no automatic retries. Preserve failures.
- [x] Run affected normal suites, formatting and strict Clippy. Independently review source and actual receipts, including exact executed names and full-byte oracles.
- [ ] Wire portable CI prerequisites and exact selectors; publish the tested slice through the existing PR, retaining hosted-CI and merge status separately.

## Limits

The cache backing fixture counts BlockStore calls and bytes using a synthetic memory implementation; its durability flag does not prove real backing durability or physical IOPS. WebSocket persistence uses actual SQLite through the public driver but does not simulate power loss. Loopback tests do not establish cross-host latency or 10,000-client capacity. The full 10-million-file target remains unchanged and requires a host with sufficient disk, memory and CPU.

## Local result

The [qualification report](../../benchmarks/cache-websocket-failures-20260927/README.md)
records seven final serial gates, 81 ordinary passes, three exact failure-case
passes, scoped strict Clippy and formatting. All final runs used the same 441
frozen inputs. Controlled write-persistence omission fails the pre-close SQLite
oracle; restored code passes. Redis composition verifies five complete reads,
two peer hits and three memory backing GETs / 2,348 bytes.

Initial compilation failure was corrected; Darwin zombie-only signal handling
was diagnosed with two owned probes and XNU source; actual earlier failures
remain retained. Diagnostic logging reproduced an overlapping replica PUT and
recovery GET exhausting their 400 ms transport budgets despite valid peer disk
bytes. Explicit actual placement completion barriers qualify separate fault
phases. Production connection-establishment contention remains unresolved.

CI source wiring is independently reviewed. Publication, hosted execution,
complete CI/formal results and merge remain separate gates; this plan does not
mark the overall active goal complete.
