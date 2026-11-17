# `serve-http` drive failover

This document is the design of automatic drive failover in a `serve-http`
forwarding cluster: what it does, the states and timing bounds, the rules it
never breaks, and what it deliberately leaves out. Operator-facing
configuration is summarised in [the CLI README](../apps/mount-rs-cli/README.md).

Failover is **opt-in**. A server without `http.forwarding.failover` behaves
exactly as before: it opens every drive in `http.drives` at startup, exits if
one cannot be opened, and owns those drives until it stops.

## What already existed

- **The storage writer lease is the only authority.** Opening a split-store
  drive takes an exclusive writer lease in its metadata store (SQLite or
  TiDB). Acquiring succeeds only when no holder exists or the holder's lease
  has expired, and each acquisition increments the drive's *fence*. Every
  publication carries the holder's fence, so a process that lost the lease
  cannot write. A clean shutdown releases the lease; a crash leaves it held
  until it expires (`lease_ttl_ms`, default 240 s for structured storage).
- **Redis is a routing hint.** An owner publishes a route record (node, boot,
  address, registration id, fence) under a 15 s TTL and refreshes it every
  5 s. A record with a lower or equal fence can never replace a live one. An
  ingress trusts a route only if it matches the pinned membership file, and
  the owner checks the route, the hop count and its current fence before it
  serves a forwarded request.
- **A drive that loses its lease fails closed.** A renewal that fails or
  times out marks the drive failed; every request then gets `503 ESTALE`, and
  the owner stops refreshing the route. Before failover, the process then
  exited and had to be restarted.

Failover adds no new authority. It only decides *which node tries to open a
drive, and when*.

## Model

**Candidates.** A server with `forwarding.owner: true` and a
`forwarding.failover` object is a *candidate* for every drive in its
`http.drives`. Several servers may list the same drive. They must describe
the same storage (the same metadata store and volume, the same block store)
and the same bearer token. The `owner` label inside `driver.storage` is only
a label on the lease; candidates may use the same or different labels.

**Liveness.** Each candidate announces itself for each of its drives in the
same Redis namespace: a sorted set per drive
(`mount-rs:<namespace>:candidates:<drive>`) whose members are node ids scored
with an expiry taken from the Redis server clock. An announcement lasts 15 s
(the route TTL) and is repeated every 5 s. Expired members are dropped by the
same script, so no node clock is involved. The set is a hint: only members
that are also in the trusted membership file are counted.

**Placement.** The candidates of a drive are ranked by rendezvous
(highest-random-weight) hashing: each live candidate's weight is a fixed
64-bit hash of `(drive id, node id)`, and the highest weight ranks first.
Every node computes the same ranking from the same live set, and a node
leaving changes the rank only of the nodes below it. A node's own rank
decides how long it waits before trying to take a drive nobody serves; it is
never a permission.

**Standby.** A candidate that does not own a drive does not open it. Its
public listener forwards requests for the drive to the published owner, like
any ingress.

## States

Each candidate runs one small state machine per drive.

```
            route absent for rank x stagger          lease acquired
 STANDBY ───────────────────────────────▶ OPENING ───────────────▶ OWNER
    ▲                                        │                       │
    │   lease held (EAGAIN): retry later     │                       │
    ├────────────────────────────────────────┤                       │
    │   open failed: back off, not a         │                       │
    │   candidate until the back-off ends    │                       │
    ├────────────────────────────────────────┘                       │
    │   lease lost / fenced / fail-back / shutdown                   │
    └──────────── RELEASING ◀────────────────────────────────────────┘
```

**STANDBY.** Every `poll_interval_ms` the node reads the drive's route.

- The read fails (Redis unreachable): no evidence, so do nothing.
- A route is published by another node: do nothing.
- No route (or a leftover route naming this node from an earlier process):
  note when the absence was first seen. Once it has lasted
  `rank x takeover_stagger_ms`, go to OPENING. The best-ranked live
  candidate has rank 0 and goes at once.

**OPENING.** The node opens the drive exactly as `serve-http` does at
startup. The open is never cancelled part-way, so a lease that was acquired
is always either kept or released.

- It succeeds: the node now holds the lease. Go to OWNER.
- It fails with `EAGAIN` (lease still held, by a live owner or by a crashed
  one whose lease has not expired): back to STANDBY. The next try is after
  `poll_interval_ms`, doubling to at most five times that.
- It fails for any other reason (storage unreachable, bad configuration):
  back to STANDBY with an exponential back-off (1 s doubling to 60 s). Until
  the back-off ends the node withdraws its announcement for that drive, so
  the others stop counting it. The doubling starts again from 1 s after an
  open that reached the lease (acquired for a minute, or found held).

