# Peer read reconnection isolation

## Evidence and hypothesis

The retained Redis fault qualification observed an overlapping replica PUT and
GET each taking about 400 ms, one PUT still in flight, and independently valid
bytes on the restarted peer's disk. Current `QuicPeerTransport::connection`
holds one per-peer mutex while awaiting QUIC/TLS establishment. A best-effort
replica handshake can therefore consume a foreground read's transport budget.

The new stage metrics separate that mutex wait from connection establishment.
First reproduce the amplification through the public cache path with a stopped
peer, an owned UDP blackhole, a pending replica handshake and a restarted peer
with verified disk bytes. Full returned bytes must remain correct; the current
implementation should fail the zero-backing-GET requirement.

## Proposed ownership

Give each trusted peer one read negotiation slot and one replica negotiation
slot. A request waits only for its own slot. Before starting a new handshake,
reuse a live authenticated connection from the other slot if that slot can be
inspected immediately. Never wait for the other slot. Both slots can refer to
the same healthy connection; simultaneous cold negotiations can establish two
connections. There are at most two cached connection slots and two guarded
negotiations per trusted peer; Quinn can retain draining connection state after
cancelled attempts, as it does with the current single slot.

Preserve the shared request, stream and byte admission bounds, exact request
deadline, certificate pin and partition checks, cache-only peer PUT semantics,
backing persistence and uncertain client-write behavior. Do not retry requests
or change the wire/storage format. Each slot's existing lock and establishment
spans retain their terminal and cancellation semantics. Cancellation drops only
the affected negotiation guard and cannot clear the other slot's connection.

The resource tradeoff is two cached outgoing connection slots per configured
peer during concurrent cold negotiation. Healthy sequential reads and placements
reuse one connection. Two cold connections can consume two transport flow-control
windows and two inbound connection permits; the application request, stream and
byte budgets remain shared. This does not reserve foreground admission capacity
when the existing global request or byte budget is exhausted.

## Gates

- [x] Observe a semantic RED after full-byte and bounded cleanup oracles.
- [x] Implement slot isolation and healthy cross-slot reuse.
- [x] Verify restarted-peer reads with zero backing GETs while the replica
  negotiation remains pending, then reuse after cancellation.
- [x] Verify both cold directions, same-slot coalescing, identity and partition
  rejection, bounded admission, and cancellation of each slot.
- [x] Run cache/remote regressions, recorder allocation controls, formatting and
  strict Clippy; retain actual stage evidence and unchanged source hashes.
- [x] Independently review the patch and actual evidence for publication to the
  existing PR.
- [ ] Qualify hosted CI before merging the published patch.

## Qualification limits

The regression measures cache/backing calls and full logical bytes on loopback.
It does not measure physical IOPS, production latency, all network failures or
the full 10-server/10,000-client workload. Reconnection isolation is one concrete
amplification fix; metadata capture/scan reduction and full capacity still need
their own evidence.

The [controlled report](../../benchmarks/peer-reconnect-isolation-20260927/README.md)
records the accepted semantic RED and final 14-gate local bank. Earlier fixture
shutdown timeouts remain unqualified. The separate close-first Initial drain
investigation needs its own deterministic reproduction and dependency fix.
