# Direct RustFS release measurement

This completed local control measures the public RustFS `BlockStore` provider
directly, without TiDB metadata, the drive coordinator or QUIC. RustFS ran in the
retained Docker fixture with a two-CPU limit and a 2 GiB memory limit. The release
executable was built from the current, uncommitted benchmark sources.

Each timed phase admits work for five seconds and includes settlement of admitted
requests in its reported elapsed time. Data operations use 4 KiB blocks. Raw
provider cache capacity is zero, cache hits are zero, and raw data SDK retries
and harness retries are zero. Reads repeatedly visit 256 previously seeded
blocks, a 1 MiB working set; RustFS and operating-system caches are not disabled.
Writes create unique blocks from payloads generated before the timed phases.

## Results

Rates divide successful logical operations by the recorded phase elapsed time.
Mean latency includes request queueing at the stated concurrency.

| Concurrent requests | GET operations/s | Mean GET latency (ms) | Unique PUT operations/s | Mean PUT latency (ms) |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 1,935.65 | 0.516 | 104.76 | 9.545 |
| 10 | 12,477.00 | 0.801 | 322.89 | 30.921 |
| 100 | 21,937.88 | 4.556 | 391.86 | 251.545 |

Separate configured-authority verification phases completed 1,124.49,
7,474.75 and 14,670.72 verifications/s at concurrency 1, 10 and 100 respectively.
They use a separate authority facade and are not data operations or TiDB queries.

All nine phases had positive completed work, settled workers, zero returned
errors, zero uncertain operations and no payload-cap limitation. The named
ignored test passed. Cleanup deleted all 4,430 exactly acknowledged blocks with
zero failed, uncertain or unattempted deletes. The backing authority marker was
retained; no range deletion or reconciliation was used.

## Interpretation and limits

Read throughput continues to rise with concurrency in this small, warm working
set. Unique write throughput rises only about 21% from concurrency 10 to 100,
while mean write latency increases from 30.9 to 251.5 ms. This establishes a
direct-provider write performance target to investigate. It does not identify
whether CPU, object layout, synchronization, storage or another resource causes
that flattening.

At concurrency 100, the 2,030 successful PUTs accumulated 510,635,867,071 ns
of harness request latency and 510,537,349,235 ns inside the awaited
`object_store.put_opts` call: 99.9807% of summed request latency. Local digest,
block-ID encoding and upload-copy spans average 30.52 microseconds per PUT.
These concurrent wall intervals do not identify exclusive CPU time. The SDK
await still includes SDK preparation, connection and transport waits, server
work and task scheduling. The direct benchmark client does not install the
configured authority client's HTTP observer, so its authority HTTP counters
cannot subdivide raw PUT latency.

These are short logical provider measurements, not physical NVMe IOPS, cold
storage throughput, a power-loss durability test or filesystem service capacity.
There are no repeat-run confidence intervals. They do not measure the complete
TiDB/RustFS combination or qualify 10,000 clients. The last completed service
results remain the [ten-drive report](../tidb-rustfs-autocommit-20260928/README.md);
the [100-drive run](../tidb-rustfs-d100-release-20260929/README.md) failed before
sustained workload stages. The fixture's memory limit differs from those older
measurements, so this is not a causal comparison with them.

Matched Docker CPU/memory/network and guest cgroup disk-request captures were
retained separately before and after this run. Their windows include setup,
seed operations, cleanup, observers and background work. They do not supply
phase-exclusive amplification or physical host disk IOPS.

The verified whole-window guest cgroup counters record zero RustFS write
requests despite the 4,430 acknowledged seed and unique-block PUTs. RustFS's
data directory uses a macOS host bind mount; the guest block-device accounting
does not cover its backing writes. Zero in this counter is not evidence of
zero storage writes. Host-side accounting or a separately controlled guest
volume is needed to measure backing write amplification.

## Provenance

- Benchmark source SHA256: `a3270217323e0977e77e6a91ac3cb197ab4434756146e38ecdb5f220b3265af7`.
- Release executable SHA256: `b8942d09f91b1969f35d9ed0da6126bfc69131f67c0a5b6ee1e747ef6e3b5046`.
- Private actual-run receipt SHA256: `3cc9aaf68c843ce15656481e5159dfffc885646aaa1b4939c470ec42c36ad5e5`.

The owner verified unchanged inventoried source and executable bytes, terminal
exit zero, child reaping and process-group absence. Root independently checked
the receipt, both log digests, the named passing test, nine exact phase cells,
histogram counts, zero errors/retries/cache hits and complete cleanup.
