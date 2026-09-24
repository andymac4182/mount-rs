# Remote Drive saturation benchmark

This benchmark measures completed 4 KiB Drive reads and committed writes through
the production QUIC service. It uses 100 client sessions distributed across ten
server instances and ten independently opened filesystem coordinators sharing
one real TiDB metadata/block namespace. The instances run in one process on the
same host. Authentication uses the existing synthetic test identity; production
grant checks, framing, TLS, and request audit logging remain enabled.

## Run

```sh
# Default replicated PD/TiKV/TiDB topology; requires Docker with >=10 GiB RAM.
MOUNT_RS_REMOTE_TIDB_SATURATION_OUTPUT=/tmp/remote-drive-saturation.json \
  ./scripts/bench-remote-tidb.sh cluster > /tmp/remote-drive-saturation.log 2>&1

# Smaller local topology; does not qualify replicated durability.
MOUNT_RS_TIDB_TOPOLOGY=single \
MOUNT_RS_REMOTE_TIDB_SATURATION_DEPTHS=1,2 \
MOUNT_RS_REMOTE_TIDB_SATURATION_SECONDS=15 \
MOUNT_RS_REMOTE_TIDB_SATURATION_WARMUP_SECONDS=3 \
MOUNT_RS_REMOTE_TIDB_SATURATION_OUTPUT=/tmp/remote-drive-saturation.json \
  ./scripts/bench-remote-tidb.sh cluster > /tmp/remote-drive-saturation.log 2>&1

# Existing disposable real TiDB endpoint, supplied in the environment.
./scripts/bench-remote-tidb.sh existing
```

The runner uses an optimized release build. Cluster mode retains the provider
contract tests and owned-resource cleanup from `scripts/test-tidb.sh`. Existing
mode requires a disposable endpoint and retains its unique benchmark namespace.
`OUTPUT` contains the latest invocation, with an additional uniquely named
sibling JSON for each invocation. Cluster restart phases therefore retain
separate artifacts. The manual GitHub job is enabled with the `tidb_saturation`
workflow input and uploads logs and all JSON artifacts.
Redirecting stderr retains audit output without rendering every operation in a
terminal. Audit output still incurs its normal CPU and file I/O cost.

Settings (all bounded):

| Environment variable suffix after `MOUNT_RS_REMOTE_TIDB_SATURATION_` | Default | Bounds |
| --- | --- | --- |
| `DEPTHS` | `1,2,4,8` | 1–6 distinct depths, each 1–32 |
| `SECONDS` | 5 | 1–30 per measured stage |
| `WARMUP_SECONDS` | 1 | 1–10 per stage |
| `BLOCKS` | 64 | 32–1024 per client, at least maximum depth |
| `REQUEST_TIMEOUT_SECONDS` | 30 | 1–60; server also enforces its own deadline |
| `MIXED` | disabled | `1` adds alternating read/write stages |
| `MODES` | `read,write` | `read`, `write`, or both; no duplicate modes |
| `OUTPUT` | unset | JSON artifact path |

For a full ramp, set `MOUNT_RS_REMOTE_TIDB_SATURATION_DEPTHS=1,2,4,8,16,32`.
Use longer stages to confirm a plateau found in a short exploratory run.
Use `MODES=read` or `MODES=write` to probe each limit independently. An overloaded
write phase can otherwise stop the run before higher read depths are tested.
Larger `BLOCKS` values can hit the TiDB provider's default 4 MiB namespace limit;
the benchmark does not override provider limits to make a dataset fit.

## Measurement boundaries

Each phase opens handles before measurement and runs reads or writes separately.
Increasing per-client queue depth increases the number of outstanding requests.
Each lane has its own handle to avoid serializing requests on a handle lock.
Writes use disjoint block offsets within each client's file. All files still
share the TiDB namespace, exercising concurrent namespace publication. The
backend uses fixed 4 KiB chunks with concurrent writers enabled.
The timed interval includes draining requests admitted before the interval ends;
setup, warmup, opens, cleanup, and integrity verification are excluded. Errors,
partial I/O, and timeouts invalidate a run rather than inflating completed IOPS.
Writes are never retried after an ambiguous result. A fresh coordinator verifies
the final file contents after all servers and original coordinators close.
Timed reads validate the complete payload, and responses must end without
trailing bytes. Latency percentiles are power-of-two microsecond upper bounds,
rather than exact quantiles.

An overload or backend error exits nonzero. The artifact identifies the failed
phase, retains at most eight error samples with the full failure count, and
marks final verification as skipped. Successful earlier stages from that run
remain exploratory evidence. Repeat below the overload point to obtain a fully
verified result; never interpret a failed run as a successful acceptance gate.

Read results are cache-warm logical Drive IOPS. This benchmark is not a physical
SSD benchmark: QUIC, JSON framing, catalog reads, audit output, TiDB transactions,
namespace publication, Docker's VM, and filesystem caches all contribute. TiDB
may perform several storage operations for one Drive request. A laptop's stated
100,000 SSD IOPS is therefore a comparison target, not an expected service result.
At 4 KiB per operation, 100,000 IOPS represents 390.625 MiB/s of payload.

Use the throughput curve together with latency and resource observations. A
throughput plateau accompanied by increasing p95/p99 latency identifies the
useful concurrency limit for this configuration. It does not by itself prove
the SSD is saturated. Docker CPU, memory, network, and block-I/O observations
help separate service/SQL/VM limits from physical storage limits.

## Local qualification, 2026-09-24

