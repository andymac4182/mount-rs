# QUIC wire optimization diagnostics

Base b753b4e5; measured working tree includes typed v2 codec, global transfer bounds and peer copy/scheduling changes before the later per-connection control-capacity fix. Both codec modes use the current v2 server; numeric JSON is a benchmark-only control-lane comparator.

- Release build with resource-profiling; Rust allocator instrumentation disabled for both throughput runs.
- 100 continuously active QUIC clients, 10 coordinators, one shared SQLite drive; 32 × 4 KiB blocks/client, depth1.
- One-second warmup, five-second read and write stages; requests include authoritative catalog verification and audit.
- Exact count/content and fresh reopened filesystem verification passed, zero request failures in both runs.
- The codec microbenchmark counts allocation/reallocation requests; it excludes network/providers. Binary read encode includes a provider-vector clone.
- SQLite counter start sample resets counters. End is already the stage count; subtracting begin would be incorrect. OS block counts are zero for these warm buffered runs, not physical device IOPS evidence.

See [analysis](../../remote-quic-optimization.md) for results and limits.
