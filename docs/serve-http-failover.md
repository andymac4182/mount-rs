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
   (A Redis Sentinel failover can bring an older route back for a few
   seconds; see [Redis Sentinel](#redis-sentinel).)

## Clocks

No node compares its clock with another node's. Three clocks are read:

- **Candidate announcements** expire on the Redis server clock.
- **The lease** expires on the metadata store's clock, in the same statement
  that checks it. For SQLite that is the clock of whichever host runs the
  statement. For TiDB it is `CURRENT_TIMESTAMP(3)` of the `tidb-server`
  that runs the statement, which is that server's own host clock and not
  the cluster's timestamp oracle: candidates that reach TiDB through
  several `tidb-server` instances read as many clocks. Every acquisition, renewal, release
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
the metadata store's by more than that margin, or two SQLite hosts (or two
`tidb-server` hosts) whose clocks differ by more than it, can leave a
deposed owner answering **reads** from its last committed state for the
excess, after a successor has taken the lease. Its writes are refused
throughout. Keep candidates and the metadata store, every `tidb-server`
included, within a second or two of each other (NTP is enough), as for any
lease.

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

## TiDB metadata

Nothing above is specific to SQLite. `apps/mount-rs-cli/tests/failover.rs`
runs every scenario with real binaries against either store; with
`MOUNT_RS_FAILOVER_TEST_STORAGE=tidb-rustfs` the lease is in TiDB and the
blocks are in RustFS, as two candidates on different hosts would have them.
The `tidb-rustfs` CI job runs them that way after its restart checks.

Measured with TiDB v8.5.7 (one `tidb-server`, one TiKV, one PD, in Docker on
a laptop), RustFS 1.0.0, `lease_ttl_ms: 12000` and the default
`poll_interval_ms` and `takeover_stagger_ms`, over eight runs of the suite
(six with its tests in parallel, two one at a time):

| Scenario | Measured | Documented bound |
| --- | --- | --- |
| `kill -9` of the owner under four writers, to the new route | 14.7 to 15.7 s | 22 s plus the open |
| Owner stopped with `SIGSTOP` past its lease, to the new route | 15.7 to 16.3 s | 22 s plus the open |
| The resumed owner gives the drive up | 0.1 to 0.3 s after `SIGCONT`; it never served its stale copy | next poll |
| Clean stop (`SIGTERM`), from the owner's exit to the new route | 1.0 to 3.6 s; a retrying client was served again 1.1 to 4.5 s after the signal | 8 s plus the open |
| Redis down for 25 s with the owner killed, from Redis returning to the new route | 1.2 to 3.6 s | 8 s plus reconnecting and the open |
| Fail-back to the preferred node, from its start | 12.4 to 15.8 s | 21 s |
| Release that TiDB does not answer (the lease row is locked) | the owner exits non-zero 2.0 to 2.1 s after `SIGTERM` (limit 2 s); the standby takes the expired lease 11.3 to 15.0 s later | `L` + `5P` + open |
| Owner cut off from Redis for 42 s | it served every direct write (98 to 132 of them; the slowest request took 0.45 s); the standby retried for 26 s and reported `lease_held` once and nothing else | no takeover |

In the kill test every acknowledged write was read back complete from the
new owner, and every write that was not acknowledged was either absent or
complete.

**What a standby's attempt costs TiDB.** Opening a TiDB drive connects,
sends the provider's schema statements (`CREATE TABLE IF NOT EXISTS` and six
`ALTER TABLE ... IF NOT EXISTS`), an `INSERT ... ON DUPLICATE KEY UPDATE`
that changes nothing, and then reads the lease row `FOR UPDATE` in a
pessimistic transaction, which it rolls back on finding the lease held. So
an attempt is not read-only, but it rewrites nothing. While the standbys above retried, TiDB ran no schema
job (the last job id was the same before and after), and their open files
stayed level. `crates/mount-rs-cluster/tests/failover.rs` opens a volume as
fast as it can while the owner writes to it: 626 attempts in 30 s (641 in a
second run) all found the lease held and nothing else, none of the owner's
1182 (1385) writes in that time failed, and a successor read the files as
the owner left them.
The lease row is locked for the length of each attempt's locking read, so a
publication by the owner can wait behind one; the slowest direct request in
the partition runs above is the measure of that.

**`lease_held` can also be a TiDB conflict.** The standby treats any
`EAGAIN` from an open as "the lease is held". The TiDB provider also
reports `EAGAIN` for an error TiDB asks to have retried (a write conflict, a
lock wait timeout, a deadlock, or a message ending in `try again later`),
whichever statement of the open it came from. Such an open is retried after
`poll_interval_ms` like any other, without the back-off of `open_failed`,
and is logged as `lease_held`. No test provokes one.

