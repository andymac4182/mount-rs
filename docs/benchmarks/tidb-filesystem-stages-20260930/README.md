# TiDB + filesystem: stage instrumentation and recovery follow-up

The latest completed performance result remains the [16-cell filesystem run](../tidb-filesystem-flush-20260930/README.md) at `bf6062cb4fc0b3dcbfde4532bba1b8c46266ab2f`: **621.863 sequential read cycles/s** and **77.026 sequential overwrite cycles/s**, aggregate across ten servers and ten active clients. These are complete Open/I/O/Close cycles, not physical IOPS. Replacing RustFS has not demonstrated a write-speed improvement.

## Current instrumented attempt: excluded

A new optimized release at `2c4d284a7a8f785f409e376bd8e833f1b898669b` includes the internal filesystem stage counters. Its archived executable passed 180 ordinary checks; six native selectors were ignored and the controller was subsequently invoked explicitly. The attempted configuration retained ten servers, ten clients, ten Drives, five Partitions and 1,000 files per Drive, with all sixteen required workload cells.

The attempt stopped after **37.491 seconds**, during `online_namespace`, when the sampled free disk fell to **68,713,459,712 bytes**, below the unchanged **68,719,476,736-byte (64 GiB)** reserve. All ten clients had connected, but there were **no completed timed cells or fresh full-content oracle passes**. Namespace creation precedes payload population; the retained partial worker checkpoint reports no block PUTs. No internal PUT-stage latency, throughput, backend comparison or physical IOPS result follows from this attempt.

The supervisor sent a cooperative interrupt, reaped its owner, observed the owner group absent and closed capture pipes without a forced stop. The owner reports its direct benchmark child reaped and benchmark group absent. A subsequent exact process check found all recorded owner, controller and worker PIDs/groups absent. That later check establishes current absence; it does not retroactively prove each worker's historical reaping. All eight original services and six volumes retained their checked identities. The private filesystem root, authority marker and partial data were retained. The supervisor continues to mark this attempt `performance_eligible=false`.

The partial worker checkpoints do not yet carry the new filesystem stage bank; phase-boundary receipts do. This limits diagnostics when a run stops between boundaries. The free-space decline was a whole-host observation and is not attributed to TiDB, filesystem payloads or any single process.

| Retained evidence | SHA-256 |
|---|---|
| Immutable executable | `71c45254d35fb460371868dc8aa5061e4b7a8d4a2c965e887ed24f3c02bbea9a` |
| Build/source manifest | `2daa6f63cb6a075b4e62f5177649e842777045a410d3f84f6b8bda7554b8fb02` |
| External supervisor receipt | `955d892015aff48848b7c80daad0be04ee5389e0174d17cebd3ffaf694668261` |
| Owner receipt | `f7b1d7e5020d578c2d27e4fc8a48374718d738ba65c8577b26583ebd1edf605d` |
| Incomplete terminal | `b1b053f6528342127889ba106292805f9ac17090805c9bf27a6a6bc0d15c6039` |

## Linux recovery acceptance: passed

The [Linux TiDB job](https://github.com/andymac4182/mount-rs/actions/runs/36690286909/job/109805616674) ran at PR head `acdeb498efcbe15b57b9e20557428010ff205fa7`, using merge checkout `c97d48fad4b178866b39452a2020780a087026a5`. This is a separate debug integration result, not the current release benchmark.

The actual SDK test `actual_tidb_filesystem_durable_peer_writes_reopen_and_root_authority` passed once before and once after recreating the TiDB frontend and restarting one TiKV member and one PD member. Its seed and retained-reopen markers confirm full-byte verification, filesystem backing authority and zero SQL block rows. The Rust CLI `sdk-self-test --reopen` also passed in both phases. These are executed selectors with captured pass markers, not an inference from overall job success. The complete job log is 150,244 bytes with SHA-256 `60cc54eed54fbf47300ced4f649999e89293113777c76e6306b705bb27e446e5`.

The SDK retains and verifies the same files across the database restart. Each CLI phase writes and removes its own self-test file while verifying shutdown/reopen. These checks establish the exercised Linux SDK/CLI persistence and orderly database restart behavior. They do not establish power-loss recovery, mounted OS throughput, independent hosts sharing local SSDs, peer-cache qualification or the 10,000-client production target.

## Next admission

The laptop needs additional working disk headroom above the retained 64 GiB floor before another fresh run. A read-only footprint check of a previous complete run of the same size found more than 115 MiB allocated to retained regular files under its filesystem block root, native captures and benchmark output. This excludes directories, database growth and other host activity, and is not a sufficient capacity guarantee. Suppressing a few MiB of partial artifacts cannot establish admission for the complete workload.

The next run must use a new prefix/root and the unchanged durability, geometry, oracle and ownership gates. A fresh RustFS comparison requires the same release and an independently completed arm. Until then, the older RustFS measurements remain historical and no filesystem speedup ratio is claimed.
