# 9P connection completion notifications

## Defect and fix

`ConnectionControl::wait` previously checked `done` before constructing its
`Notified` future. If `finish` set completion and broadcast in that gap, the
future missed the broadcast and could remain pending indefinitely.

Create the notification future before the Acquire completion check. The locked
Tokio 1.53.1 implementation records `notify_waiters` broadcasts from future
creation, including broadcasts before the future's first poll. A completion
before creation is covered by the subsequent `done` check; completion after a
false check is covered by the existing future.

Completion still follows transport shutdown, session destruction and removal
from the server's client map. Explicit close requests stop and waits for actual
completion. Dropping a connection clone or cancelling a waiter does not stop
the transport. This ordering change adds no heap allocation or timeout policy.

## Executed verification

The test-only hook injects actual `finish` after the real unfinished observation.
Manual polling avoids scheduler timing: the original implementation returns
`Pending` when the assertion requires `Ready`. The hook, its storage and its
call are absent from production builds.

| Gate | Observed result | Receipt SHA256 |
|---|---|---|
| Exact public wait regression before fix | One semantic assertion failure, Cargo 101 | `897aadaeab9f169c9a07ab1e7a5e659221223546f0088acfa59c1dc34d9c3070` |
| Library after fix | 39 passed | `185ac0fff21861c99cc50c6a7dbef5797d57c6e861dde8a3ca40e0ea1bf2df9b` |
| All compiled macOS targets | 70 passed across 12 harnesses | `993be0472f45386e841caf94631aed396834baf41fa0cbd85de062ff2404e603` |
| Strict Clippy, all targets | Passed | `19dcd748d6dff1f7b53b6e849115353f4f43e39c48704de96d6fc37e0d12d4bc` |
| Package formatting check | Passed | `c9080076adf92c3f055e01c09cb13bb6b67da4ed376b6a3b27fe947fc9dc6c4d` |

Each successful gate retained its output hashes, reaped its direct child,
confirmed process-group absence and preserved the 64 GiB free-disk floor.
The first all-target attempt ended during compilation and its guard failed the
process-group probe with `EPERM`, leaving no qualified result. Free disk space
was below the reserve immediately afterward. A subsequent read-only process
survey found no compiler or test remaining. Only old, nonexecuting third-party compilation
archives were reclaimed before the successful all-target run. Retained datasets,
native executables and evidence were preserved.

Six new controls exercise completion in the observation gap through both
`wait_closed` and `close`, completion before the first poll, broadcast to
multiple waiters, waiter cancellation, clone ownership and repeated close.
Existing loopback TCP/Unix-socket and attached-stream lifecycle tests also passed.

Final Rust source SHA256:
`39fce5adad23aabc20e60ea61b68bc82bf1c10d9e0e8833fa337751e1c792135`.
It differs from the independently reviewed GREEN candidate only in formatting.

## Remaining qualifications

The previous macOS Node job timed out after 20 seconds in the 9P shared-lock
phase. Its exact pending await and the causal relationship to this defect are
unproven. Validate with a freshly built N-API addon, the focused server test and
the original full CI command; retain the existing timeouts. This Rust result
does not establish that the Node timeout is resolved, native OS mounting,
cross-host behavior, performance capacity or full remote-storage qualification.