**Choosing `lease_ttl_ms`.** The owner renews every eighth of the lease and
gives each renewal as long again; a renewal that takes longer fails the
drive closed, and the node releases it and becomes a standby again. On
TiDB a renewal is one pessimistic transaction of four round trips to one
`tidb-server`, so `lease_ttl_ms / 8` is the longest stall of TiDB (a
`tidb-server` restart behind a load balancer, a TiKV leader election) that
an owner rides out without giving its drive up, and `lease_ttl_ms` itself
is how long a crashed owner's drive stays unavailable.

- The default, 240 s, rides out a 30 s stall and recovers from a crash in
  about four minutes. Keep it unless that recovery time is too long.
- `lease_ttl_ms: 120000` rides out 15 s and recovers in about two minutes.
  Go no lower if owners should keep their drives through a TiKV leader
  election, which takes on the order of ten seconds after a store fails.
- `lease_ttl_ms: 60000` rides out 7.5 s and recovers in about 65 s. A
  longer stall costs every owner its drives: each is released, and taken
  again by whichever candidate asks first once TiDB answers, with the
  sessions on it lost.
- The 12 s lease of the tests (1.5 s per renewal) lost no lease in the runs
  above, on an idle single-node TiDB on the same host. It is a test value.

None of the stall figures was measured by stalling TiDB: they follow from
the renewal rule, and the length of a TiKV election is TiKV's own default.
The recovery figures are the crash bound under Timing bounds, which the
runs above met with the 12 s lease.

A `SIGTERM` release is bounded by `drain_timeout_ms +
drive_shutdown_timeout_ms` whatever TiDB does. An open, and a release after
a lost lease, are bounded only by the provider's statement timeout (120 s
unless `statement_timeout_ms` is set in the connection URL; a lock wait
gives up after 50 s).

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

## Redis Sentinel

With `http.forwarding.redis_sentinel` the registry is the primary of a
Redis Sentinel deployment (one primary, its replicas, and the sentinels
that monitor them) instead of a single Redis server. Nothing above changes:
Redis still only holds hints, and the storage lease is still the only
authority. This section is what a failover of *Redis* does to those hints.

**Which server is used.** A server asks its configured sentinels, in order
from the one that answered last, for the address of the primary
(`SENTINEL GET-MASTER-ADDR-BY-NAME`), connects to the address named, and
uses it only if that server itself reports the `master` role. Every
registry command (the route scripts, the route read, the candidate script)
goes to that one connection. A replica is never read or written.

**When the primary is looked up again.**

- The connection drops or is refused.
- A command is answered `READONLY` or `MASTERDOWN`: the server was demoted.
- Three commands in a row go unanswered.
- About once a second while commands are being sent, a check finds that
  the sentinels name another address, or that the connected server no
  longer reports `master`. A demoted primary keeps answering reads, so this
  check is what moves a server that only reads (an ingress, a standby). A
  check that cannot reach a sentinel changes nothing (the `ROLE` half of it
  still runs).
- A command sent more than two seconds after the last check began waits
  up to half a second for a new check before it is sent. Without this, a
  connection left idle while its server was cut off or demoted was used
  once more on trust: an owner's route refresh (every 5 s) went to an
  isolated old primary once after the sentinels had named another, and a
  registry connection that had not been used at all kept pointing at the
  old primary indefinitely, so its first read could come from the cut-off
  server or, after the heal, from a replica. If the check cannot finish in
  half a second (no sentinel answers), the command is sent on the
  connection it has, as before.

The command that met the failure is not sent again. Its caller sees an
unavailable registry and does what it does in any Redis outage: an owner
keeps its lease and its own listener, a forwarded request gets `503`
not-dispatched, a standby takes that poll as no evidence. Until the
sentinels have promoted a replica there is no primary to find, and every
lookup fails; this is the outage described under Redis outages, and it
lasts as long as the sentinels take (`down-after-milliseconds` plus the
election).

**What the promoted server may have forgotten.** Redis replication is
asynchronous. The new primary can lack the last writes the old one
acknowledged, and can still hold what those writes replaced or deleted.
Each case, and what the cluster does:

