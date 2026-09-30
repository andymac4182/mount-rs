# Cache and WebSocket failure qualification — 2026-09-27

[Machine-readable report](report.json). Local macOS loopback execution, with
seven serial bounded gates against the same 441 frozen inputs. The final gates
passed 81 ordinary tests, three explicitly selected ignored failure cases,
strict scoped Clippy and workspace formatting. Independent source and runtime
reviews checked receipt/log hashes, full-byte oracles and cleanup observations.

## Backing calls by cache phase

| Phase | Peer hits | Memory backing GET calls | Backing bytes |
|---|---:|---:|---:|
| Healthy Redis directory | 1 | 0 | 0 |
| Advertised holder lacks bytes | 0 | 1 | 782 |
| Peer unavailable | 0 | 1 | 781 |
| Redis unavailable; restarted peer disk valid | 1 | 0 | 0 |
| Redis and peer unavailable | 0 | 1 | 785 |
| Total: five complete reads, five peer attempts | 2 | 3 | 2,348 |

Distinct binary payloads and one-peer query limits make directory steering and
fallback observable. Every returned byte is compared. Successful placement for
the stale-holder block and normal transport-error completion for the outage
block are observed before stopping/restarting A; cancellation cannot satisfy
the barrier. Redis cleanup observes the owned child's exit status before its
directory is removed and its socket is reused.

## Useful failure preserved

Without placement completion barriers, the recovered peer's 786 disk bytes
were independently valid with zero RAM usage, but an earlier PUT was still in
flight. That PUT failed after 401,887 microseconds; the overlapping GET failed
after 400,515 microseconds and added a backing GET. Source holds the per-peer
connection mutex across establishment. This supports the contention hypothesis;
it does not separately measure mutex wait and handshake time.

The test barriers isolate sequential fault phases. **Production reconnect
contention remains unresolved.** The direct disk oracle warms the OS page
cache, so these timings do not qualify cold physical disk latency or throughput.

## Durable WebSocket reply loss

A signed OIDC session, trusted TLS and negotiated protocol reach the actual
service and SQLite driver. The relay receives a complete successful write
reply, holds both sockets open and forwards none of the response. An independent
read-only SQLite view verifies metadata, all 65,673 bytes and exact EOF before
any close. After deliberate disconnect, the caller stays uncertain and refuses
follow-up requests; one write was submitted, with zero replay, reconnect or
QUIC datagrams during the 100 ms observation window. A fresh view repeats the
full metadata/bytes/EOF check after close completion.

A deliberate omission of only `PersistedHandle::write` persistence fails with
`preclose oracle metadata mismatch` before emitting a success receipt. The
production file was then restored byte-identically before final qualification.
An earlier compile-only failure and a prior parent portability failure remain
private failure evidence; neither is counted as a passing gate.

## Containment and CI

The repository parent keeps its leader unreaped through owned-group TERM/KILL
decisions, then observes leader reap, group absence and output EOF before
removing retained fixtures. Original Darwin terminal-only-group `EPERM` results
remain recorded; reconciliation requires those independent final observations.
Whole-process containment does not replace fixture graceful-cleanup assertions.
The 24 modeled parent controls fail three positive cases on the old parent and
pass on the correction. A separate shell control confirms strict CI pipelines
preserve producer exit 7 instead of masking it as exit 0.

The new CI job extracts an owned Redis executable without installing or
starting a service and exports only receipts/source pins. Its Ubuntu runtime,
package shared libraries and hosted completion are not yet verified.

These cache backing counts are synthetic memory `BlockStore` calls, not SSD
IOPS or object-store requests. The SQLite result is local commit and orderly
reopen, not crash or power-loss durability. Cross-host capacity, the unchanged
10-server/10,000-client/10,000-drive/5,000-partition/10-million-file target, and
complete symbolic qualification remain outstanding.