**OWNER.** With the lease held, the node installs the drive in its peer
router and public listener, and only then publishes the route, carrying the
fence the lease returned. Every `poll_interval_ms` it checks that the lease
is still held with that fence.

- The lease is lost, or was re-acquired under a new fence, or a route with a
  higher fence is found in Redis: go to RELEASING.
- Redis is unreachable: keep the lease and keep serving requests that arrive
  on this node. Route publication is retried for as long as the lease is
  held. Ownership never depends on Redis being reachable.
- Fail-back is enabled and another live candidate has ranked first for
  `fail_back_after_seconds` without interruption: go to RELEASING.

**RELEASING.** In order: withdraw the route; remove the drive from the peer
router and public listener; wait for requests in flight (up to
`drain_timeout_ms`), close sessions and their handles; shut the drive down,
which releases the lease. Then back to STANDBY. On process shutdown the
announcement is withdrawn too.

## Invariants

1. A node publishes a route for a drive only while it holds the drive's
   writer lease, and the route's fence is that lease's fence.
2. A node opens a drive only after reading that no route is published. A
   failed read is never treated as "no route".
3. Nothing in Redis can make a node serve a drive. The lease is checked by
   storage on every publication, by the owner on every forwarded request and
   on every poll.
4. A node that loses the lease stops serving the drive, withdraws its exact
   route record and releases what it holds before it may try again.
5. Two nodes are never both able to commit writes to one drive: the second
   acquisition cannot succeed until the first lease is released or expires,
   and from then on the first holder's fence is rejected by storage.
6. Routes only move forward: a takeover publishes a strictly higher fence,
   and Redis refuses to replace a live route with a lower or equal one.

## Clocks

No node compares its clock with another node's. Three clocks are read:

- **Candidate announcements** expire on the Redis server clock.
- **The lease** expires on the metadata store's clock, in the same statement
  that checks it: the TiDB server's for TiDB, and for SQLite the clock of
  whichever host runs the statement. Every acquisition, renewal, release
  and publication is one conditional write on owner, fence and expiry, so a
  clock that is wrong can make a lease end early or late but cannot let two
  holders both commit: the second acquisition changes the fence, and from
  then on the first holder's writes match nothing. File blocks are
  content-addressed and written once; an upload by a node that has lost the
  lease is never referenced by a publication.
- **Each owner's own clock** decides only how long it trusts its last
  renewal without asking storage again: until 5 s (or the whole lease, if
  shorter) before the expiry storage returned. This is what an owner uses to
  answer reads and to admit forwarded requests between renewals.

The last one is the only place skew matters. An owner whose clock is behind
the metadata store's by more than that margin, or two SQLite hosts whose
clocks differ by more than it, can leave a deposed owner answering **reads**
from its last committed state for the excess, after a successor has taken
the lease. Its writes are refused throughout. Keep candidates and the
metadata store within a second or two of each other (NTP is enough), as for
any lease.

A process that is stopped for longer than its lease (a paused VM, a long
stall) is covered by the same rules: when it resumes its cached lease has
expired on its own clock, the renewal is refused, the drive fails closed and
the node releases it. `apps/mount-rs-cli/tests/failover.rs` pauses an owner
past its lease and checks that it never serves its stale copy afterwards.

## Timing bounds

Symbols: `R` = route TTL (15 s, fixed); `L` = the drive's `lease_ttl_ms`;
`P` = `poll_interval_ms`; `S` = `takeover_stagger_ms`; `k` = the taking
node's rank among the candidates still announced (0 for the best-ranked, at
most candidates - 1); `O` = the time to open the drive.

**Crash (SIGKILL, power loss, host or network loss of the owner).** The
route lapses at most `R` after the crash. The lease expires at most `L`
after the crash. A standby sees the lapse within `P`, starts trying after
`k x S`, and retries at most every `5P` while the lease is held. A new owner
is published no later than

```
max(R + k x S, L) + 5P + O      after the crash
```

With the defaults (`L` = 240 s, `P` = 1 s, `S` = 2 s) that is about
**245 s plus the open**; the lease dominates. With `lease_ttl_ms: 20000` it
is about 25 s. Below `L` = 15 s the route TTL dominates and the bound stays
near 20 s. Measured by `scripts/serve-http-cluster-e2e.py` with real
binaries, SQLite storage and `lease_ttl_ms: 20000`: 21 s from `kill -9` to
the new route, against a bound of 25 s plus the open; and 0.1 to 1 s after a
clean stop. An ingress caches a route for at most 1 s, and drops a cached
route when the owner it names does not answer, so clients see the new owner
within about a second of its publication.

