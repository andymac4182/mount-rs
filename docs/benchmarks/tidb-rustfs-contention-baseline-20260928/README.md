# TiDB + RustFS contention baseline

The first complete remote control passed on source revision
`73afa304cd6a3939cd8695960a8db4e93d9b1c80`: ten server processes, ten clients,
ten Drives, five Partitions and 100 files per Drive. All sixteen idle/active
pattern stages completed, followed by two fresh byte verification passes.
Partition, sibling-Drive and revoked-grant denials passed. All ten workers
exited successfully and were reaped; the parent confirmed group absence and
pipe EOF, with unchanged source and no unresolved ownership observations.

The fixture used three PD and three TiKV processes, one TiDB process and one
RustFS process in a Docker VM with 16 GiB configured RAM and fourteen CPUs.
Native preflight reported 16,745,295,872 VM memory bytes, about 15.6 GiB.
Native SQL membership and the owned RustFS preflight passed. Definitions and
grants used the existing SQLite service catalog; filesystem metadata used
TiDB and immutable blobs used RustFS. Each server opened every Drive, giving
100 worker filesystem/RustFS client instances. This does not qualify a
catalog shared across hosts.

## Remote performance

Rates below are successful ordinary read/write RPCs per second **for the
whole fleet**. Open/close and other metadata/control RPCs are counted
separately. Each active stage requested one second; actual elapsed active
time supplies its denominator. Boundary observation and idle liveness are
excluded. These are short diagnostic windows, not sustained capacity tests.

| Pattern | One active client, nine idle | Ten active clients |
| --- | ---: | ---: |
| Sequential read | 71.00 | 363.85 |
| Random read | 79.42 | 386.99 |
| Sequential overwrite | 39.49 | 170.87 |
| Random overwrite | 40.48 | 174.91 |
| Mixed | 52.78 | 236.69 |
| Hot file | 45.98 | 196.51 |
| Append/truncate, ordinary writes only | 24.58 | 108.23 |

Churn uses metadata operations: 67.05/s with one active client and 367.65/s
with ten. Random reads produced 1,160.98/s total acknowledgements including
open and close, versus 386.99/s actual read RPCs. Those totals must not be
reported as read IOPS.

[Remote evidence](remote.json) retains all sixteen cells, source and binary
hashes, paired metric hashes, selected counters and their scopes. Independent
review matched all 352 terminal-pinned metric files and all acknowledgement,
cycle and rate joins. Rust allocator profiling was disabled; whole-operation
allocation counts are unavailable.

## Repeated metadata work

The all-active random-read stage completed 394 open/read/close cycles and
1,970 compact loads: **five compact loads per cycle**. Each load issued an
anchor SELECT and a selected-guard SELECT inside a read transaction, giving
ten SELECT calls plus ten begin/rollback calls per cycle. These are recorded
database API actions, not wire packets or physical disk operations.

Compact-load elapsed spans totalled 8,577.880 ms across the concurrent
workers; enclosing refresh spans totalled 9,233.682 ms. Pool checkout totalled
2.395 ms and selected filesystem gate waits totalled 0.334 ms in this window.
Async spans overlap and cannot be added as exclusive costs. The measurements
identify repeated metadata round trips as a concrete optimization target;
they do not establish TiDB server lock or queue contention.

A proposed single joined SELECT inside the existing repeatable-read
transaction would reduce the recorded actions from twenty to fifteen per
read cycle. It is not implemented or measured in this baseline. Transaction,
authority, freshness, uncertainty and byte verification guarantees must be
retained.

## Matched shared and separate Drive controls

Two native controls kept eight workers, 32 distinct changed contents and 64
timed logical read/write operations in both cells. Both passed canary,
full-byte/EOF, shutdown, two fresh reopen, removal and empty-root checks on
the same owned durable fixture and exact native addon.

| Drives | Logical read/write operations/s | Median write (ms) | Median read (ms) | Summed gate wait (ms) |
| --- | ---: | ---: | ---: | ---: |
| One shared Drive | 139.789 | 65.121 | 48.652 | 2,640.867 |
| Eight separate Drives | 328.951 | 40.236 | 7.769 | 0.036 |

These rates are aggregates across the eight lanes. The short sequential
controls show a 2.35-fold rate difference under this workload; Drive count
also changes independent TiDB pools, RustFS clients, gates, prefixes and
setup objects. This does not isolate a mutex or establish horizontal
production capacity. Pool checkout totalled 0.181/0.201 ms respectively.
Inclusive gate and storage counters enclose lane setup/open/close and cannot
be divided by the shorter timed-operation interval as exclusive costs.

All 64 ordinary blob gets in each native window hit the adapter cache.
Both windows also recorded 144 backing-marker GETs. Cache hits do not imply
zero backing traffic. Seven recovered conditional-create conflicts occurred
in the shared-Drive window and none in the separate-Drive window; setup
objects are part of those counter windows. Raw adapter metrics exclude
marker requests, and HTTP attempts remain unavailable.

[Paired tables](native-pair.md) and [scalar evidence](native-pair.json) retain
the exact addon and evidence hashes, matching 745-input source manifests,
selected counters and backend resource coverage. The addon was built before
the remote selector increment; both cells used the same verified binary.
These native rates use different operations and geometry from the QUIC
remote controls and cannot be compared directly.

## Backing I/O coverage

[Backing tables](backing-io.md) and [scalar evidence](backing-io.json) retain
the individual TiKV container cgroup block counts and separate process and
network counters. The final capture was delayed by a sandbox socket refusal,
so its approximately 478-second interval includes startup, all workload
stages, oracles, cleanup, observer work, extra idle time and background work.
There is no complete logical-operation denominator for that enclosing
interval and no per-operation block amplification claim.

TiKV data mounts used ext4, with exposed whole-container Linux VM device
accounting. RustFS data used VirtioFS; its blob data path is not covered by
guest block-device accounting. Physical macOS NVMe IOPS remain unavailable.
The counts do not establish saturation of the laptop's disk.

The distributed BlobCache decorator was not configured. Direct RustFS
client cache/raw-object statistics and HTTP attempts were unavailable in
this remote harness. Frontend CPU observations exclude backend containers.
No 10,000-client, 1,000-file-per-Drive, cross-host, crash or power-loss
qualification is claimed.

## Follow-up validation

The owned-fixture selector test now uses a native absolute path on Windows
and retains its original Unix path. The validator is unchanged. This fixes
a confirmed cross-platform fixture defect; it does not identify the cause
of other hosted CI failures.

| Local gate after the fixture correction | Result | Receipt SHA256 |
| --- | --- | --- |
| Selector test suite | 9 passed, 0 failed | `57a56615733f47d59c083582b13246d513c892bfedebff08ab60addd7e677440` |
| Formatting | exit 0 | `6b3a545034677f2e7642957b807df43a7cd158022442ef90258d56abf53150e9` |
| Strict service Clippy, all targets | exit 0 | `64d8c0bd3eb54b0ce56fec9f3aae9551c649fe2037315291b8d55e04eded4999` |

These checks ran on macOS, with source unchanged within each owned run and
successful original-child reap, group absence and pipe EOF. Native Windows
execution and current-head hosted CI remain separate gates. The PR remains
draft and unmerged while CI failures are unresolved.
