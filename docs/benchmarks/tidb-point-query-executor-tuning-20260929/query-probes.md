# TiDB point-query executor tuning: isolated probes

## Change and scope

Commit `2fef4cf7d5fe639fd3a446f9a78c87d881d54713` adds two statement hints to `FILE_POINT_SQL`: `SET_VAR(tidb_max_chunk_size=32)` and `SET_VAR(tidb_executor_concurrency=1)`. This selected-file statement uses explicit primary-key bindings and selects at most one row from each joined primary key. Its predicates and selected columns are retained. The root-entry lookup, write-authority query and bulk reads have no such hints in the inspected source.

Four separately executed matched probes compared the prior FILE query with a correlated-volume rewrite, one worker, maximum chunk size 32, and the two settings combined. Each used one warmed owned TiDB v8.5.7 connection and 30 alternating prepared executions per arm, at 100 and 1,000 empty files. The first three probe builds captured provider revision `801ef633d01d7ef1dc95e0dc5a65b57ddb34ebed`; the combined probe captured the actual new commit and the prior provider snapshot as its comparison. These are short metadata-only measurements, not concurrent Drive throughput or independent replicated runs.

## Observed latency

Values are milliseconds for the individual prepared FILE query; percentages compare medians within each matched probe. The comparison value differs between probes, so the rows must not be pooled as repetitions of one experiment.

| Probe | Files | Median, comparison → candidate | Median change | Mean, comparison → candidate | p95, comparison → candidate |
|---|---:|---:|---:|---:|---:|
| Correlated volume | 100 | 0.960 → 1.346 | +40.2% | 0.972 → 1.407 | 1.148 → 1.704 |
| Correlated volume | 1,000 | 0.932 → 1.257 | +34.9% | 0.947 → 1.265 | 1.257 → 1.454 |
| One worker | 100 | 0.921 → 0.852 | -7.5% | 0.928 → 0.872 | 1.090 → 0.997 |
| One worker | 1,000 | 0.911 → 0.839 | -7.9% | 0.948 → 0.851 | 1.374 → 1.035 |
| Maximum chunk 32 | 100 | 0.950 → 0.948 | -0.2% | 0.990 → 1.001 | 1.173 → 1.293 |
| Maximum chunk 32 | 1,000 | 0.927 → 0.950 | +2.5% | 0.955 → 0.987 | 1.161 → 1.211 |
| Chunk 32 + one worker | 100 | 0.876 → 0.786 | -10.2% | 0.875 → 0.803 | 0.956 → 0.906 |
| Chunk 32 + one worker | 1,000 | 0.892 → 0.835 | -6.4% | 0.934 → 0.947 | 1.138 → 1.530 |

The correlation rewrite selected IndexJoin/MergeJoin and was slower in both populations. One worker reduced the median by about 8% without changing either displayed HashJoin memory value. Maximum chunk size 32 reduced tracked memory but gave little latency benefit: the 1,000-file median was 2.5% slower, with worse mean and p95.

The combined change had lower medians at both sizes. At 1,000 files, however, mean latency increased from 0.934 to 0.947 ms (1.4%) and p95 increased from 1.138 to 1.530 ms (34.5%). Thirty samples do not establish the tail distribution or a cluster-throughput win. The combined change requires the separate concurrent wire measurement before making that claim.

## Observed operator memory and worker count

Both populations reported the same memory displays for each configuration:

| Configuration | First join | Second join | Observed probe workers |
|---|---|---|---:|
| Comparison | HashJoin: 141.5 KB | HashJoin: 100.6 KB | 5 |
| Correlated volume | IndexJoin: 134.0 KB | MergeJoin: 7.07 KB | Different join executors |
| One worker | HashJoin: 141.5 KB | HashJoin: 100.6 KB | 1 |
| Maximum chunk 32 | HashJoin: 79.1 KB | HashJoin: 77.1 KB | 5 |
| Chunk 32 + one worker | HashJoin: 79.1 KB | HashJoin: 77.1 KB | 1 |

These are the individual `EXPLAIN ANALYZE` operators' tracked-memory strings. They are not cumulative allocated bytes, whole-process memory, or a measured request peak. Adding them does not establish a simultaneously live total. The empty-file row payloads also do not represent the nonempty 128 KiB files used by the wire workload.

