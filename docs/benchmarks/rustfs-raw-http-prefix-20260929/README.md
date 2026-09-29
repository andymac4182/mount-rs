# Direct RustFS HTTP and prefix breadth release comparison

These three completed local release runs measure public RustFS `BlockStore`
calls at 1, 10 and 100 disjoint prefixes. Each run contains GET, unique PUT and
configured authority verification phases at concurrency 1, 10 and 100: 27
successful measured cells in total. The [comparison JSON](comparison.json)
retains the counts, integer nanosecond totals and derived rates.

All three runs use the same retained RustFS 1.0.0 container with pinned image
`rustfs/rustfs:1.0.0@sha256:8cc9801755448b71a786705ce76692c77e14936cccd87cf2fc31842e58f4d1ff`,
a two-CPU limit and a 2 GiB memory limit. Fresh health receipts for all three
runs match the same eight original fixture roles, lifecycles, limits and
mounts. RustFS `/data` uses a macOS host bind mount; guest write counters do
not cover its physical host writes. The memory cap differs from the older
1 GiB combined controls, so these rates do not provide a causal comparison
with those measurements.

The timed data path shares **one raw HTTP client across all prefixes**. Each
prefix also has a configured authority facade with four independently signed
clients, constructed outside the timed phases and retained during the run.
There are therefore 4, 40 or 400 live configured authority clients in addition
to the shared data client. Prefix count is not the only changing resource in
these controls. Including one released standalone probe per prefix, setup
constructs 5, 50 or 500 configured/probe clients in total.

Each data operation transfers 4 KiB. GET repeatedly reads the same total corpus
of 256 seeded blocks, or 1 MiB, distributed across the selected prefixes.
Provider cache capacity and observed cache hits are zero; RustFS and operating
system caches are not disabled. Unique PUT payloads are generated before the
timed phases. Raw data SDK retries and harness retries are zero. The separate
configured authority facade retains its existing bounded probe retry policy.

Each phase admits requests for five seconds. Its recorded elapsed time includes
settlement of work admitted before the deadline. Rates are successful logical
operations multiplied by 1,000,000,000 and divided by elapsed nanoseconds;
mean latency is summed request-timer spans divided by successful operations and
1,000,000. Nanosecond counters are parsed as integers before deriving these
values. Means include waits inside the measured call.

## GET results

| Concurrent requests | 1 prefix: operations/s | 1 prefix: mean ms | 10 prefixes: operations/s | 10 prefixes: mean ms | 100 prefixes: operations/s | 100 prefixes: mean ms |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 1,891.22 | 0.528 | 1,960.78 | 0.509 | 1,790.94 | 0.558 |
| 10 | 11,759.85 | 0.850 | 12,597.09 | 0.793 | 13,171.43 | 0.759 |
| 100 | 20,344.07 | 4.913 | 21,321.73 | 4.688 | 20,064.30 | 4.981 |

## Unique PUT results

| Concurrent requests | 1 prefix: operations/s | 1 prefix: mean ms | 10 prefixes: operations/s | 10 prefixes: mean ms | 100 prefixes: operations/s | 100 prefixes: mean ms |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 101.25 | 9.872 | 103.96 | 9.618 | 98.51 | 10.151 |
| 10 | 297.51 | 33.551 | 329.89 | 30.271 | 307.40 | 32.471 |
| 100 | 364.86 | 270.271 | 284.99 | 344.769 | 362.75 | 271.335 |

## Configured authority verification

These phases use the separate configured authority facades. Their operations
are authority verifications, not block data operations or TiDB queries.

| Concurrent requests | 1 prefix: verifications/s | 10 prefixes: verifications/s | 100 prefixes: verifications/s |
| ---: | ---: | ---: | ---: |
| 1 | 1,093.96 | 1,072.63 | 950.35 |
| 10 | 7,107.24 | 6,914.30 | 7,032.08 |
| 100 | 14,019.81 | 13,326.28 | 13,561.92 |

## HTTP timing and request counts

For every timed data cell, one successful logical GET or PUT corresponds to
one SDK call and one observed HTTP dispatch. Each dispatch returned a 2xx
response, with no transport error, cancellation before headers or response
body error. All GET response bodies reached EOF with exactly 4 KiB read per
successful operation. Successful PUT response bodies were dropped after
headers; the SDK does not consume their bodies. Offered PUT bytes are request
body lengths, not a measurement of bytes sent or physically committed.

