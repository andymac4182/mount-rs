# Matched native controls: one and eight Drives

Two accepted short sequential controls; no capacity claim or attribution of the
whole rate difference to a mutex. Both use eight concurrent Node lanes, the same
verified Rust addon `bc5be094…`, 32 distinct changed 4098-byte contents, 32 partial
4096-byte writes and 32 verified full reads. Drive count also changes independent
TiDB pools, RustFS client instances, filesystem gates, prefixes and initial setup
objects (one versus eight). Counters enclose lane setup/open/close; the shorter
rate window excludes those operations and read verification. Inclusive spans
overlap and are not additive exclusive costs.

| Drives / workers | Logical operations/s | Timed ms | Write median ms | Read median ms | Gate wait sum ms | Pool checkout ms |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 / 8 | 139.789 | 457.833 | 65.121 | 48.652 | 2640.867 | 0.181 |
| 8 / 8 | 328.951 | 194.558 | 40.236 | 7.769 | 0.036 | 0.201 |

These are aggregate logical operation rates across the eight lanes, not per-Drive
rates or physical IOPS. Each cell has 64 timed operations, one native Node process
and eight shared backend containers. Native counter windows are 748.690 ms and
539.811 ms respectively; the addon exposes 136 core rows and 116 declared storage
rows. The comparison retains selected scalar rows, not raw namespaces or identities.

Each native window records 64 ordinary RustFS adapter gets and 64 adapter cache
hits, plus 40 logical puts. One Drive records seven recovered conditional-create
conflicts and verification GETs; eight Drives records none. Both record 72 marker
probe GETs plus 72 marker data GETs. Raw block-store adapter request metrics exclude
these marker requests. HTTP attempts and internal successful retries are unavailable;
ordinary adapter cache hits therefore do not imply an absence of backend traffic.
Retry-exhausted and terminal adapter-error counts are zero within these windows.

Enclosing backend container totals are descriptive and include background work:

| Metric | One Drive | Eight Drives | Coverage |
| --- | ---: | ---: | --- |
| CPU ns | 755442000 | 816762000 | all eight containers |
| Block bytes | 1056768 | 1110016 | all eight containers; no physical attribution |
| Network RX bytes | 1764541 | 1551918 | all eight container interfaces |
| Network TX bytes | 1964191 | 1673331 | all eight container interfaces |
| Block operations | unavailable | unavailable | all eight members missing; not zero |

Container interfaces can count the same virtual/client/server traffic more than
once. These backend totals are not divided by the shorter timed operation window.
Native and JS allocation counts are unavailable. Caller durability assertions and
flush/reopen checks do not establish crash, power-loss or production qualification.

The JSON contains parent, result and root acceptance hashes, the shared 745-input
source-manifest hash, addon/helper/runner/config/ready pins, instance counts, selected
gate/pool/storage/raw-request scalars and explicit resource coverage. File-only review
verified both parent/result pins, the actual addon file hash, equal source manifests,
shared helper/private-source/config/ready pins, eight lane task counts, instance-set
joins and backend aggregate joins. No provider/runtime/network/ledger/repository work.
