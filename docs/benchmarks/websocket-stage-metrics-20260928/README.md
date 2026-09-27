# WebSocket stage qualification

The client now records eight stages: TCP connect, TLS handshake, HTTP upgrade,
socket wait, request encoding, request submission, response reception and response
decoding. The service uses its existing application/authentication recorder with
an appended upgrade row. Its WebSocket snapshot has 22 rows and a separate
`mount-rs.service-websocket.v1` schema.

Use the [operator guide](../../bottleneck-metrics.md) for build features, exact
profiling and tracing flags, counter scopes and unavailable quantities. Ordinary
server constructors keep the observer disabled. CLI output follows actual
service, provider and cache cleanup. Slow records retain fixed labels, the
100 ms threshold and the 16-record budget.

## Executed checks

The [report](report.json) joins 17 successful local gate receipts, their raw-log
hashes and source pins. It includes 15 real TLS/WebSocket client test windows
and both actual signed CLI shutdown records. Client rows conserve terminal
outcomes and settle in-flight gauges. The CLI's WebSocket record contains no
QUIC transport subtree; its warm verifier legitimately performs zero key fetches.
The separate signed catalog helper checks hello and renewal verification/grant
stages and one initial key fetch.

| Check | Retained result |
| --- | --- |
| Default service/client/CLI packages | 432 passed, 37 ignored across 36 test binaries |
| Packages with service/CLI profiling compiled | 452 passed, 37 ignored across 36 binaries; process-wide recorder off for parallel ordinary tests |
| Service constructor/lifecycle/authentication suites | 2 default, 8 enabled and 15 recorder tests passed |
| Core diagnostics and warmed recorder allocations | 20 diagnostics tests and one isolated allocation test passed |
| NAPI Rust export/unit tests | 53 passed, 6 ignored |
| CLI diagnostics and signed SQLite restart | 8 recorder tests and one real CLI test passed |
| SQLite committed write after lost WebSocket reply | One exact test passed without replay |
| Report validation and owned-parent controls | 235 pure Node controls and 81 modeled parent controls passed |
| Formatting and strict Clippy | Passed for touched packages and profiling builds |

The broader package run exposed three sampler lifecycle tests coupled to real
production disk admission. They now use an explicitly marked modeled disk probe
while retaining real PID, CPU/RSS and terminal sampler observations. Public
`Resources::start`, the real host preflight, the 64 GiB floor, RSS cap and shutdown
deadline remain unchanged. A new control rejects low or unavailable observations
before publishing a file or spawning the sampler thread.

The report fixtures were also corrected to declare the current 108 rows and
21 families. Historical inventories remain literal and incomplete, with missing
measurements unavailable. No projector padding or added addon client-source
coverage was introduced.

## Scope

This qualifies bookkeeping and preserved local transport/authentication/cleanup
behavior. Nested timings overlap and are inclusive wall time. Zero added
allocations applies to warmed span/recorder/scope helpers, excluding real TLS,
WebSocket, JWT, catalog and request allocations.

The WebSocket snapshot supplies no TCP/TLS wire-byte, frame-count, peer
acknowledgement, CPU or physical-device IOPS measurement. This slice supplies no
new native-addon, Linux, Windows, crash or power-loss qualification. The full
10,000-client / 10,000-Drive / 5,000-Partition / 10M-file production run, hosted CI
and PR merge remain pending.