The observer measures the interval from the first poll of the forwarded
request through return of response headers from awaited HTTP execution.
The table below compares its exact integer totals with the enclosing SDK
`put_opts` call at concurrency 100.

| Prefixes | Successful PUTs | Summed SDK call ns | Summed request-to-headers ns | Headers interval / SDK wall time | SDK wall time / request latency |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 1,886 | 509,661,193,923 | 509,598,809,327 | 99.987760% | 99.986360% |
| 10 | 1,488 | 512,961,461,886 | 512,905,617,779 | 99.989113% | 99.989206% |
| 100 | 1,877 | 509,219,487,052 | 509,139,982,013 | 99.984387% | 99.985105% |

About 99.99% of summed SDK PUT wall time at concurrency 100 lies inside
this request-to-headers interval in each prefix run. That interval includes
connection and transport waits, RustFS server work, storage waits and task
scheduling. It cannot identify the limiting resource or separate CPU time
from waiting. Concurrent intervals overlap; they are nested wall-time sums,
not exclusive latency or CPU components.

Measured local digest, block-ID encoding and upload-copy spans sum to
50,036,270, 36,693,810 and 49,270,496 ns at concurrency 100 for 1, 10 and 100
prefixes respectively: 26.530, 24.660 and 26.250 microseconds per PUT. They
are also wall intervals and do not establish exclusive CPU or all local work.

The observer counts one known forwarding future box and one known response
body box per dispatch. These are specific instrumentation allocations, not a
complete allocation count for the provider, SDK or transport.

## Completion and cleanup

All 27 cells have positive completed work, settled workers and complete worker
metrics. Returned failures, uncertain operations, adapter errors, cache hits
and harness retries are zero. No cell hit its payload capacity. Success
histograms match successful counts; failed and uncertain histograms are empty.
SDK and HTTP deltas conserve the logical data operation counts, with no metrics
saturation or remaining in-flight HTTP work at phase boundaries.

| Prefixes | Seed blocks | Successful timed PUTs | Exactly acknowledged and deleted blocks |
| ---: | ---: | ---: | ---: |
| 1 | 256 | 3,889 | 4,145 |
| 10 | 256 | 3,665 | 3,921 |
| 100 | 256 | 3,914 | 4,170 |

Cleanup deleted every exactly acknowledged seed and timed PUT block after a
fresh configured authority verification. Failed, uncertain and unattempted
deletes are zero. Authority markers are retained, no run is quarantined, and
no range deletion or reconciliation was used. All three named tests passed;
the owners recorded exit zero, no deadline or signals, reaped children and
absent process groups.

## Interpretation and limits

The controls ran in prefix order 1, 10 and 100 on the retained local fixture.
Each is a short single measurement with no repeat-run confidence interval.
Backend cache history, scheduling, observer overhead and the growing live
authority client population remain variables. Changes between these tables
cannot be attributed solely to prefix breadth.

These measurements describe direct logical provider calls. They exclude TiDB
metadata, the drive coordinator, QUIC and an operating system mounted CLI.
They do not establish physical NVMe IOPS, cold backend reads, power-loss
durability, long-duration capacity or 10,000-client qualification. The caller's
durability declaration does not qualify RustFS fsync or crash behavior.

The latest completed combined service measurement is the
[strict ten-drive control](../tidb-rustfs-strict-d10-20260929/README.md).
The historical [ten-drive guarded-autocommit report](../tidb-rustfs-autocommit-20260928/README.md)
measured
737–819 aggregate read cycles/s, 194–201 overwrite cycles/s and 331 mixed
cycles/s. A read or overwrite cycle includes one 4 KiB data operation plus
open and close. The [100-drive release run](../tidb-rustfs-d100-release-20260929/README.md)
failed during payload population and reached no sustained workload stages.
Neither the prefix controls nor that failed run qualify 10,000 clients.

## Provenance

