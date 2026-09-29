# TiDB + RustFS: completed ten-client control

The latest completed control used source `2f3be9d7015a3053d4f5820e4afd0ce4a4e6d575`, ten QUIC server processes, ten clients, ten drives, five partitions, and 100 files per drive. The service catalog used SQLite; filesystem metadata used TiDB and blobs used a fresh RustFS named volume with authenticated strict bucket policy. Each pattern had a five-second active window, followed by settlement.

These are whole-cluster logical cycle rates. Read and overwrite cycles each transfer one 4 KiB block and include file open and close; mixed, hot-file, append/truncate and churn cycles contain multiple filesystem requests. They are not physical disk IOPS.

## Completed throughput

| Pattern | One active client | All ten clients active |
| --- | ---: | ---: |
| Sequential read | 124.892 cycles/s | 815.656 cycles/s |
| Random read | 124.255 cycles/s | 819.605 cycles/s |
| Sequential overwrite | 46.080 cycles/s | 214.431 cycles/s |
| Random overwrite | 48.826 cycles/s | 201.031 cycles/s |
| Mixed | 69.529 cycles/s | 320.656 cycles/s |
| Hot file | 51.629 cycles/s | 233.354 cycles/s |
| Append/truncate | 53.747 cycles/s | 149.365 cycles/s |
| Create/rename/unlink churn | 13.267 cycles/s | 43.013 cycles/s |

Rates use integer completed cycles divided by recorded active elapsed time, including settlement. Observer time is recorded separately. This is one short local service-harness run, without confidence intervals or a production capacity claim.

## Measured backing work

The following figures describe the cells with **all ten clients active**. Wall-time spans can overlap and are not exclusive CPU time or TiDB server lock-wait measurements.

- **Random read:** four selected SQL queries and four returned rows per cycle, returning 7,247.745 metadata JSON bytes. There are also two 20-byte backing-marker GETs per cycle. Mean SQL query time is 2.320468 ms; the mean warm SDK blob-get time is 8.664 microseconds.
- **Sequential overwrite:** seven SQL executions, one transaction begin and one commit per cycle, plus four marker GETs and one 4 KiB PUT. Mean commit time is 5.292720 ms. Mean SDK PUT time is 26.022626 ms; HTTP dispatch to response headers is 25.965579 ms. Selective inode serialization averages 455.112 bytes per cycle, with zero anchor serialization.
- **Random overwrite:** mean commit time is 5.491514 ms and mean SDK PUT time is 26.859639 ms; HTTP dispatch to response headers is 26.798277 ms.
- **Churn:** 26 SQL executions per cycle. Six inode-query calls return an average 479.392 rows, totaling 230,836.890 metadata JSON bytes per cycle. There are three commits per cycle, averaging 42.540049 ms, and twelve marker GETs. There are no blob PUTs in this pattern.

Observed CAS, mutation-conflict, rewrite and fallback counters were zero, as were selected storage error and cancellation counters. Local pool and coordinator gate waits were tiny for these separate drives. This evidence does not establish that TiDB has no internal contention.

Warm reads use the generic SDK adapter's bounded RAM cache. The distributed server RAM/disk cache, QUIC peer reads and discovery plugins were not configured in this combined control.

## Qualification and evidence

Both independent fresh backend oracles passed full expected-byte, membership and EOF checks. All 100 server/drive routes passed their existing Stat checks; those route checks do not themselves verify payload bytes. Scope and revocation denials, all ten workers' unforced exit and reaping, and cleanup passed. Actual RustFS telemetry covered the end of the benchmark; strict policy and sync-stage witnesses are not crash-durability qualification.

All 1,176 terminal-pinned compressed metric frames were rehashed and independently decoded. Their total size was 21,244,002 encoded bytes and 173,048,509 decoded bytes. All 16 result cells were independently recomputed from 320 before/after worker frames. Peak sampled retained capture was 38,400,984 bytes, below the unchanged capture limit.

[derived.json](derived.json) preserves exact counts and clocks, selected SQL/metadata/HTTP/CPU counters, source and artifact hash bindings, and qualification gates. Allocation instrumentation was disabled. Whole-host disk traces had substantial background activity and do not provide attributable physical IOPS or storage amplification. No performance gain is attributed to metric compression.

## Larger run

The subsequent 100-client/100-drive/50-partition run with 1,000 files per drive completed creation of its 100,000-file namespace. The enclosing Python supervisor then failed with `TimeoutError` during payload population, before sustained traffic or fresh oracles. Its receipt contains no operation context for that timeout, so it is not evidence that the native payload deadline expired or that TiDB contention caused the failure. Cleanup passed and the failed artifacts were retained.

There is **no qualified throughput result for that 100-client run**, and the 10,000-client production target remains unqualified.
