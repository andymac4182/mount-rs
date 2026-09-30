# Existing TiDB/RustFS fixture resources — 2026-09-30

A bounded read-only observation sampled the eight existing backend containers
twice, over about ten seconds. No benchmark workload was injected. Background
and observer activity are included. Container identities, lifecycle,
configuration, volumes and network definitions matched before and after.

The three TiKV containers held **8,493,711,360 bytes (7.91 GiB) of anonymous
memory** at the second observations. All eight containers' sequential anonymous
sum was 10,647,543,808 bytes. Whole-guest available memory was
4,364,955,648 bytes (4.07 GiB), within a 15.60 GiB VM.

| Role | Anonymous memory, bytes |
| --- | ---: |
| PD1 | 170,680,320 |
| PD2 | 96,960,512 |
| PD3 | 142,778,368 |
| TiKV1 | 2,866,257,920 |
| TiKV2 | 2,851,045,376 |
| TiKV3 | 2,776,408,064 |
| TiDB | 643,420,160 |
| RustFS | 1,099,993,088 |

These observations identify current container memory ownership, not Rust heap
allocations or ownership of the earlier benchmark's growth. Sequential container
samples and whole-guest samples are not a simultaneous memory accounting cut.
The next workload must independently pass its existing admission check.

All eight byte-counter intervals were available and monotonic. Docker returned
`null` operation-count counters for every role, so **datastore IOPS remain
unavailable**. Missing counts are not zero. Device rows are retained individually;
layered device totals are not summed. These are guest cgroup counters, not
physical host SSD I/O or per-pattern amplification.

The first observer rejected the null counter shape. A separately reviewed
derivative preserves the fixed 60-second bound, 64 GiB host free floor,
five-second GET bounds and three-request concurrency; it explicitly reports
optional I/O availability separately from required memory/CPU/network completion.
The successful observation completed in 10.327 seconds, with the child reaped and
its process group absent.

[observations.json](observations.json) contains the closed-role observations,
CPU and network deltas, byte-counter device rows, availability flags and evidence
hashes. Raw container responses remain private.

No production capacity or new throughput result is established here.
