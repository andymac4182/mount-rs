# Live service metrics and construction controls

The CLI can emit periodic diagnostics while serving requests. Enable profiling
and explicitly select a 1–60 second interval; see the
[quick start](../../bottleneck-quickstart.md). Shutdown records keep their v2
shape. Periodic records share a sequence, capture time and process bank snapshot
across the two listeners.

## Retained validation

The [report](report.json) records commands, source hashes and terminal receipts.

| Gate | Result |
| --- | --- |
| Touched default tests | 876 passed; 211 ignored |
| Touched profiled tests | 909 passed; 211 ignored |
| Strict Clippy, default and profiled | Passed |
| Workspace formatting | Passed |
| Startup filter | 50 passed |
| Signed configured CLI | Full payload/EOF, denials, live records, clean stop and disabled restart passed |

Test totals are executions, including repeated controls. Ignored external
backend and native cases are not qualification evidence.

The actual CLI capture occurred before SIGINT. It contained 22 stages per
transport, 110 shared storage rows and 136 shared core rows. Both transports had
completed read/write observations. Backing block put/get byte observations were
98,320 bytes each. These cumulative values are not a throughput or physical IOPS
measurement. The disabled restart emitted no service diagnostic records.

The original signed test failed because completed I/O produced no periodic
records. Its expected failure was reported after graceful stop and joined pipe
readers. The unit missing-API compile baseline is recorded separately.

Construction controls cover 11 SDK journal cases, 17 actual SQLite authority
cases, four PGlite client cases, six TiDB client cases and 17 startup cases.
They support the [construction ownership seam](../construction-ownership-controls-20260928.md).
PGlite/TiDB protocol peers do not establish live backend SQL behavior. Journal
models do not establish actual SDK filesystem handoff.

Warmed runtime leases add zero allocations with observation disabled or enabled.
The handle holder adds no I/O boxes; underlying async trait calls still allocate
32/48-byte futures. Cold construction, provider/RPC/metadata work, snapshots and
close are outside this allocation control.

## Remaining work

Ordinary SDK opening still needs journal composition and CLI postconfiguration
ownership. The CLI/process fixture remains eager; lazy startup v2 is a library
contract. AWS source freezing, upstream SlateDB construction ownership and
FoundationDB integration remain open.

Native mounts, live backend comparisons, crash/power-loss and new symbolic
qualification are separate gates. The ten-server, 10,000-client/Drive,
5,000-Partition, 1,000-file-per-Drive target was not attempted here.