| The new primary... | What happens | For how long |
| --- | --- | --- |
| lacks an owner's route, or has it with less time left than the owner believes | Requests that need forwarding get `503` not-dispatched. A standby that reads "no route" may try to open the drive; the open fails with `EAGAIN` because the owner holds the lease, and nothing changes. The owner's next refresh finds its record missing and publishes it again. | At most one refresh interval (5 s) after the owner reaches the new primary, plus one reconciliation. |
| still holds a **predecessor's** route (a lower fence) that the current owner had replaced, or that the predecessor had withdrawn | Forwarders send requests for the drive to the predecessor. If that server is running it refuses them (`503` not-dispatched): it no longer holds that registration, and it checks its own lease before serving. If it is gone, the requests wait and fail as in "What clients see" for a crashed owner. No request is served by it. The current owner publishes over the old record at its next refresh: a lower fence never blocks a higher one. | Until the owner's next refresh (at most 5 s after it reaches the new primary), or until the old record's remaining lifetime (at most 15 s) runs out, whichever is first. |
| lacks a candidate's announcement, or still lists a candidate that has withdrawn or died | Only the order in which standbys try (`k x S`) is affected. Announcements are repeated every 5 s and expire after 15 s. | At most 15 s. |

A record with a **higher** fence than the live owner's cannot reappear:
fences are issued by storage and only grow, and a route is published only
under the fence of a lease that was actually granted. So a promoted replica
can show an older route for a few seconds, but it can never make the
current owner give its drive up, and it can never make another node able to
write. Invariant 6 ("routes only move forward") therefore holds on any one
Redis server, and across a Redis failover it holds again once the owner has
refreshed.

One real fault was found and fixed here. An owner **without** `failover`
used to treat any route it did not write as a conflict when it reconciled,
and stopped with exit status 1. A predecessor's record reappearing after a
Redis failover (the second row above) would have stopped the drive's
correct owner. It now publishes over a record with a lower fence, as a
failover candidate always did, and still stops for a record with an equal
or higher fence that is not its own.
`a_predecessors_record_that_reappears_is_replaced_by_the_current_owner` in
`crates/mount-rs-cluster` covers the registration, and
`apps/mount-rs-cli/tests/redis_sentinel.rs` puts a predecessor's record back
on a promoted primary under a running failover owner.

**Clocks.** Route lifetimes and candidate announcements expire on the Redis
server's clock, and after a failover that is another host's clock. Redis
replicates expiries as absolute times, so a new primary whose clock is
ahead of the old one's ends records early by the difference (owners publish
again within a refresh interval), and one that is behind keeps a dead
owner's route or announcement for that much longer than 15 s. Keep the
Redis hosts' clocks together (NTP is enough). Not tested: the test
deployment runs on one host.

**Partitions inside the Redis deployment.** Tested with real processes by
`apps/mount-rs-cli/tests/redis_sentinel_partition.rs`. The fixture
(`scripts/redis-sentinel-fixture.sh` with
`MOUNT_RS_SENTINEL_FIXTURE_PARTITION=1`) runs the six Redis processes in one
container, each making its connections from its own loopback address, and
drops packets with iptables from a second container that shares the
network namespace: established connections go silent, as in a real
partition. Links between Redis processes are cut individually; the host,
and so every mount-rs server at once, can be cut off from any of them.

| Partition | What happens | Measured |
| --- | --- | --- |
| The old primary cut off from the sentinels and both replicas, still reachable by every server, still accepting writes | The sentinels promote a replica. Every server stops sending commands to the old primary within about a second of the promotion (the periodic check, or the check before an overdue command) and the owner's route is on the new primary. Nothing a server wrote to the old primary alone is kept: when the link returns the sentinels make it a replica and it resynchronises from the new primary, dropping its own writes. A route record with a later fence naming another node, put on the isolated old primary by the test after the servers had left, was never read (an owner without failover that read it would stop) and was gone after the resync. No server went back to the old primary. The route's fence never changed. | Promotion 4.3 to 4.7 s after the cut; last command from a server to the old primary 0.2 s after the promotion; route on the new primary 4.6 s after the cut; after the heal, a replica in 10.5 to 11.2 s and its own writes gone in 15.4 to 16.6 s. Every forwarded and direct read was served (longest gap 0.13 s). |
| Split brain: the old primary with one sentinel on one side, both replicas with two sentinels (a quorum and a majority) on the other; the owner can reach only the lone sentinel, the standby and an ingress only the other two | Two primaries. The owner keeps publishing on the old one, where its record stays fresh. On the new one the replicated route lapses and nothing replaces it: the standby reads "no route", opens the drive, gets `EAGAIN` (`lease_held`) and never takes it. Forwarded requests on the majority side get `503` not-dispatched; requests to the owner are all served. The lease owner and fence never change. After the heal the owner's sentinel names the new primary, the owner moves there and publishes its route under the same fence, and forwarding resumes. | Promotion 4.5 s after the cut; route lapsed on the new primary 11.7 s after the cut; `lease_held` 0.1 s later; split held 27 s; after the heal, the owner's route on the new primary and forwarding back in 4.5 s. Forwarded reads went up to 20 s without an answer (all `503`). |
| The servers cut off from the primary; the sentinels and replicas are not, so no failover | A Redis outage for the servers, as under "Redis outages": the owner keeps serving its drive, forwarded requests get `503`, the owner's unrefreshed route expires on the primary, nobody exits. When the link returns the owner publishes again on the same primary. | Forwarded requests refused 0.5 s after the cut; route back 0.14 s after the heal. |
| The servers can reach only the old primary (not the sentinels), and the old primary is cut off from the rest | The servers keep using the old primary: a check that cannot reach a sentinel is no evidence. All of them use the same server, so forwarding keeps working through it while the sentinels promote another. When the old primary rejoins and becomes a replica, the servers stop using it (its `ROLE`); with no sentinel reachable they have no primary and forwarded requests get `503`. When the sentinels are reachable again the servers find the new primary. | Forwarding through the old primary worked throughout the split; after it became a replica, forwarded requests were refused within 2.3 s; with the sentinels back, the route was on the new primary in 0.9 s. |