## Source explanation and runtime boundary

The tagged v8.5.7 HashJoin build fetcher requests a chunk with both initial capacity and maximum set to the session's `MaxChunkSize` before obtaining rows. Changing only the initial chunk size therefore misses this build buffer. The default initial size is 32 and maximum is 1,024; lowering the maximum to its supported minimum, 32, directly reduces this reservation. Fixed-width column data and variable-width data, offsets and null bitmaps are reserved using the requested capacity. [Build fetch allocation](https://github.com/pingcap/tidb/blob/v8.5.7/pkg/executor/join/hash_join_base.go#L333), [allocator](https://github.com/pingcap/tidb/blob/v8.5.7/pkg/util/chunk/alloc.go#L93), [column reservations](https://github.com/pingcap/tidb/blob/v8.5.7/pkg/util/chunk/column.go#L123), [defaults](https://github.com/pingcap/tidb/blob/v8.5.7/pkg/sessionctx/variable/tidb_vars.go#L1392), [bounds](https://github.com/pingcap/tidb/blob/v8.5.7/pkg/sessionctx/variable/varsutil.go#L234).

The legacy implementation also creates 320 hash-map shards and an initial 64-entry store independently of probe concurrency. That is a remaining allocation candidate, not a measured attribution in these probes. The tag defaults `tidb_hash_join_version` to `legacy`, but the runtime artifacts did not witness that variable. The default alone cannot prove which implementation executed. [Shards](https://github.com/pingcap/tidb/blob/v8.5.7/pkg/executor/join/concurrent_map.go#L23), [entry store](https://github.com/pingcap/tidb/blob/v8.5.7/pkg/executor/join/hash_table_v1.go#L558), [version default](https://github.com/pingcap/tidb/blob/v8.5.7/pkg/sessionctx/variable/tidb_vars.go#L1675).

## Hint, cache and restoration evidence

The exact v8.5.7 whitelist explicitly verifies `tidb_init_chunk_size`, `tidb_max_chunk_size` and `tidb_executor_concurrency` for `SET_VAR`. A generic current documentation label does not override this tagged behavior. Cached plans retain statement hints, restore them on a hit, and reapply their settings before executor construction. `SET_VAR` does not inherently imply a prepared-cache miss. [Verified variables](https://github.com/pingcap/tidb/blob/v8.5.7/pkg/sessionctx/variable/setvar_affect.go#L78), [cached hints](https://github.com/pingcap/tidb/blob/v8.5.7/pkg/planner/core/plan_cache.go#L265), [hint application](https://github.com/pingcap/tidb/blob/v8.5.7/pkg/planner/optimize.go#L193), [executor settings](https://github.com/pingcap/tidb/blob/v8.5.7/pkg/executor/internal/exec/executor.go#L89).

Every measured SELECT arm recorded 30/30 immediate prepared-cache hits at each population. All four probes recorded equal typed hit/miss oracles and cleanup of all seven SQL families. The chunk-only and combined probes additionally recorded an explicit cached positive control and two forced-uncacheable negative controls, returning the same complete row with cache flags 1 and 0/0 respectively. The retained warning, `skip prepared plan-cache: not a SELECT/UPDATE/INSERT/DELETE/SET statement`, comes from the diagnostic EXPLAIN wrapper; it does not establish cache loss in the separately measured SELECT executions.

The combined probe's session witnesses before and after were identical: executor concurrency 5, maximum chunk size 1,024, autocommit enabled, pessimistic transactions and REPEATABLE-READ isolation. These witnesses occur after successful context resets. Tagged source restores previous hinted variables at the start of the next statement; neither source nor this probe establishes restoration at result EOF. [Context reset](https://github.com/pingcap/tidb/blob/v8.5.7/pkg/executor/select.go#L920).

## Tradeoffs and qualification

The intended improvement is less executor buffering and scheduling work for selected-file point queries. A smaller maximum chunk and one worker can reduce throughput for larger result sets, so the hints are scoped to that statement. They do not remove SQL round trips, change storage layout, prove lower physical I/O, or establish zero allocations. Actual Go allocation counters and the concurrent wire workload must determine the allocation and throughput effects. The mixed latency results above remain part of the evidence even if a later workload improves.
