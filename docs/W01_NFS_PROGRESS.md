# W01-NFS progress tracker

Parent roll-up: [W01 progress ledger](./W01_PROGRESS.md)

Owner: delegated W01-NFS task (thread `01a0c456-a28e-7cb3-9b48-a3d23e7ec8c0`)

Last refreshed: **2026-09-22** (Australia/Brisbane)

Status: **In progress — production NO-GO**

This tracker owns NFSv3/MOUNT, NFSv4.1, the shared session/router boundary,
connection lifecycle, native mounts, and NFS-specific durability and handle
semantics.

| Gate | State | Required evidence |
| --- | --- | --- |
| NFSv3/MOUNT public API and wire behavior | In progress | Codec, rootless wire, session, error, generated N-API, and shared-handle-view evidence |
| NFSv4.1 router/session behavior | In progress | Version routing, COMPOUND/state matrix, shared handle/counter proof, direct N-API view, and bounded channel/state knobs |
| Connection and server lifecycle | In progress | Active connection objects/counts, malformed/EOF/reset handling, close/wait, async disposal, and restart evidence |
| macOS/Linux native NFS | Hosted acceptance evidenced | Run `35658285441` completed both `native-nfs` jobs successfully, including privileged native tests and SQLite-over-NFS; keep the job-scoped platform evidence current |
| Handles, concurrency, crash and durability | In progress | Cross-version handle lifetime, bounded LRU/pinning, cancellation/close, crash/restart, ordering, and persistence evidence |

## Current queue

- Reconcile the remaining upstream NFS connection-object surface and direct
  session/member parity; the local live-client object/close boundary, both
  N-API session `destroy()` operations, direct v3/v4/unified driver and
  effective-options views, write-verifier readback, direct v3 routing, bounded
  `maxHandles`/NFSv4 pinning policy, bounded v4 channel/state knobs, and
  deterministic static NFSv4 owner map, lease expiry enforcement, and
  request-level `onError` callback now pass. Dynamic owner callbacks and the
  N-API `now` bridge now also pass their Rust and live N-API evidence. The
  low-level mutable v4 state table and callback function values are explicitly
  outside the supported N-API inspection surface; their behavior is qualified
  through wire operations, effective knobs, lease sweeping, shared handles,
  and live callback tests.
- Exercise the full v3/v4 behavior matrix, including stateful v4 operations,
  malformed records, reconnects, and version negotiation.
- The rootless wire suite now drives two independent NFSv4.1 sessions through
  concurrent distinct-file OPEN/WRITE/READ round trips. This closes a bounded
  in-process multi-client userspace case. Multiple server processes sharing a
  backend are explicitly outside supported scope because session, lease,
  replay, and handle arbitration is process-local; native-client ordering
  remains a separate hosted/platform gate.
- Keep the hosted native result current. Run `35658285441` completed both
  `native-nfs (macos-latest)` and `native-nfs (ubuntu-latest)` successfully:
  macOS covered native NFSv3, CLI persistence/cleanup, and SQLite hosting;
  Ubuntu covered the privileged NFSv3/NFSv4.1 lane and SQLite hosting.
- Qualify cross-process crash, concurrency, and durability behavior; bounded
  cancellation/close now has a local transport lifecycle gate, and the
  host-backed NFSv3 data path now has a forced-process-restart gate. NFSv4
  lease/replay/file-handle recovery, cross-process concurrency, and power-loss
  durability remain outside the supported local claim. The restart gates
  explicitly reject a pre-crash NFSv3 file handle with `NFS3ERR_STALE` before
  recovering the same bytes through a replacement-server LOOKUP, and reject a
  pre-crash NFSv4.1 session with `NFS4ERR_BADSESSION` before dispatch and the
  pre-crash root handle with `NFS4ERR_STALE`.

## Evidence ledger