**Clean stop of the owner (SIGTERM, or fail-back).** The owner withdraws the
route and releases the lease during shutdown. The new owner is published
within

```
P + k x S + 5P + O      after the old owner released the lease
```

normally one to two seconds. The `5P` term applies only if the standby
tried between the route withdrawal and the lease release.

**Loss of the lease on a live owner** (storage unreachable from the owner
only). The owner notices at its next renewal or poll, withdraws the route
and releases. A standby that can reach storage takes over once the lease has
expired: the crash bound applies, measured from the owner's last successful
renewal.

## Redis outages

- Standbys do nothing while they cannot read the route. Owners keep their
  leases and keep serving their own public listeners. Requests that need
  forwarding fail with 503, as they did before failover.
- When Redis comes back empty, owners republish within a few seconds (one
  poll after they can reach it; up to 5 s if Redis lost its data without the
  owner seeing an error). A standby
  that reads "no route" in that window may try to open the drive; the
  attempt fails with `EAGAIN` because the owner still holds the lease, and
  nothing changes.
- A Redis that loses announcements only changes how long a standby waits
  (`k x S`), never who may write.
- **An owner cut off from Redis while its standby is not.** The owner keeps
  renewing its lease in storage and keeps serving, for as long as the outage
  lasts (a server never exits because of Redis), and its route expires after
  15 s. The standby then reads "no route" and tries to open the drive. The
  attempt fails with `EAGAIN` because the lease is held, the standby logs
  `lease_held` once, publishes nothing and tries again every `5P` at most.
  The owner is not displaced and there is one writer throughout. Requests
  that reach the owner directly are served; requests that need forwarding
  get 503 until the owner can publish again, which it does within a few
  seconds of reaching Redis (measured 0.3 to 0.8 s after refused
  connections and 1.2 to 3.2 s after a silent link, which first has to be
  recognised as dead). If the owner really is gone, the same attempts
  succeed once its lease expires.
- **Every node cut off from Redis.** Nothing moves. If the owner also dies
  during the outage, the drive stays unavailable until a standby can read
  Redis again: a standby never opens a drive on a registry it cannot read.

## What clients see

| Situation | Response |
| --- | --- |
| Request arrives while no owner is published | `503`, `x-mount-dispatch: not-dispatched`, `Retry-After: 1`. Nothing was executed; retry. |
| Request was in flight on the owner that failed | `502`, `x-mount-dispatch: unknown`. The outcome is unknown and the request is not replayed. Reads may be retried; a write must be reconciled by the client (for example with `If-Match`). |
| Request forwarded while the crashed owner's route is still published (up to 15 s after the crash) | The forwarder waits on the dead owner for up to its 30 s operation timeout, then answers `502`, `x-mount-dispatch: unknown`. Clients should use their own shorter timeout and treat it the same way. |
| First requests an ingress forwards to an owner process that replaced a crashed one at the same address | `502`, `x-mount-dispatch: unknown`, at most once per pooled connection (`peer_connections`) of that ingress: each stale connection fails one request and is then replaced. Until the ingress has noticed that the old process is gone, such a request waits like the row above instead of failing at once. |
| Request reaches an owner that is releasing the drive | `503` (`server is shutting down` or `not-dispatched`). Retry. |
| Remote session or open handle that lived on the old owner | Lost. Session requests carrying the old session are refused by the new owner with `403 EACCES`; the client must create a new session and reopen its handles. Writes acknowledged before the failure are durable. |

Acknowledged writes survive a failover because they were committed to shared
storage under the lease before they were acknowledged.

The 502 rows describe how the forwarding layer already behaved when an owner
crashed; failover does not change them, it only makes a successor appear.

## Startup, shutdown and logs

A failover candidate opens no drive before it has read the registry. At
startup it waits up to 15 s for every drive to make its first decision, then
prints the usual `http listening` line. A drive it could take is being served
by then. A drive whose lease is held elsewhere leaves the server running as
a standby for it; the server does **not** exit with
`EAGAIN ... acquire writer` as a server without failover does. A drive that
cannot be opened for another reason is retried with back-off and reported,
and does not stop the server or its other drives.

On SIGTERM the server drains its public listener, then for each drive it
owns withdraws the route and its announcement, drains, and releases the
lease, so a standby takes over without waiting for anything to expire. The
release is given `drain_timeout_ms + drive_shutdown_timeout_ms`. If storage
does not answer in that time the server stops waiting, reports the drives as
abandoned and exits non-zero, as a server without failover does for a drive
that will not shut down; the lease then expires by itself and a standby
takes over after `L`.

Each state change is one JSON line on stderr:

