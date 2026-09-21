# W01-FUSE progress tracker

Parent roll-up: [W01 progress ledger](./W01_PROGRESS.md)

Owner: delegated W01-FUSE task `01a0c45c-4283-7473-9588-59c41cd1def0`  
Worktree: `/Users/andrewmcclenaghan/.codex/worktrees/a0af/mount-rs`  
Branch: `andymac4182/c/w01-fuse-main-20260922`

Last refreshed: **2026-09-22** (Australia/Brisbane)

Status: **In progress — production NO-GO**

This tracker owns the FUSE protocol, mount-free session, native mount,
callback, and lifecycle boundary. A focused codec or session pass does not
close native Linux mount, hosted CI, FSKit, or root-callback acceptance.

| Gate | State | Required evidence |
| --- | --- | --- |
| Public FUSE exports and typed/raw protocol behavior | In progress | Pinned-oracle body/whole-message differentials and generated declarations |
| Rust-backed mount-free session | In progress | INIT, options, cache, lookup, flush, errors, handles, callbacks, and lifecycle coverage |
| Linux native FUSE | External gate | Hosted `/dev/fuse`/`fuse3` mount, read/write, unmount, fault and callback-event result |
| macOS/FSKit boundary | External gate | Supported/unsupported decision backed by an actual host result |
| Errors, cancellation, concurrency, crash, restart and cleanup | Open | Deterministic and native lifecycle evidence with explicit failure classification |

## Current queue

- Complete the remaining applicable FUSE session, callback, and mount members.
- Run hosted Linux native mount and transport-error event qualification.
- Qualify cancellation, close, crash/restart, concurrency, and durability
  behavior on every supported native runtime.
- Keep unsupported FSKit/platform results explicit; never convert skips into
  production passes.

## Evidence ledger

| Date | Chunk | Result | Remaining blocker |
| --- | --- | --- | --- |
| 2026-09-22 | Inherited W01 baseline | Current `origin/main` FUSE codec/session packets and existing root evidence remain available; privileged/native results are separate | Native Linux/FSKit, root callback events, remaining session parity, and lifecycle/race gates |
| 2026-09-22 | Rust session controls | Added public `FuseSessionOptions`, configurable inode identity, INIT preferences, cache/timeout policy, explicit flush modes, lifecycle/error observability, and regression tests; focused package tests and strict Clippy remain required before publication | Native mount/callback, hosted Linux, cancellation/concurrency, crash/restart and durability evidence |
| 2026-09-22 | Mount option boundary | Added fail-closed validation for caller-supplied native mount option tokens and transport-owned overrides; focused package tests and strict Clippy passed on macOS | Hosted Linux `/dev/fuse`, callback-event, crash/concurrency/durability, and signed/activated FSKit evidence |
| 2026-09-22 | N-API mount-free session facade | Added the Rust-backed `FuseSession` N-API class and public `./fuse` facade with typed options/defaults, negotiated state, inode views, request/reply/error counters, assertion/error callbacks, notification encoders, destroy-state readback, generated declarations, and raw INIT/LOOKUP/READLINK coverage | Native Linux callback events, complete operation/session parity, cancellation/concurrency, crash/restart and durability evidence |
| 2026-09-22 | Native FUSE terminal-error hooks | Added `FuseMountHooks`, owned `FuseTransportError` kinds, `mount_with_hooks`, exactly-once terminal reporting, callback-panic isolation, and a mount-free Unix-stream protocol-failure harness; current macOS FUSE target remains green and the Linux-only harness is target-gated | Hosted Linux `/dev/fuse` callback-event delivery, complete native lifecycle, cancellation/concurrency, crash/restart and durability evidence |
| 2026-09-22 | N-API automatic-mount callback bridge | Threaded `FuseMountHooks` through `mount-rs-auto` and exposed root `mount(..., { onTransportError })`; JavaScript callbacks are converted to an owned TSFN before the async mount future, with generated declarations and callback-safe teardown | Local Rust/N-API checks, Clippy, 16 N-API unit tests, debug addon build, generated typecheck, FUSE suite, and default facade test pass; hosted Linux callback-event delivery, FSKit, and full native lifecycle remain external |
| 2026-09-22 | Mount-free FUSE lock session | Added session-scoped `GETLK`/`SETLK` range conflict tracking, same-owner replacement/unlock, `RELEASE`/`DESTROY` cleanup, strict lock-body/flag validation, and explicit `EAGAIN` for `SETLKW` because the serialized session cannot safely block | Focused session target is 19/19, complete locked FUSE targets and strict Clippy pass; POSIX lock flags remain unadvertised for native mounts until a blocking/concurrent native path and hosted lock evidence exist |

## Completion rule

The owning task may mark W01-FUSE production-ready only when every applicable
gate above is PASS or explicitly accepted outside supported scope, the exact
commands and host prerequisites are recorded here, and this file is committed
with the implementation/test chunk. Until then the decision remains
**NO-GO**.