The release build ran against real TiDB v8.5.7, one PD, one TiKV, and one TiDB
SQL frontend. Docker reported 14 CPUs and 8,318,976,000 bytes of memory, with
stable daemon identity throughout the qualified runs. The host is an M4 Pro
MacBook Pro with 48 GB RAM and an Apple AP1024Z SSD. These results qualify this
local single-node configuration, not replicated durability or physical SSD
IOPS. Hosted saturation CI is configured but has not run for this change.

The unique-data read/write run used 100 clients, ten server instances, 6,400
distinct 4 KiB seed blocks (25 MiB), 3-second warmups, and nominal 15-second
stages. Each measured stage had zero failures; all 100 final files passed fresh
namespace and content verification. The provider contracts and ambiguous-COMMIT
checks also passed, and the harness removed its owned resources.

| Total queue depth | Read IOPS | Write IOPS | Read p99 upper bound | Write p99 upper bound |
| --- | ---: | ---: | ---: | ---: |
| 100 | 54.62 | 11.57 | 4.194 s | 33.554 s |
| 200 | 75.60 | 12.28 | 4.194 s | 33.554 s |

IOPS divides completed operations by actual elapsed time including drain. For
example, the depth-200 write stage completed 359 writes in 29.237 seconds. Its
33.554-second histogram upper bound does not mean a request took exactly that
long. Request latency includes stream admission, response parsing, and content
validation; the server's 30-second dispatch deadline covers backend dispatch.

A separate read-only ramp used the same dataset and timing, allowing higher
read concurrency without a write overload stopping the run. All stages had
zero failures, and all 100 final files passed fresh verification.

| Total queue depth | Read IOPS | Read p99 upper bound |
| --- | ---: | ---: |
| 100 | 89.37 | 2.097 s |
| 200 | 69.70 | 8.389 s |
| 400 | 30.79 | 16.777 s |
| 800 | 117.44 | 8.389 s |
| 1,600 | 114.90 | 16.777 s |

The observed read plateau was about 117 IOPS: doubling queue depth from 800 to
1,600 reduced throughput by 2.2% and doubled the p99 histogram bound. The lower
points were nonmonotonic, so these short runs do not establish a stable latency
SLO or a precise optimal queue depth.

A separate write-only probe at total queue depth 400 passed warmup but failed
its measured stage: 455 writes completed, while 117 requests failed, including
66 client request timeouts. Its 12.52 completed writes/s is failed-phase
diagnostic evidence, not qualified throughput. Uncertain writes were never
replayed, cleanup completed, and final content verification was explicitly
skipped. The successful depth-200 result above remains the qualified write
measurement.

To reproduce the independent ramps, add `MODES=read` and
`DEPTHS=1,2,4,8,16`, or `MODES=write` and `DEPTHS=4`, using the full environment
variable prefix in the local command above. The write capacity probe is
expected to exit nonzero when it crosses the request deadline.

The read benchmark itself passed, but its subsequent provider fault gate
exposed a test fixture collision: two concurrent tests generated the same
PID/timestamp namespace key. A checked atomic sequence now makes those keys
unique even with equal timestamps; a regression test covers that case. Both
real ambiguous-COMMIT fault tests passed when rerun after the final write
capacity probe, using a wrapper that accepted only the recorded timeout
boundary and successful cleanup. This does not turn the overloaded benchmark
into a passing integrity test.

Before the conditional-load fix, the initial namespace occupied 866,917 bytes.
Source inspection showed two full namespace reloads per read in concurrent
mode, with TiDB using the default unconditional `load_if_changed`. At this
namespace size, that transferred
approximately 1.65 MiB of namespace text per 4 KiB read (423 times the payload),
before other SQL and wire costs. At 100,000 Drive reads/s it would imply roughly
161 GiB/s of namespace text alone. This identifies a structural amplification
to profile and reduce; it does not establish every runtime bottleneck.

An earlier repeated-block exploratory ramp reached read depths 100–800 and
write depths 100–400. It failed during depth-800 write warmup at the request
deadline, with metadata publication canceled before its outcome was known.
That run is partial overload evidence and skipped final verification; its
throughput figures are superseded by the unique-data qualification above.
An initial attempt also lost its Docker containers while reported daemon
capacity changed; it yielded no valid measured stages and was discarded.

## Conditional TiDB metadata loads

TiDB now overrides `load_if_changed` with a single fresh SQL statement that
returns the revision and a `CASE` expression. An exact nonzero revision match
returns SQL NULL for the namespace, avoiding its transfer over the SQL wire,
JSON decoding, and replacement of the coordinator's validated namespace.
Both filesystem consistency checks remain enabled. Revision and payload come
from the same statement snapshot, so a concurrent publication cannot pair a
new revision with an older payload. Every publication still increments the
revision using the existing conditional commit.

Revision zero and caller revisions outside signed BIGINT range use the full
load path. Missing rows and negative stored revisions remain errors. An
unchanged revision does not detect out-of-band edits that violate the provider
contract by changing namespace content without incrementing the revision;
`load` remains unconditional.

The real TiDB regression first failed with the former unconditional load,
then passed with the fix. It covers initialization, changed and unchanged
revisions, malformed JSON, oversized caller revisions, negative stored
revisions, and missing rows. The multi-coordinator provider tests additionally
exercise visibility and concurrent publication.

This removes the 423-times namespace transfer on reads whose metadata is
unchanged. It does not establish a physical TiKV read amplification ratio:
TiDB/TiKV may still fetch internal row data to evaluate the expression. A
changed revision still transfers the full namespace, so workloads with
continuous writes retain metadata publication and refresh costs.

See [the four-provider comparison](remote-provider-comparison.md) for the
matched SQLite, PGlite, FoundationDB, and TiDB workload and remaining costs.
