# Cache stage metrics

## Purpose

Measure the current cache path before changing its locking or storage behavior.
The retained failure report showed an overlapping PUT and GET exhausting their
transport budgets despite independently valid peer disk bytes. Separate queue
wait from lookup and connection establishment so subsequent changes have a
measured target.

## Contract

Append six fixed storage rows at 85 through 90: distributed miss admission,
singleflight acquisition, RAM lookup, disk lookup, peer connection lock
acquisition and outbound connection establishment. Append RAM and disk hit-byte
counters at core indexes 134 and 135. Preserve every existing index. Final banks
are 91 storage and 136 core rows. Use the existing allocation-free recorder
primitives and bounded fixed-field slow logs; no identifiers enter labels.

Keep all cache, mutex, authentication, deadline, persistence and fallback
semantics. Finish each queue span immediately upon acquisition. Pending drop is
cancelled. RAM probes include misses; hit counters count Some, including empty
bytes. Disk timing includes the existing bounded helper and its verification;
collapsed worker/read errors remain successful misses. Establishment includes
the existing outbound handshake, identity check and slot installation; live
connection reuse has no establishment span.

## Delivery

- [x] Add isolated exact tests first and observe semantic failures after full
  behavior/byte oracles on the current producers.
- [x] Instrument the existing boundaries, append banks and update exact
  exporters/consumer controls. Historical missing rows stay unavailable;
  native-addon audited instrumentation remains 78/85 because it does not
  depend on the blob-cache crate.
- [x] Verify cold coalescing, warm RAM/disk, empty hits, admission/singleflight/
  disk cancellation, real mTLS cold/reused connections, held mutex cancellation,
  overlapping blackhole PUT/GET and rejected identity.
- [x] Run warmed public recorder allocation gates, cache ready-future profile
  controls, affected consumer tests, formatting and strict Clippy.
- [x] Independently review source and actual evidence and retain a report.

Publication uses the existing PR. Hosted CI and merge remain separate gates.

## Limits

Durations are inclusive wall time, not exclusive CPU or wire latency. Disk
lookups do not count physical IOPS. Hit-byte counters do not measure allocation
churn. Warm recorder allocation controls exclude initialization, snapshots,
logging, returned payloads and async state. No production reconnect fix,
throughput improvement or full-capacity claim follows from adding observers.