All three runs use measured source commit
`5d3391ddb424686db94d421a30304f50b288ba76`, release executable SHA256
`29a2766c96d5133fbe02bbae161737bc0eabee85b3d2aace26ac4da939a32e24`
and bounded owner SHA256
`a0e4b56c6b1085f4bc28472d8f7788b90df2efbd14b57beae3ddcac3f23e3dfd`.
The configuration SHA256 is
`8af8118967d352e3d4065ec72521ff49473cd051da1875b00b8841b000e4748d`;
the offline derivation helper SHA256 is
`60a23b851ae8c0001e84d0de69e82c80b3e1b431eeabf9337af0d44bc68fe716`.
Each receipt records matching before/after inventories of the following 16
source files and unchanged executable bytes.

| Prefixes | Actual-run receipt SHA256 | Fresh health receipt SHA256 |
| ---: | --- | --- |
| 1 | `47355879c0cb155b8a017065cc27829234342f3b474fea0de33aa282eda1ad2d` | `e526138d4cac545650455188126252fd0543bff18a9313f1d15b5b429bb3d317` |
| 10 | `16a1cb4b96c35bac17055ed0449360d63d824c3f4646ca6414588e7b11d3c1b4` | `d5f3be3cb799280720f631a7b3f1c66bba5f84435cd6a63f059a3595a3be3550` |
| 100 | `79dc73337b9f24b708cd8dabe9b365428e884f57e985fc8d5258228a62013496` | `96b9d657ca1cb31898a44c2d573b5829f42ace714458f52f36fb43a5709c0745` |

| Inventoried source | SHA256 |
| --- | --- |
| `Cargo.lock` | `f79031a6bb5845d231ae339cc43a988f9bc5c0ca19f80b0a3fbcbb12294853f0` |
| `Cargo.toml` | `437c5a8dcab46be8ab2ebf17e6bb7c74fa84438a68b1636748bb70b82bc5b3e1` |
| `providers/mount-rs-object-store-blocks/Cargo.toml` | `b18f2ebe159842850fe79408aeb2d9b7635022ae1012680e19ac68c7d8d0efee` |
| `providers/mount-rs-object-store-blocks/src/lib.rs` | `f765a8c1fc8d39580c2b0236612c0026945f52b36e1fc040758004a9ced65aae` |
| `providers/mount-rs-object-store-blocks/src/qualification.rs` | `2225c98ba8dccc797b508d0262884a790d3a1173c0606605cb8a3f3bef87813d` |
| `providers/mount-rs-object-store-blocks/src/raw_metrics.rs` | `4d543a48c2a1ec38ad7a5835b032544dd36e2fea845b04eef7df9e0d20a874b0` |
| `providers/mount-rs-rustfs/Cargo.toml` | `254255050a2b212669e20bd4d4b843b287dbffe4d75df95f497a0f96acc8e7ee` |
| `providers/mount-rs-rustfs/src/http_observation.rs` | `8dff86404b4367f88ec1a938ac3f8ec02c6b0787b8748e7da12fa16b45d4f900` |
| `providers/mount-rs-rustfs/src/lib.rs` | `979061c61c39f630addeacc59e8cdb6b256b04328bcb6138a2f95b5604d95645` |
| `providers/mount-rs-rustfs/tests/raw_saturation.rs` | `f11bded3a0255f879abdc43b0b1a7df28515ea3b4fb280e48805830ca3b2ddfb` |
| `src/diagnostics.rs` | `04c0b91b8a4101f0194172e66fc9c635b095c901ec86d1a2564e46f3e2697db7` |
| `src/diagnostics/object_store.rs` | `ea9bd70a32cf02ebae349679492437f27c83cd422f5af7ad13e34b0fa3ec89bb` |
| `src/diagnostics/storage.rs` | `37f9c8e6d4bd4474b733379703ad43fb04f8359b9e64c9ced8f89c788717fd5c` |
| `src/lib.rs` | `8b7571250aca6ad94c52ba1197b8d897869b8cf110a112bd933eb5830550147d` |
| `src/storage.rs` | `b35945f3dba67af96174ce20e82488dabab8cf281c644a9b9cd043851d85828e` |
| `src/storage/compact.rs` | `af6fdf76f2a6a446240e85b20528ed3a64cbc76fa2251962eb6d7636c3c59d18` |
