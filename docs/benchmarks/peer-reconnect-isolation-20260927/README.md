# Peer read reconnection isolation

The cache stage metrics identified a foreground read waiting behind a replica
PUT's connection negotiation. Each trusted peer now has independent read and
placement negotiation slots. Each role waits for its own slot and uses
`try_lock` to reuse a live, already authenticated connection from the other role.

## Controlled result

| Public cold read | Before | Final after |
| --- | ---: | ---: |
| Returned binary bytes | 786 | 786 |
| Read elapsed time | 402.80 ms | 6.04 ms |
| Backing GET calls | 1 | 0 |
| Backing returned bytes | 786 | 0 |
| Peer hits | 0 | 1 |
| Replica PUT still in flight during read | 1 | 1 |

These are one before and one final after observation on loopback. They identify
this connection-lock bottleneck; they do not establish sustained throughput,
physical IOPS or production latency. The replica caller is deliberately retained
unpolled after its first poll. Quinn's wire driver can independently finish its
handshake; the test retains the caller's negotiation guard.

The fixture observes the restarted server's real incoming service ownership
before cancellation. That observation is coupled to the current weak-owner
accept loop and does not assert TLS or task completion. Full persisted disk
bytes, returned bytes, authenticated reuse, a 44-byte PUT readback and inbound
partition denial pass before actual transport idle and zero tracked cache owners
permit directory removal. The baseline then fails exactly the zero-backing-GET
assertion; the final implementation passes all stage assertions.

## Validation

Fourteen local gates passed: 90 Rust test executions, 53 modeled parent controls,
formatting and strict Clippy. The gates cover both cold role directions, empty
PUTs, healthy reuse in both directions, same-role coalescing, cancellation of
owners and waiters, identity rejection, cache failure scenarios, Redis failure
and child cleanup, and remote/WebSocket lost-reply behavior. Warmed metric
recorders still pass zero added allocations; the whole cache/metadata path is
not qualified allocation-free.

The [report](report.json) retains source and receipt digests, logical I/O and
stage observations, and six earlier unqualified fixture shutdown attempts.
No raw paths, certificates or transport connection identifiers are published.
Exact selectors run through `scripts/test-remote-failures.py peerreconnect` and
`peermetrics`, with a dedicated `CARGO_TARGET_DIR`.

## Resource and qualification limits

Each trusted peer has two guarded negotiations and two cached connection slots.
Healthy sequential roles share one authenticated connection. Simultaneous cold
roles may establish two connections and consume two inbound/flow-control
budgets. Request, stream and byte admission remain shared; reads have no reserved
capacity when those budgets are exhausted. Cancelled Quinn attempts can retain
additional draining state, so two slots are not a total endpoint-state bound.

The earlier close-first Initial teardown stall remains a separate pending
investigation. Source tracing of the
[pinned Quinn release](https://github.com/quinn-rs/quinn/releases/tag/quinn-proto-0.11.18)
suggests missing first-packet close-timer bookkeeping. A deterministic protocol
reproduction and dependency fix are not qualified by this patch.

Hosted CI and merge remain separate gates. At base `6b282cc`, both Linux cache
and peer metric steps passed; the allocation step exposed only exit code 1.
The parent now emits bounded, fixed-field failure hints to distinguish offline
dependency, compilation, exact-case, zero-case and lifecycle failures. Those
marker classifications are diagnostic hints, not confirmed root causes.
