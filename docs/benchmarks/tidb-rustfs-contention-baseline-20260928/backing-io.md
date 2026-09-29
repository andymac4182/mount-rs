# Remote ten-server control: backing-server counter window

Observed partial. The top-level before/after interval is **478.418783 seconds**. Source intervals below differ slightly because captures were sequential. This delayed window includes accepted control startup, work, oracles, cleanup, observer overhead, additional idle time and backing-server background work. It is not exclusive active workload timing.

All four exact guest execs settled with exit code 0. No observed counter reset or device-series change. No logical operation denominator is asserted. Physical NVMe IOPS and actual TiDB queue/lock/pool-wait evidence remain unavailable.

## TiKV: Linux container cgroup block accounting

All three `/data` mounts are ext4 on Linux `254:1`; the exposed cgroup counter rows are Linux device `254:0`. These are separate source cgroups. Retain these per-source rows; do not sum device/partition stacks or rates. Counts include whole-container and background activity, and do not identify a Drive or database path.

| Source | Source interval (s) | Read IOs | Write IOs | Read bytes | Write bytes | Read IOs/s | Write IOs/s | Write bytes/s |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| tikv1 | 478.419478292 | 1 | 9,188 | 4,096 | 82,231,296 | 0.002090 | 19.204904 | 171881.162 |
| tikv2 | 478.419907125 | 0 | 9,110 | 0 | 82,059,264 | 0.000000 | 19.041850 | 171521.425 |
| tikv3 | 478.422525084 | 0 | 14,385 | 0 | 93,302,784 | 0.000000 | 30.067564 | 195021.721 |

Zero read deltas describe explicitly exposed unchanged cgroup counters only; they do not establish cold-read efficiency, absent backing activity, physical disk saturation, or a contention cause. These are Linux cgroup IO counts, not physical disk request counts or logical Drive IOPS.

## RustFS: host-forwarded data path

Actual `/data` is **virtiofs**, Linux mount device `0:46`. Its object data path is host-forwarded and is not quantified by guest block-device accounting. The RustFS container exposed `254:0` counters with 0 read IOs, 2 write IOs and 8,192 write bytes over 478.424075458 seconds. Those observed guest counters are retained as partial accounting; they do not measure RustFS object-data I/O or imply that writes were cheap.

## Process I/O counters: separate Linux process accounting

`syscr`/`syscw` are process read/write syscall accounting; byte counters are Linux process I/O accounting. They are not physical device IOPS or logical Drive operations. No syscall count is substituted for a block count.

| Source | rchar | wchar | syscr | syscw | read_bytes | write_bytes | cancelled_write_bytes |
|---|---:|---:|---:|---:|---:|---:|---:|
| tikv1 | 35,567,825 | 62,860,885 | 344,435 | 19,353 | 4,096 | 82,202,624 | 0 |
| tikv2 | 35,602,696 | 62,667,838 | 345,225 | 20,077 | 0 | 82,034,688 | 0 |
| tikv3 | 36,675,898 | 63,811,018 | 432,507 | 109,747 | 0 | 93,270,016 | 0 |
| rustfs | 207,135,072 | 53,530,570 | 89,178 | 178,048 | 0 | 31,173,875 | 0 |

## Container eth0: separate network accounting

These counters include application traffic, replication, observer and background traffic. They are container-interface measurements, not physical network-device measurements. Different source windows are preserved; no aggregate network rate is asserted.

| Source | RX bytes | RX packets | TX bytes | TX packets |
|---|---:|---:|---:|---:|
| tikv1 | 62,523,504 | 52,911 | 7,185,547 | 53,449 |
| tikv2 | 62,590,184 | 51,987 | 5,901,841 | 52,640 |
| tikv3 | 74,777,456 | 177,641 | 310,475,086 | 159,663 |
| rustfs | 27,446,110 | 39,836 | 23,463,327 | 34,173 |

## Evidence and validation

File-only reads and independently checked scalar delta/rate arithmetic. No new observer, process, network request, runtime inspection or source edit was performed. Scalar JSON contains only approved source roles, numeric counters/windows, filesystem/device scope, status fields and evidence hashes; no original CIDs, labels, names, host mount paths or raw counter file content.

- Accepted end-retry receipt SHA: `882f0f37ea7b90fb4b8d016abda578f8b0696b016c4432b1502ea255f658e23b`.
- Begin snapshot SHA: `4c18500588e9341a0c54f6710fd68e39ed347f104dd79c4945a42fe433d9a171`.
- End snapshot SHA: `44f00465d39e3ee33b1359dbc38895b28d085e08247a682d18a4143725052a2c`.
- Scalar JSON SHA: `fba9503e79fb196d7c4237b3731742555d9e438a27082cea23a32e44d75f829c`.

No wait measurements in these files establish TiDB connection-pool, SQL lock, transaction retry or server queue contention. No allocation or throughput conclusions are derived from this enclosing counter interval.