```json
{"drive":"shared","event":"drive_failover","fence":7,"state":"acquired"}
{"drive":"shared","event":"drive_failover","fence":7,"state":"published"}
{"drive":"shared","event":"drive_failover","fence":7,"reason":"shutdown","state":"released"}
```

| `state` | Meaning |
| --- | --- |
| `acquired` | The drive is open here under `fence`; this server holds its lease. |
| `published` | Its route is in Redis (again, after an outage). |
| `released` | The drive was given up. `reason` is `lease_lost`, `fail_back` or `shutdown`. |
| `lease_held` | No route is published but the lease is still held; the server keeps retrying. Logged once per episode. |
| `open_failed` | The drive could not be opened (`error`); the server backs off. |
| `route_unpublished` | The lease is held but Redis cannot be written (`error`). Logged once per outage. |
| `release_failed` | A release step failed (`error`); the lease may stay held until it expires. |

`/readyz` and `/healthz` are unchanged, and `cache_report_interval_seconds`
reports the drives the server owns at that moment.

## Fail-back

By default a drive stays with the node that took it until that node stops:
moving a drive costs a drain and breaks its sessions. A rolling restart
therefore tends to leave the drives on the nodes restarted first.

`fail_back: true` lets an owner hand a drive back. When another live
candidate has ranked first for `fail_back_after_seconds` without
interruption (any failed read of the candidate set restarts the count), the
owner releases the drive as on a clean stop and does not try to take it back
for one route TTL (15 s). A hand-back succeeds when the node it was meant
for publishes the drive. If that node does not, whether nobody takes the
drive or a third candidate does, the old owner doubles its own
`fail_back_after_seconds` for that drive (up to one hour) the next time it
owns it, until a hand-back succeeds. A preferred node that announces itself
but cannot open the drive therefore costs each healthy candidate a
handful of hand-backs at growing intervals, not one every
`fail_back_after_seconds`. A candidate that cannot open the drive stops
announcing itself while it backs off, so it stops being preferred for that
long.

## Not handled

- **A hand-back to a node that has just died.** Liveness is a 15 s
  announcement. With `fail_back`, an owner can hand a drive back to a
  preferred node that crashed within the last 15 s, or that announces
  itself but cannot open the drive. The drive is then unavailable for the
  hold-off (15 s) until the old owner, or another candidate, takes it
  again. Later hand-backs are spaced out as described under Fail-back.
- **A release outside shutdown is not bounded by the supervisor.** After a
  lost lease or a hand-back the node drains for `drain_timeout_ms` and then
  shuts the drive down with no limit of its own; it relies on the storage
  client's timeouts. Until that returns the node is not a candidate for the
  drive. Only the release at process shutdown has the overall limit
  described above.
- **A candidate removed from the membership file but left running** keeps
  the drives it owns: the lease is still its own. Ingresses stop trusting
  its route, so those drives are unreachable through them until the node
  is stopped; standbys see its route, and its lease, and wait. Stop a node
  (SIGTERM hands its drives over) before removing it.
- **Redis can delay a failover, never cause two writers.** Anything able to
  write the namespace can publish a route or an announcement that makes
  standbys wait, or a higher-fence route that makes an owner release its
  drive (it is retaken once the forged record is gone). Restrict the
  namespace as the README describes.
- **Stale reads under clock skew** beyond the 5 s renewal margin; see Clocks.
- **A standby's attempt on SQLite metadata is not read-only.** Opening a
  SQLite metadata store folds the pending namespace change rows into the
  stored document before the lease is tried. The namespace is unchanged and
  the owner carries on (tested with 1700 attempts against an owner writing
  throughout), but each attempt while the lease is held, at most one every
  `5P` and only while no route is published, costs one rewrite of the whole
  document. Trying the lease before folding would avoid it and is left for
  the SQLite provider. The same contention has not been tried against TiDB.
- **No replay.** Requests in flight on a failed owner are not replayed, and
  sessions and handles do not move.
- **No faster recovery than the lease.** A standby cannot take a drive
  before the crashed owner's lease expires. Shorten `lease_ttl_ms` to
  shorten recovery, at the cost of less headroom for slow storage.
- **No check that candidates share storage.** Two candidates whose configs
  point the same drive id at different storage would each serve their own
  copy in turn. `memory` stores are rejected with failover for this reason;
  for the rest it is the operator's responsibility.
- **No load-aware placement.** Rank is a hash; it ignores load and capacity.
- **No lease on `sqlite`-kind and `host` drives**, so they cannot be
  failover candidates: failover requires `splitstore` drives with exclusive
  ownership.
- **Local caches are not shared.** A new owner starts with its own block and
  disk caches.
- **Route TTL is not configurable.** It stays 15 s.