| Date | Chunk | Result | Remaining blocker |
| --- | --- | --- | --- |
| 2026-09-22 | v3/v4 shared-state foundation | N-API v3/v4 direct routing and `Nfs4Session` view pass; the Rust server shares the v3/v4 handle table, path lock, and counters; NFS crate and host-enabled package regressions pass | Direct shared-handle inspection, remaining upstream connection/session member parity, native/hosted lifecycle, and crash/durability gates |
| 2026-09-22 | active connections and shared handle inspection | Rust `NfsServer::connections()` now reports active accepted socket tasks and close awaits their teardown; N-API `NfsServer.connections`, `NfsSession.handles`, and `Nfs4Session.handles` are generated and exercised with BigInt handle identity. Rust tests pass 31 unit, rootless wire 1, transport errors 4, v4 barrier 1, and v4 wire 2; release N-API build, generated typecheck, and live server integration pass | Full v3/v4 stateful matrix, remaining upstream connection/session member parity, native/hosted lifecycle, and crash/durability gates |
| 2026-09-22 | live NFS connection objects | Rust `NfsServer::clients()` now exposes stable-id/peer connection objects with shared v3/v4 sessions and abort-safe `close`/`wait_closed`; N-API `NfsConnection` and the `./nfs` subpath export are generated and exercised against a live loopback client | Full v3/v4 stateful matrix, hosted lifecycle, Linux NFSv4.1, crash/durability, and any remaining upstream member differences |
| 2026-09-22 | transport reconnect and pipelined concurrency | Rootless NFSv4.1 proves a session can continue across an orderly TCP transport reconnect while the server process remains alive; a rootless NFSv3 connection completes eight pipelined MOUNT NULL calls with `max_in_flight=4`. The complete NFS target passes 31 unit, rootless wire 1, transport concurrency 1, transport errors 4, v4 barrier 1, and v4 wire 3 | This is userspace/process-lifetime evidence only; automatic reconnect, crash/restart and durable lease/reply recovery, native Linux v4.1, hosted lifecycle, and the full stateful/member matrix remain open |
| 2026-09-22 | restart-boundary state classification | A rootless restart-boundary test reuses the backend across two server instances, proves `NfsServer::close()` destroys the first v4 session, and verifies the old session is rejected with `NFS4ERR_BADSESSION` by the replacement server. The v4 wire target now passes 4/4 | This proves process-local session/lease/replay state is not crash-durable; backend data durability, crash injection, native Linux v4.1, hosted lifecycle, and full upstream state/member parity remain open |
| 2026-09-22 | macOS native NFSv3 loopback | The refreshed opt-in `native_loopback_mount_round_trip` gate passed 1/1 in 0.11s on the exact pushed tip, including the temporary native mount, filesystem round trips, and bounded cleanup | Privileged Linux NFSv4.1, hosted lifecycle, full stateful matrix, remaining upstream member differences, and crash/durability gates |
| 2026-09-22 | pinned upstream and bounded-handle parity | The pinned oracle (`85361a8212ff9bff8e69f62fa8993ef2c2ec51e8`) passed 266 NFSv3/MOUNT and NFSv4.1 TCP conformance cases with 18 capability/root skips and zero mismatches; the N-API NFS codec differential passed; `maxHandles` now has shared LRU eviction, root/current protection, and NFSv4 open-state pins, covered by two focused table tests and the N-API server option path | The 18 conformance skips are explicit capability boundaries, not passes for unsupported rows; upstream `onError`, callback ID maps, N-API clock injection, native Linux v4.1, hosted lifecycle, and crash/durability remain open |
| 2026-09-22 | NFSv4 channel/state limit parity | Rust `Nfs4StateOptions` and nested N-API `Nfs4StateKnobs` now cover lease seconds, per-client sessions, fore slots, COMPOUND operations, request/replay-cache ceilings, per-file opens/locks, and reclaim policy; the v4 wire suite passes 5/5, including `maxLocksPerFile` enforcement for an existing lock state, `NFS4ERR_TOOSMALL`/`NFS4ERR_NOSPC` channel-cap statuses, and replay of a refused `CREATE_SESSION` followed by a next-sequence retry | Upstream dynamic ID-map callbacks, N-API clock injection, and session `onError` remain explicit parity gaps; native Linux/hosted lifecycle and crash/durability remain external gates |
| 2026-09-22 | NFSv4 reclaim ordering | With `requireReclaimComplete` enabled, `OPEN` and `LOCK` now return `NFS4ERR_GRACE` until the client completes `RECLAIM_COMPLETE`; the affected rootless v4.1 round-trip and same-owner/cross-client share tests pass | Full state/member parity, callback ID maps, N-API clock injection, session `onError`, native Linux/hosted lifecycle, and crash/durability remain external gates |
| 2026-09-22 | NFSv4 static ID-map parity | Rust `Nfs4IdMap` and nested N-API `Nfs4IdMap` now qualify mapped user/group names, preserve numeric fallback, reject other domains with `NFS4ERR_BADOWNER`, and keep user/group namespaces separate; the full locked NFS target passes 35 unit tests, rootless wire 1, pipelined concurrency 1, transport errors 4, v4 barrier 1, and v4 wire 5; release N-API build, generated typecheck, live server integration, and strict affected Clippy pass | Upstream callback-based maps, N-API clock injection, session `onError`, native Linux v4.1, hosted lifecycle, and crash/durability remain external gates |
| 2026-09-22 | NFSv4 seeded identity parity | `nfs4.seed` now folds a configured uint32 into the client-id high half and session-id prefix; the rootless v4 wire test asserts both identities and N-API parsing/type coverage passes | N-API clock injection, callback-based maps, session `onError`, native Linux v4.1, hosted lifecycle, and crash/durability remain open |
| 2026-09-22 | NFSv4 lease expiry enforcement | `Nfs4Clock` enables deterministic Rust monotonic-time tests; expired clients are swept automatically before COMPOUND dispatch or explicitly through `Nfs4Session::sweep_expired`, releasing sessions, locks, open states, and pinned backend handles; the v4 wire test covers both paths and passes 6/6 | N-API clock injection, callback-based maps, session `onError`, native Linux v4.1, hosted lifecycle, and crash/durability remain open |
| 2026-09-22 | NFS request error callback parity | Rust `NfsSessionHooks` and N-API `NfsServerOptions.onError` now report ordinary status failures and decoded XDR/dispatch failures; decoded calls, panic isolation, generated typing, and live N-API delivery are covered; complete NFS target passes 37 unit, rootless wire 1, transport concurrency 1, transport errors 4, v4 barrier 1, and v4 wire 6, with release build/typecheck and live server integration green | Dynamic ID-map callbacks, N-API clock injection, native Linux v4.1, hosted lifecycle, and crash/durability remain external gates |
| 2026-09-22 | NFSv4 dynamic owner and clock callbacks | Rust `Nfs4IdMap` now supports panic-isolated synchronous name/id callbacks, and the N-API `nfs4.idmap.nameOf`/`idOf` plus `nfs4.now` callbacks are retained and released with the server; a live N-API v4.1 `EXCHANGE_ID`/`CREATE_SESSION`/`GETATTR`/`SETATTR` sequence observed owner and group callback arguments, translated names, reverse translations, and injected clock calls. The complete locked NFS target passes 38 unit, rootless wire 1, transport concurrency 1, transport errors 4, v4 barrier 1, and v4 wire 6; release build, generated typecheck, live server integration, pinned NFS codec differential, and strict affected Clippy pass | Full upstream state/member matrix, native Linux v4.1, hosted lifecycle, and cross-process crash/cancellation/concurrency/durability remain external gates |
| 2026-09-22 | bounded NFS close cancellation | A new real-TCP lifecycle target drives MOUNT into a blocked `FsDriver::stat` and proves both `NfsConnection::close()` and `NfsServer::close()` cancel the request worker, retire the active connection, and return within 250 ms; 2/2 pass. The recorded hosted native-NFS jobs for run `35650924001` were cancelled, so they are not promoted to acceptance evidence. | Full upstream state/member matrix, native Linux v4.1, a completed hosted lifecycle run, and cross-process crash/concurrency/durability remain external gates |
| 2026-09-22 | queued NFS close cancellation | With `max_in_flight=1`, a second real-TCP MOUNT record waits for a dispatch permit behind a blocked first request; `NfsConnection::close()` now observes shutdown while waiting, aborts the active worker, retires the connection, and passes the bounded lifecycle target. The complete locked NFS target passes 39 unit tests, process restart 2, rootless wire 1, transport concurrency 1, transport errors 4, lifecycle 3, v4 barrier 1, and v4 wire 7; strict Clippy, formatting, and diff checks pass | This closes the local queued-request teardown race; automatic reconnect, full upstream state/member scope, native-client ordering, cross-process concurrency, and crash/durability qualification remain open |
| 2026-09-22 | concurrent NFS listen/close serialization | Concurrent `NfsServer::listen()` calls now share one bound TCP listener, and `close()` cannot race a bind because both use one lifecycle gate; the focused real-TCP lifecycle target passed 4/4, including blocked, queued, server-close, and concurrent-listen cases. The complete locked NFS target passed 39 unit tests and all applicable integration targets; strict Clippy, formatting, and diff checks passed | This closes the local duplicate-listener and bind/close race; automatic reconnect, full upstream state/member scope, native-client ordering, cross-process concurrency, and crash/durability qualification remain open |
| 2026-09-22 | N-API session destroy parity | `NfsSession.destroy()` now tears down both shared v3/v4 sessions and `Nfs4Session.destroy()` tears down v4 state; generated declarations, direct runtime assertions, and TypeScript assignments pass alongside the release addon, live N-API server integration, pinned codec differential, and strict affected Clippy | Remaining upstream `driver`/`options`/direct-v3 member differences, native Linux v4.1, a completed hosted lifecycle run, and cross-process crash/concurrency/durability remain external gates |
| 2026-09-22 | N-API session/member parity and supported scope | Unified, direct v3, and direct v4 N-API views now expose the server-owned read-only driver wrapper, effective scalar options, configured ID-map presence, write verifier, shared v3 routing, and shared handle/counter/destroy state; `./nfs` exports `Nfs3Session`. Release declarations, TypeScript assignments, live runtime identity/verifier/options assertions, the NFS codec differential, the refreshed pinned oracle conformance (266 passed, 18 explicit capability/root skips, 0 mismatches), full locked NFS targets, and strict affected Clippy pass. The low-level mutable v4 state table and callback function values remain intentionally unexposed; live wire/callback/lease tests cover their supported behavior. Hosted CI runs `35656252661`, `35657100915`, and `35657445618` for the published parity/evidence commits were cancelled before any native-NFS job ran and are not acceptance evidence. | A completed hosted native lifecycle run and cross-process crash/concurrency/durability remain external gates; the 18 pinned-oracle capability/root skips remain explicit boundaries |
| 2026-09-22 | hosted native NFS platform qualification | Run `35658285441` completed `native-nfs (macos-latest)` (`106528418544`) and `native-nfs (ubuntu-latest)` (`106528418983`) successfully. The macOS job passed native NFSv3, CLI persistence/cleanup, and SQLite-over-NFS; the Ubuntu job passed privileged native NFSv3/NFSv4.1 and SQLite-over-NFS, with macOS-only CLI checks skipped on Ubuntu. | The named NFS jobs are accepted platform evidence, but the overall workflow remains non-green because unrelated jobs failed; full upstream state/member scope, cross-process crash/concurrency/durability, and production acceptance remain open |
| 2026-09-22 | cross-process NFSv3 backend recovery | A child NFS server wrote `crash-recovered.txt` with `FILE_SYNC`, was force-terminated, and a replacement child server using the same `HostFs` root successfully performed MOUNT, LOOKUP, and READ; the replacement also verified the persisted host file directly. The focused process target passed 1/1 and the complete locked NFS target passed. The subsequent code-bearing hosted run `35661453808` had both native-NFS jobs cancelled before any steps and is not acceptance evidence. | This qualifies host-backed NFSv3 data recovery across a process crash, not power-loss durability or NFSv4 lease/replay/file-handle recovery; cross-process concurrency and those NFSv4 durability claims remain open |
| 2026-09-22 | cross-process NFSv3 handle boundary | The forced-restart target now also sends the pre-crash file handle to the replacement server and observes `NFS3ERR_STALE` before looking up the same path and recovering the `FILE_SYNC` payload; the focused process target passed 1/1. | This makes process-local handle state explicit, but does not qualify cross-process handle recovery, power-loss durability, NFSv4 lease/replay recovery, or cross-process concurrency |
| 2026-09-22 | cross-process NFSv4 session and handle boundary | A real child server established an NFSv4.1 session and root handle, was force-terminated, and a replacement child rejected the old session with `NFS4ERR_BADSESSION` before dispatch and the old root handle with `NFS4ERR_STALE`. Session IDs now fold both halves of the eight-byte write verifier, avoiding the observed rapid-replacement alias. Both process-restart tests passed 2/2 with warning-denied NFS Clippy green. | This classifies NFSv4 session/lease/replay/handle state as process-local under a crash, but does not qualify durable v4 recovery, cross-process handles/concurrency, power-loss durability, full upstream state/member scope, or production acceptance |
| 2026-09-22 | current-tip pinned NFS oracle refresh | The exact pinned differential was rerun against the published listen/close-serialization tip with `CI=true CARGO_TARGET_DIR=/private/tmp/mount-rs-w01-nfs-lifecycle-20260922 MOUNTX_SOURCE=/private/tmp/mountx-w01-nfs-oracle pnpm --dir tests/upstream exec vitest run nfs-conformance.test.mjs --config vitest.config.mjs`: 266 NFSv3/MOUNT and NFSv4.1 cases passed, 18 capability/root cases remained explicit skips, and zero failures or mismatches were reported. | The 18 capability/root skips remain explicit boundaries; this is userspace TCP oracle evidence and does not replace native-client, hosted lifecycle, crash/durability, or production acceptance gates |
| 2026-09-22 | NFSv4 two-client concurrency and scope | The rootless v4 wire target now establishes two independent sessions, runs distinct-file OPEN/WRITE/READ round trips concurrently over two TCP connections, and reads both exact payloads back; the full v4 wire target passed 7/7 and warning-denied NFS Clippy passed. Multiple server processes sharing one backend are explicitly outside supported scope because session, lease, replay, and handle arbitration is process-local. | Native-client ordering remains a separate hosted/platform gate; power-loss durability, full upstream state/member scope, and production acceptance remain open |
| 2026-09-22 | hosted status for v4 concurrency tip | CI run [`35663954461`](https://github.com/andymac4182/mount-rs/actions/runs/35663954461) for the published concurrency commit was cancelled before GitHub created any jobs (`jobs: []`); it is not hosted native-NFS evidence. The latest accepted named native jobs remain run `35658285441`. | No newer hosted native result is claimable; native-client ordering, NFSv4 lease/replay/file-handle recovery, power-loss durability, full upstream state/member scope, and production acceptance remain open |

## Exact commands and gate boundaries

- `./scripts/cargo-shared test -p mount-rs-nfs --all-targets --locked` — PASS:
  39 unit tests, process restart 2, rootless wire 1, transport concurrency 1,
  transport errors 4, lifecycle 4, v4 commit barrier 1, and v4 wire 7; the
  native mount target remains 1 explicitly ignored test. The reconnect and
  pipelining rows are rootless userspace evidence, not native or hosted-client
  acceptance.
- `CI=true CARGO_TARGET_DIR=/private/tmp/mount-rs-w01-nfs-conformance-20260922 MOUNTX_SOURCE=/private/tmp/mountx-w01-nfs-oracle pnpm --dir tests/upstream exec vitest run nfs-conformance.test.mjs --config vitest.config.mjs` — PASS: 266 pinned-oracle NFSv3/MOUNT and NFSv4.1 TCP cases, 18 explicit capability/root skips, 0 mismatches; the disposable target was removed after the run.
- `CI=true CARGO_TARGET_DIR=/private/tmp/mount-rs-w01-nfs-lifecycle-20260922 MOUNTX_SOURCE=/private/tmp/mountx-w01-nfs-oracle pnpm --dir tests/upstream exec vitest run nfs-conformance.test.mjs --config vitest.config.mjs` — PASS at the listen/close-serialization tip: 266 pinned-oracle NFSv3/MOUNT and NFSv4.1 TCP cases, 18 explicit capability/root skips, 0 mismatches.
- `(cd integrations/mount-rs-napi && pnpm build)` — PASS: release addon and
  generated declarations rebuilt.
- `(cd integrations/mount-rs-napi && node test/typecheck.mjs)` — PASS.
- `(cd integrations/mount-rs-napi && node test/servers.mjs)` — PASS: live NFS
  v3/v4 routing, callback-backed v4.1 `EXCHANGE_ID`/`CREATE_SESSION`, owner
  `GETATTR`/`SETATTR`, injected clock calls, shared handle snapshots, active
  connection count, live client identity/peer/session views, connection
  close/wait, direct v3/v4 session destruction, malformed record reporting,
  request-level NFS `onError` delivery, and destroyed-state cleanup.
- `(cd integrations/mount-rs-napi && node test/nfs-codec.mjs)` — PASS for the
  root/`./nfs` export identity checks and the complete pinned byte differential
  against `85361a8212ff9bff8e69f62fa8993ef2c2ec51e8`, including
  `NfsConnection`.
- `./scripts/cargo-shared clippy -p mount-rs-nfs -p mount-rs-napi --all-targets --locked -- -D warnings` — PASS.
- `MOUNT_RS_NFS_NATIVE_TEST=1 ./scripts/cargo-shared test -p mount-rs-nfs --test native_mount -- --ignored --exact native_loopback_mount_round_trip --nocapture` — PASS: macOS native NFSv3 loopback mount, filesystem round trips, unmount, and bounded cleanup; 1 passed, 0 failed, 0.11s on the exact pushed tip.
- `./scripts/cargo-shared test -p mount-rs-nfs --test transport_lifecycle --locked` — PASS: 4/4 bounded real-TCP lifecycle tests covering connection-level and server-level close over a blocked backend request, connection close while a queued request waits for the configured in-flight slot, and concurrent idempotent `listen()` calls.
- `./scripts/cargo-shared test -p mount-rs-nfs --test process_restart --locked -- --exact nfs_v3_host_backend_survives_process_crash_and_restart --nocapture` — PASS: a forced child-process termination caused the replacement server to reject the old file handle with `NFS3ERR_STALE`, then replacement-server MOUNT/LOOKUP/READ recovered the `FILE_SYNC` payload from the same `HostFs` root; 1 passed.
- `./scripts/cargo-shared test -p mount-rs-nfs --test process_restart --locked -- --exact nfs_v4_session_and_handles_are_process_local_after_process_crash --nocapture` — PASS: a forced child-process termination caused a replacement server to reject the old NFSv4.1 session with `NFS4ERR_BADSESSION` before dispatch and the old root handle with `NFS4ERR_STALE`; 1 passed.
- `./scripts/cargo-shared test -p mount-rs-nfs --test v4_wire --locked` — PASS: 7 rootless NFSv4.1 wire cases, including two independent sessions concurrently completing distinct-file OPEN/WRITE/READ round trips; 0 failed.
- Hosted run [`35658285441`](https://github.com/andymac4182/mount-rs/actions/runs/35658285441) — PASS for both named NFS jobs: [`macOS job 106528418544`](https://github.com/andymac4182/mount-rs/actions/runs/35658285441/job/106528418544) passed native NFSv3, CLI persistence/cleanup, and SQLite-over-NFS; [`Ubuntu job 106528418983`](https://github.com/andymac4182/mount-rs/actions/runs/35658285441/job/106528418983) passed the privileged native NFSv3/NFSv4.1 lane and SQLite-over-NFS. The overall workflow remains non-green because unrelated jobs failed, so this is job-scoped NFS evidence rather than a whole-workflow release pass.

## Completion rule

The owning task may mark W01-NFS production-ready only when every applicable
gate above is PASS or explicitly accepted outside supported scope, exact
commands and prerequisites are recorded here, and this file is committed with
the implementation/test chunk. Until then the decision remains **NO-GO**.
