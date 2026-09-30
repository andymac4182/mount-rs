# TiDB/RustFS guarded autocommit measurement

These are three ordered, short controls on the same retained local backend.
They measure ten QUIC server processes, ten clients, ten separate Drives, five
Partitions and 100 files per Drive. Filesystem metadata uses TiDB 8.5.7 and blobs
use RustFS; the authorization catalog uses SQLite. Each server eagerly opens all
ten Drives in this harness, giving 100 coordinator and block-store replicas.
This measurement does not use the subsequently implemented lazy CLI runtime.

## Whole-fleet logical workload cycles per second

| All-active pattern | Original two-SELECT RR (`73afa304`) | Joined SELECT RR (`083c8291`) | Guarded autocommit (`52c1cdee`) |
| --- | ---: | ---: | ---: |
| sequential_read | 363.85 | 325.26 | 736.68 |
| random_read | 386.99 | 297.95 | 818.83 |
| sequential_overwrite | 170.87 | 118.61 | 201.03 |
| random_overwrite | 174.91 | 120.90 | 194.06 |
| mixed | 236.69 | 213.77 | 331.26 |
| hot_file | 196.51 | 102.27 | 238.33 |
| append_truncate | 210.67 | 71.57 | 258.63 |
| churn | 91.91 | 56.50 | 104.39 |

A read or overwrite cycle performs one 4 KiB data operation plus open and close.
Append/truncate and churn cycles contain additional metadata work. These are
aggregate logical rates across the fleet, not per-Drive rates or physical IOPS.
The [complete comparison](comparison.md) reports all 16 pattern/mode cells and
its [JSON](comparison.json) retains separate data RPC, metadata RPC and total
acknowledgment rates. Active clocks exclude metric observers and idle liveness.
The first two controls and their exact shared middle run are retained in the
[previous report](../tidb-rustfs-joined-read-20260928/README.md).

All 16 cycle rates improved relative to joined RR. Relative to the original
control, random reads improved about 112% and random overwrites about 11%; the
larger 175%/61% gains against joined RR include recovery from its regression.
Each read still performs five compact loads and five SELECTs. Five explicit
transaction starts and five rollbacks per read are removed. Selected-load
inclusive wall time falls from 27.07 to 9.25 ms per random-read cycle. Pool and
local gate waits remain small in this separate-Drive, depth-one workload.

Churn also improved despite using none of the changed selected-load calls.
The ordered controls therefore do not isolate all throughput gains from backend
cache/history, scheduling or other environmental changes. No repeat-run
confidence interval is available. Inclusive async spans overlap and must not be
summed as exclusive CPU or latency components.

## Correctness and provenance

The candidate is a clean committed release build at `52c1cdee`, bound to the
Cargo-emitted executable, source inventory, worker digests and terminal receipt.
Nineteen actual TiDB compact tests passed, including single-statement consistency,
authority/corruption checks, unknown-commit acknowledgment behavior and a scoped
session-reuse/cancellation control. Full snapshot and publication transactions
retain their previous behavior. All 16 workload stages completed with no workload
or cleanup error. Both fresh backend-context passes verified every Drive's
membership, full bytes and EOF; final payload bytes differ between controls
because append cycle counts differ. Scope and revocation denials also passed.
These backend reopen controls do not verify every server replica's local cache.

## Backing-server observations and limits

[Server counters](server-counters.md) and their [JSON](server-counters.json) retain
independently checked whole-window SQL and matched-label TiKV-client deltas.
Their 333.178-second window includes substantial background time, including
163.616 seconds after terminal update. RPC coverage is partial because one new
histogram pair has no baseline. Zero covered retry/backoff deltas and unverified
lock-stage counters do not establish absence of contention or pure lock-wait
time. The observer validates four metric roles before/after; it supplies no
post-capture PD/RustFS identity witness. These counters cannot be normalized into
per-phase amplification or physical NVMe IOPS.

No allocation counts, physical disk IOPS, clean causal isolation, production
capacity, 10,000-client qualification, native mounted CLI end-to-end behavior,
power-loss durability, or current hosted CI success is established here.