A server that can reach only the old primary while *other* servers follow
the sentinels is the split-brain row: the two groups read different
registries. Requests are never served by a server that does not hold the
lease, because the lease is in storage; what the split costs is
availability (`503`) on the side whose registry lacks the route, for as
long as the split lasts. Redis's own `min-replicas-to-write` makes a
primary that has lost its replicas refuse writes, which turns the owner's
side into an outage instead; it is not tested here.

Not tested: the reverse split (the owner on the majority side and a
standby on the minority side with the old primary), which by the same
reasoning leaves the standby reading the owner's lapsed route and finding
the lease held; an owner that dies *during* a split, whose successor's
route on one side is lost when the sides merge (the predecessor-record row
of the table above then applies); and a partition between mount-rs
servers and only some of the sentinels other than by giving a server a
shorter sentinel list, which is how the split-brain test cuts each server
off. The host-side cut applies to every server at once.

**Measured** by `apps/mount-rs-cli/tests/redis_sentinel.rs` with real
servers, Redis 8.4, one primary, two replicas and three sentinels
(`down-after-milliseconds 2000`, quorum 2), all in one container on a
laptop, over seven runs, with and without TLS and passwords:

| Step | Measured |
| --- | --- |
| `kill -9` of the primary to a promoted replica | 3.1 to 5.9 s |
| To the owner's route being on the new primary | 3.2 to 7.2 s after the kill (0.1 to 1.5 s after the promotion) |
| To a write forwarded through an ingress succeeding | 3.2 to 7.0 s after the kill |
| Longest time a client repeating one forwarded read went without an answer | 2.2 to 4.0 s; every other answer in between was a `503` |
| Direct requests to the owner during all of it | all served; longest gap 0.11 to 0.27 s |
| A route deleted from the new primary, to the owner publishing it again | 0.6 to 3.0 s |
| A predecessor's route put back on the new primary, to the failover owner publishing over it | 4.7 to 4.9 s |
| With two candidates: the lease, the owner and the fence | unchanged; the standby never opened the drive and logged no `lease_held` |
| The owner then killed: the standby's route on the promoted primary | 11.6 to 14.7 s (12 s lease) |

The old primary, started again, was reconfigured as a replica by the
sentinels and no server returned to it.

The partition figures above are from one run of each test on a laptop
(Docker Desktop, Redis 8.4, the same deployment shape); each test was run
at least twice.

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
- **Redis Cluster.** The registry talks to one Redis server, or to the
  primary of a Redis Sentinel deployment. There is no cluster redirection
  in the client: a Redis Cluster endpoint is not supported and not tested.
- **Redis hosts whose clocks differ**, and sentinels added or removed
  while servers run (the sentinel list is read from the configuration at
  startup), are not tested. Partitions inside a Redis Sentinel deployment
  are tested, with the limits listed under
  [Redis Sentinel](#redis-sentinel): during a split brain, forwarded
  requests on the side without the route get `503` for as long as the
  split lasts.
- **A standby's attempt on SQLite metadata takes the write lock but writes
  nothing.** Opening the metadata store checks the schema and tries the
  lease in `BEGIN IMMEDIATE` transactions, so an attempt waits for, and
  briefly excludes, the owner's commits. It commits no change: the pending
  namespace change rows are folded into the stored document only in the
  transaction that takes the lease, and an attempt that finds it held leaves
  them, the document and the schema version as they were (the SQLite
  provider tests this with `PRAGMA data_version`, and
  `crates/mount-rs-cluster/tests/failover.rs` opens a volume as fast as it
  can against an owner writing throughout). The exception is a metadata
  file from an older build, whose first open by this build adds the missing
  columns and tables whoever opens it. Operations other than opening,
  loading and publishing still fold, whoever calls them; a standby calls
  none of them before it holds the lease.
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
