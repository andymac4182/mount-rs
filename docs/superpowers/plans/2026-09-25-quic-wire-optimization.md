# QUIC wire optimization implementation plan

> **For agentic workers:** Use the subagent-driven-development skill for bounded implementation and review tasks; use test-driven-development for behavior changes.

**Goal:** Eliminate numeric JSON payloads on negotiated client I/O, reduce peer copies, bound active transfer memory and avoid serial peer latency.

**Architecture:** Negotiate typed binary I/O on `mount-rs/2`, the only currently supported version, retaining existing JSON control/metadata operations. Share existing authorization and audit dispatch. Peer v1 keeps its wire bytes while framing and cache access use owned/shared payloads. Apply byte budgets and connection windows before body allocation; hedge and replicate with bounded concurrency.

**Tech stack:** Rust 1.95, Tokio, Quinn 0.11.12, Rustls, existing providers and codecs.

## Global constraints

- Linux and macOS; ordinary local mounts and provider ownership remain unchanged.
- Keep ALPN and hello negotiation for future versions; advertise and accept only v2 today. ALPN/version binds the codec; no parse-error codec guessing or replay of uncertain writes.
- Exactly one partition per client connection; current permissions, catalog freshness, handle revision/backing checks, renewal, audit and timeout semantics remain authoritative.
- Peer identities/partition permissions and registered backing scope remain enforced. Cache requests never recursively forward/fetch origin.
- Frame/header/payload limits are validated before allocating body buffers; server-wide byte and operation bounds hold until cancellation work really ends.
- Cache placement remains optional after backing flush. Workers retain staging/IO charges through noncancellable work; no unbounded queue or scope table.
- Keep peer v1 bytes; only report measured gains. Production capacity/power-loss/cross-host claims require separate evidence.
- Serialize Cargo, Kani and timed workloads; use scripts/cargo-shared and /private/tmp/mount-rs-remote-drive-target. Preserve unrelated files.

## Tasks

- [x] 1. Negotiated client binary I/O. New protocol module with typed read/write requests, bounded raw bodies and typed errors/counts; typed transport APIs without legacy fallback; direct caller-buffer reads. Reuse service dispatch authorization/audit through an internal typed payload mode. Prove red/green codec bounds, truncation, version, permission and real QUIC negotiated-version rejection plus uncertain-write nonreplay.
- [x] 2. Peer copy and latency work. Shared immutable cache accessor, bounded header encoder and separate header/payload writes, direct status/body receive and borrowed decode before ownership. Retain peer v1 exact bytes. Add bounded placement workers and delayed hedge preserving query/deadline/byte quotas; regression slow owner/healthy replica, concurrent placement, isolation/trust/corruption/cancellation.
- [x] 3. Transfer admission. Explicit asymmetric client/peer windows and server-wide receive/send budgets before allocation, held through response ownership; bound generic control frames too. Tests saturated budgets, cancellation, oversized header/body and control progress.
- [x] 4. Qualification/review. Formatting, strict touched then workspace Clippy and all-target tests; real QUIC/Redis regressions; matched codec and real service 4 KiB read/write benchmarks with allocator-disabled controls. Extend bounded frame/budget arithmetic Kani harnesses and state proof limits. Independent specification/quality review of immutable patch; docs with actual bytes/allocations/throughput and limitations.

## Delivery evidence

See `docs/remote-quic-optimization.md` and retained `docs/benchmarks/remote-wire-20260925` artifacts. Latest-only v2 retains ALPN/hello negotiation. Review found and corrected renewal stream starvation with post-header per-connection data admission and Quinn credit slack. The old queued-overflow load fixture now verifies prompt rejection and exact admitted completion. Workspace serial gate: 1,315 passed / 106 ignored / 161 targets; two explicit Redis fixtures and two new arithmetic proofs passed. Matched codec comparator uses current v2 JSON controls, not a released v1 compatibility path; the real SQLite pair proves this local shared-drive diagnostic only.
