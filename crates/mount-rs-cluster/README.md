# mount-rs-cluster

Server-side advisory volume routing. Clients never connect to Redis. An ingress
server resolves a volume route and forwards directly to the authenticated owner.
TiDB grants ownership and fencing generations; Redis only advertises them.

`RedisRouteRegistry` takes a Redis client or connection that the caller has
configured, a deployment namespace and a bounded request timeout. This crate
does not open a connection from a URL and does not itself require TLS or
authentication: whatever the caller's `redis::Client` was built with is what
is used. `mount-rs serve-http` builds that client in
`apps/mount-rs-cli/src/redis_connect.rs`, which is where `rediss://`, the
password, a private CA and the refusal of plaintext to another host are
enforced; see "The Redis connection" in `apps/mount-rs-cli/README.md`. Register a
`VolumeRoute` only after validating TiDB ownership and loading the volume. The
record includes node/boot/registration identities, a full u64 TiDB fence, socket
address and TLS server name. Membership must authenticate that address/name;
Redis content alone never authorizes a peer or file operation.

Updates are atomic Lua operations. A lower/equal competing fence cannot replace
a live registration; refresh/removal compare the entire registration. Expired
records cannot be renewed. After Redis restart or expiry, advertisement requires
fresh ownership validation. A stale advertisement after Redis loses its record
can still produce a stale hint; the destination's live ownership check must
reject it. This registry does not solve ownership by storing an epoch in Redis.

Records are bounded to 4096 bytes before Redis GET allocation, identifiers to
256 ASCII characters, discovery TTL to 20ms–30s, and request timeouts to 30s.
Lost write replies return `WriteOutcomeUnknown`; no automatic write replay is
performed. Reconcile before retrying. Provision separate physical maintenance
connections if lookup load could delay registration refresh.

The exact-registration compare/update/remove pattern is adapted from inspected
[agentuplink catalog code](https://github.com/andymac4182/agentuplink/blob/main/crates/tunnel-catalog/src/redis.rs).
This is a mount-specific implementation, not a dependency on its device catalog
or Redis ownership authority. The optional HTTP/3 integration forwards requests
from an ingress server to the registered owner.

Validation:

```sh
./scripts/cargo-shared test -p mount-rs-cluster --locked
# Only against an isolated disposable Redis instance:
MOUNT_RS_REDIS_TEST_URL=redis://127.0.0.1:6379/ \
  ./scripts/cargo-shared test -p mount-rs-cluster --locked -- --ignored
```

The real-service test uses a unique namespace and verifies takeover at the top
of the u64 range, stale cleanup, exact refresh, expiry and lookup. These
tests use disposable Redis 8.4.0 over loopback plaintext. TLS with a private
CA, password and ACL logins, refused handshakes and logins, and an outage
longer than the route lifetime are tested with real `serve-http` processes
in `apps/mount-rs-cli/tests/redis_connection.rs`. Redis Sentinel, Redis
Cluster and a replicated pair failing over are not tested.

## HTTP/3 transport foundation

Enable `http3` to use `mount_rs_cluster::transport`, the local copy of agentuplink
`tunnel-transport` based on `626ba9365b5d3e1faf8265f0b6ff50cc9111b749`.
Healthy checkout reads the peer’s atomically published connection slot. A missing or superseded connection enters per-destination dial coordination.
The root Cargo patches pin h3 0.0.8, h3-quinn 0.0.10 and quinn-proto 0.11.17 to
the same revision. The explicit quinn-proto version constraint prevents a newer
registry release silently bypassing the patch. Downstream workspaces must carry
the same root patches; Cargo ignores patches declared inside dependencies.

Reviewed patch purposes: validated remote GOAWAY and unresolved-request
cancellation/accounting; cancellation-safe Quinn receive adapters; immediate
stream-credit replenishment at bounded capacity. The protocol patches use upstream sources under their original licenses. The local transport and its MIT license are in `vendor/tunnel-transport`.

The transport's peer certificate role currently uses the URI SAN
`urn:agent-tunnel:peer:<node-id>`. Reusing the transport requires that compatible
certificate profile in addition to CA trust, approved public-key pins and the
application's membership/owner policy. A Redis endpoint/name is insufficient.

```sh
./scripts/cargo-shared test -p mount-rs-cluster --features http3 --locked
./scripts/cargo-shared clippy -p mount-rs-cluster --all-targets --features http3 --locked -- -D warnings
```

The real loopback mTLS test checks ten requests on one pooled connection, request
policy refusal without handler dispatch, and client/server shutdown. It does
not yet exercise filesystem forwarding, production certificates, all upstream
patch regressions, or route changes during active filesystem operations.

## Owner-bound registrations

`OwnedVolumeRegistration` binds a route to an `Arc<mount_rs_sdk::Filesystem>`.
It validates the SDK's current fencing generation before advertisement/refresh
and again after the Redis await. Shutdown, failed ownership, or generation change
rejects the registration and attempts exact-record removal. A destination check
also requires the complete expected route, including process boot identity.
These are admission snapshots; filesystem operations retain their own lease
checks, and callers must separately authenticate peers and authorize users.

`OwnedVolumeRegistration::run` advertises and refreshes every TTL/3 with a
TTL/3 update deadline. The caller spawns/joins it and supplies a cancellation
token and status watch channel. It reports failure before cleanup and attempts
exact withdrawal on failure or graceful stop. It never automatically replays an
unknown write or re-advertises a missing/replaced registration. Aborting the task
clears its observed readiness; Redis TTL handles abandoned cleanup. Join this
supervisor before filesystem shutdown. `mount-rs serve-http` starts and joins
it for each drive of a server configured with `http.forwarding.owner`
(`apps/mount-rs-cli/src/http_sessions.rs`).
Cancelled cleanup can leave only a TTL-bounded hint. If ownership changes during
an advertisement, stale discovery may exist briefly; it never authorizes serving.

`OwnedVolumeRegistration::reconcile` is an explicit supervisor operation after
route loss or an uncertain Redis update. It validates owner permission, reads
the route, advertises a missing hint or refreshes an exact match. An observed
different registration returns `Conflict` without modifying it. Advertisement
and refresh still perform their atomic Redis comparisons and owner checks;
a race may return an error rather than success. The caller owns retry/backoff,
cancellation and readiness. This method never acquires a filesystem lease or
replays a filesystem request, and `run` does not invoke it automatically.

`run_with_recovery` is an opt-in alternative. Supply `RegistrationRecovery`
with a positive initial delay, a maximum delay no greater than 30 seconds and
1–100 attempts per outage, or `RegistrationRecovery::UNLIMITED_ATTEMPTS` to
keep reconciling for as long as the registry is unavailable. It uses capped
exponential backoff, publishes
`Recovering(error)` while unavailable, and resets the attempt budget after
successful reconciliation. A conflicting registration observed during
reconciliation, lost ownership or invalid registry data is terminal. Cancelling
recovery attempts exact withdrawal; aborting the task clears observed readiness
and leaves TTL-bounded cleanup. Each backoff is jittered between half and all
of the current exponential ceiling, with fresh per-outage process-randomized
entropy to spread retries across volumes and servers. A live Redis regression
recovers 100 independent memory-backed volume registrations after concurrent
route removal, preserves their fences and withdraws all routes on cancellation.
This is control-plane coverage, not 100-client filesystem latency or Redis HA
failover qualification. Fleet deployment tuning remains open.
`mount-rs serve-http` runs every owner registration this way through
`RegistrationSupervisor`, with unlimited attempts and a two-second maximum
delay: an owner serves its own drives without Redis, so a Redis outage of
any length must not stop it. With a bounded budget the worker fails when the
budget runs out, and the supervisor reports failure for the whole server.

Use `RedisRouteRegistry::with_client` when recovery must survive both a
dropped socket and a connection that stays open but stops answering. It
builds a `redis::aio::ConnectionManager` from the supplied `redis::Client`
and configuration, and replaces it after three consecutive commands time out
on it. The manager alone reconnects after a refused, reset or closed
connection but never after a timeout, so a connection whose packets are
silently dropped would otherwise fail every command until the operating
system abandoned it. `with_client` connects lazily: check a connection from
the same client first if a bad address, certificate or password should stop
startup.

`RedisRouteRegistry::with_manager` takes a caller-configured
`redis::aio::ConnectionManager` and has the manager's behaviour only.
The original `new(MultiplexedConnection, ...)` constructor remains available
but does not reconnect that connection. With the pinned Redis 1.7 client, a
failed command still returns its error while reconnection supports later
commands; registry write errors remain `WriteOutcomeUnknown` and must be
reconciled. Configure TLS, authentication and connection/reconnect timeouts
on the manager, and keep maintenance and lookup managers separate if traffic
isolation is required. The registry's outer command timeout is unchanged.
An isolated `CLIENT KILL ID` regression verifies a new connection ID and an
unchanged route after reconnect; Redis restart/failover remains a separate gate.

Run `./scripts/test-redis-restart.sh` for the isolated restart gate. It starts
a dedicated nonpersistent Redis 8.4 container on an explicit loopback port,
stops/starts that container and verifies the endpoint is unchanged and volatile
state was lost. The same owner process recovers its route and Ready state,
retains its fencing generation and reads the original bytes through an open
handle. Cancellation withdraws the recovered route. This uses memory metadata
and content, not TiDB takeover or native remote mounts. Redis HA failover and
network partitions are not covered. The script owns and cleans its container.

For actual TiDB metadata with local persistent blocks, run:

```sh
MOUNT_RS_TIDB_DOCKER_PLATFORM=linux/amd64 \
MOUNT_RS_TIDB_TOPOLOGY=single \
MOUNT_RS_TIDB_COMPOSITION_COMMAND='MOUNT_RS_REDIS_RESTART_METADATA=tidb ./scripts/test-redis-restart.sh' \
./scripts/test-tidb.sh
```

This variant uses noatime, a 240-second lease and a 60-second serving window.
It additionally rejects a competing owner, then cleanly releases and reopens
under a higher fence with persisted contents intact. This is single-node
fixture evidence, not crash takeover, partition or cross-host local-storage
acceptance. See `benchmarks/read-audit/results/tidb-redis-restart-validation.json`.

The isolated Redis test now also opens an SDK split volume, advertises and
refreshes it, rejects another boot identity, shuts the volume down, verifies
refresh refusal/removal, then verifies delayed cleanup preserves a successor.
That test uses memory metadata; combined TiDB ownership-loss/Redis-partition
acceptance remains required.

Supervisor validation against real Redis covers a route surviving multiple TTLs,
graceful stop/removal, SDK shutdown causing `Failed(OwnershipLost)`, and preserved
successor routing. A task-abort test checks the watch state cannot remain Ready.
Status is observational: no cached Ready message authorizes a request, and
`check_destination` plus the filesystem operation's own lease checks remain
mandatory. Redis delay/partition and cancellation during an ambiguous update
still need combined failure testing.

## Failover of volume ownership

Several servers may be candidate owners of one volume. Which of them serves
it is still decided only by the storage writer lease; these pieces decide
who tries to take it, and when. Design and timing bounds:
[`docs/serve-http-failover.md`](../../docs/serve-http-failover.md).

- `placement` ranks a volume's candidates by rendezvous hashing of
  `(volume, node)`. The hash is fixed (FNV-1a and a SplitMix64 finish) and
  pinned by a test: every node must compute the same ranking.
- `RedisRouteRegistry::announce_candidate`, `candidates` and
  `withdraw_candidate` keep one sorted set per volume
  (`mount-rs:<namespace>:candidates:<volume>`) of node ids scored with an
  expiry on the Redis server clock. The set is a hint and must be
  intersected with trusted membership.
- `FailoverMachine` is the per-volume decision logic with no I/O: a standby
  opens a volume only after reading that no route is published and waiting
  `rank x stagger`; a registry that cannot be read is never evidence; an
  owner lets go when it loses the lease or, if enabled, to hand the volume
  back to a better-ranked live candidate.
- `FailoverSupervisor` (feature `http3`) runs the machine for each candidate
  volume of a server. It opens a volume through a caller-supplied
  `DriveFactory`, installs it in the `OwnerHttpRouter` (`insert`), publishes
  its route under the lease's fence, and on release withdraws the route,
  removes and drains the handler, and shuts the volume down. A route is
  never published for a volume that is not open. `mount-rs serve-http`
  starts it for an owner configured with `http.forwarding.failover`.

`OwnerHttpRouter` now serves a set of volumes that can change while it runs;
requests read an immutable snapshot. An ingress answers `503` with
`x-mount-dispatch: not-dispatched` and `Retry-After: 1` when no route is
published.

`tests/failover.rs` runs candidates against a real Redis and a SQLite-backed
split store: clean handover, a crashed owner replaced only after its lease
expires, a lost lease, a candidate that cannot open the volume, fail-back
on and off, and three candidates across repeated stops.

## HTTP/3 owner application

`OwnerHttpHandler` serves one SDK volume through the existing `mount-rs-http`
application without a second TCP listener. Attach it to a membership-authorized
`PeerServer`. It requires exactly one `x-mount-owner-route` header containing the
complete matching `VolumeRoute` and `x-mount-hop: 1`. Route/identity mismatch and
recursive forwarding are refused before filesystem dispatch. The handler checks
live SDK ownership before dispatch; individual filesystem operations retain
lease validation. Ordinary per-drive bearer authorization is preserved.

Request bodies are pulled as bounded HTTP/3 chunks and response bodies are
streamed with transport backpressure. Dropping a handler cancels unfinished
stream directions. Response-body failures after dispatch remain transport errors,
not safe-to-retry admission refusals. The adapter does not automatically replay
writes or claim exactly-once filesystem effects. Drain the peer listener, close
the application, withdraw registration and then shut down the SDK volume.

The loopback mTLS test now writes 131123 bytes through this handler, reads the
full file and a range, checks wrong-bearer refusal, stale fence and second-hop
refusal without file changes, and ownership-loss refusal after SDK shutdown.
It also verifies listener-free application shutdown. That test connects directly
as an authenticated server peer. The cluster load harness exercises Redis route
resolution and ingress forwarding across three processes. `mount-rs
serve-http` wires the peer listener, the authenticated membership and the
registration supervisor into its service lifecycle when `http.forwarding` is
configured; `scripts/serve-http-cluster-e2e.py` exercises that with real
processes.

## Ingress forwarding

`IngressForwarder::forward` accepts an HTTP request body, discovers the volume
route on the server, verifies it against a bounded operator-authenticated
`PeerDirectory`, and streams to the owner using a shared `PeerClient`. The peer
certificate must match the configured public-key pin and node role. Redis cannot
supply its own trusted endpoint, boot identity or certificate.

### Live membership reload

A server takes a changed membership without a restart. `PeerMembership` holds
the current `PeerDirectory` and the SPKI pin set derived from it. Build the
forwarder with `IngressForwarder::with_membership`, every `PeerClient` with
`PeerClient::new_with_pin_provider(.., membership.pins(), ..)`, and the peer
listener with `PeerServer::new_with_pin_provider(.., membership.pins(), ..)`
and a request policy that calls `membership.accepts(identity)`. Nothing then
holds a copy of an older membership:

- the transports check a peer's pin against the current pin set after every
  handshake, and the listener checks it again on every request;
- the listener's policy checks the peer's node and pin against the current
  directory on every request;
- the forwarder resolves a route against the current directory on every
  request, including a route it has cached, and requires the owner's
  certificate to carry that record's node and pin.

TLS chain validation against the CA is unchanged; the CA is read once at
startup.

`IngressForwarder::reload_membership(candidate)` (or `PeerMembership::apply`
for a server with no forwarder) replaces the membership:

1. The candidate is validated in full first. A reload is refused, and the
   membership in force stays exactly as it was, when the file does not parse;
   lists no servers, more than 1,024, or more than the capacity the
   `PeerMembership` was created with; has an invalid record or a repeated
   node; or would cut this server off. A server's own certificate must still
   match a record (`with_local_identity`), and a server that listens for
   peers must also keep its record's `boot`, `address` and `tls_server_name`
   (`with_local_listener`), because its listener is bound to that address
   and the routes it has published carry those values. `MembershipError`
   says which rule and which record.
2. The directory is swapped, then the pin set. Both are single atomic
   replacements. A reader between the two sees the new directory with the
   old pins and applies both checks, so it can refuse a peer the new
   membership admits but cannot admit one that either membership refuses.
3. Connections that the new membership no longer covers are closed. This is
   an immediate close, not a drain.

After a reload returns:

- **A removed server** is refused on new inbound connections (its pin is
  gone, and the policy no longer finds its node) and is no longer dialled:
  a route naming it is `UntrustedRoute`. Connections already established
  with it, in both directions, are closed. A request in flight on one of
  them fails; for a write the outcome is unknown and it is not replayed, as
  with any lost peer connection. Remove a server only after stopping it or
  moving its volumes.
- **A re-keyed server** (same node, new `spki_sha256`) is treated the same
  way for its old key: connections made under the old pin are closed, and
  the next handshake must present the new key. A pin that has left the file
  does not keep working on an existing connection.
- **A server whose `boot`, `address` or `tls_server_name` changed** keeps
  its inbound connections if its pin is unchanged, but pooled outbound
  connections to its old address are closed and routes carrying the old
  values are no longer trusted.
- **An added server** is accepted and dialled from the next handshake.
- **Unchanged servers** keep their connections. A reload that describes the
  membership already in force changes nothing and closes nothing.

The capacity exists because the transports' connection limits are fixed when
they are built: size them from `membership.capacity()`, not from the number
of members at startup.

The ignored `membership_reload_admits_added_peers_and_cuts_off_removed_ones`
test covers this over real mTLS sockets: a peer added by reload forwards, a
removed peer has its connection closed and is refused, invalid and
self-excluding files change nothing, and a re-listed owner is used again
only under its real key.

The bounded route cache avoids a Redis round trip on each read. Entries are
short-lived hints, not ownership grants. An owner `not-dispatched` response
invalidates the exact cached route for the next request. No request is retried
inside this adapter, including writes with unknown outcomes. Clients cannot
supply internal owner/hop headers. URI authority and routing headers are rebuilt
from the verified route; drive bearer authorization is preserved for the owner.

Request upload and response download run concurrently with one absolute
operation deadline and transport backpressure. Dropping the response cancels
its receive direction and aborts any outstanding upload. Upload size is bounded;
a late overflow or stream failure is an unknown outcome because the owner may
already have acted. A successful response must also finish its upload. This is
streaming forwarding, not a durable write-idempotency protocol.

The ignored `redis_discovery_forwards_filesystem_over_http3` test requires an
isolated Redis URL and exercises the ingress library through real mTLS sockets:

```sh
MOUNT_RS_REDIS_TEST_URL=redis://127.0.0.1:6379/ \
  ./scripts/cargo-shared test -p mount-rs-cluster --features http3 --locked -- --include-ignored --nocapture
```

Stream cancellation/failure stress tests and combined TiDB/RustFS routed
tests remain outstanding. That fixture starts at the ingress library
boundary; the public listener, CLI wiring, local-owner bypass and membership
reload are covered by `scripts/serve-http-cluster-e2e.py`.

## Public ingress listener

`IngressForwarder` implements `mount_rs_http::HttpRequestHandler`; pass an
`Arc<IngressForwarder>` to `HttpServer::start_with_handler`. The normal listener
tracks connections and supports graceful close. Clients send only the public
filesystem URL and bearer token. Registry/peer failures return bounded redacted
errors, distinguishing `not-dispatched` from `unknown` outcomes; no private
endpoint or credentials appear in those responses.

Real socket tests now include public HTTP PUT, full/range GET, bearer refusal,
internal-header spoof refusal, and listener shutdown. Two ingress listeners
with independent route caches share the same owner and see each other's writes;
closing either ingress does not close the shared owner. The fixture uses one
process and a shared peer client. `mount-rs serve-http` provides the CLI
startup configuration, and the public surface it serves is specified in
`transports/mount-rs-http/openapi-cluster.yaml` and checked with real
processes by `apps/mount-rs-cli/tests/cluster_contract.rs`. Public TLS is the
reverse proxy's job, as for a single server. Native remote mounts remain
open.

`IngressForwarder::forward` returns the owner's answer as it was, including
an owner's refusal of a request it did not run (404, 409 or 503 with
`x-mount-dispatch: not-dispatched` and no body). The `HttpRequestHandler`
implementation, which is what a public listener uses, turns every such
refusal into the one public answer for a request that was not dispatched:
503 with `Retry-After: 1` and the standard error body. `ClusterIngress`
does the same for a drive the server owns itself, and answers paths that
name no drive (`/healthz`, `/readyz`, `/v1/drives`) from the drives the
owner router holds at that moment instead of forwarding them.

## Remote handle lifecycle foundation

`OwnedHandleSession` retains SDK handles for one authenticated session and owner
generation. IDs are session-local references, not authorization tokens. It
preserves inode identity and cursor serialization, rejects capacity overflow
before open side effects, and provides retryable explicit close/drain. The
future transport must own authentication, bounded session counts, expiration
and cleanup; dropping the table alone is not cleanup. Pending opens reserve capacity without holding the
table lock across backend work; cancellation releases the reservation and drain
waits for admitted opens. The HTTP session protocol built on it is
`SessionHttpHandler`, below.

`VolumeSessions` adds bounded per-volume session ownership, 256-bit OS-random
session tokens bound to authenticated client identities, idle-expiry admission
and explicit cleanup. Callers must authenticate identities independently, look
up each request, schedule expiry, enforce a server-wide resource budget and join
registry shutdown before filesystem shutdown. Cleanup failures retain their
slots for retry. No HTTP routes or automatic background task are created.

`VolumeSessions::run_expiry(cancel, cadence)` is an explicit caller-owned worker;
cadence must be positive and at most half the idle timeout. Cancellation drains
sessions. Abort or failure stops admission and retains cleanup state. Retain the
registry and retry `close` after failure/abort; do not interpret an interrupted
drain as successful cleanup or release the SDK first. The API creates no hidden
task and does not impose a backend-close timeout.

For multi-volume servers, construct one `SessionBudget` and pass clones to every
`VolumeSessions::new_with_budget` call. Pending creates and closing session
records consume capacity; successful cleanup releases it. The limit applies only
to participating registries and bounds session count, not cache or request-buffer
memory. Unbudgeted `new` remains a per-volume-only constructor.

`SessionCredentials` is a static owner-side bearer-token-to-principal map for
session HTTP routes. It rejects duplicate Authorization headers and never trusts
caller identity headers. Supply high-entropy tokens over encrypted transport;
length validation does not establish entropy. Debug redacts credential material.
`serve-http` uses it for the session routes of every drive when
`http.remote_sessions` is set. It does not manage TLS or rotation: tokens
are read at startup and change by restart.

`SessionHttpHandler` implements `HttpRequestHandler` for authenticated POST/DELETE
`/v1/drives/{volume}/sessions`. POST returns protocol version 1 and a session token;
DELETE takes `x-mount-session`. Requests must have an empty completed body, and
responses are no-store. The caller must serve encrypted transport and explicitly
drain the listener, registry and SDK in that order, while owning the expiry task.
This lifecycle handler can be enabled on `OwnerHttpHandler::with_sessions`; it
now also exposes bounded handle RPC at the `/handles` suffix. Lost creation responses are not safely
replayable; their bounded sessions are reclaimed by expiry.

Configure `OwnerHttpHandler::with_sessions` before sharing the handler. It creates
the registry for that exact owned SDK filesystem and volume, using the supplied
shared session budget. Obtain `session_registry()` to own its expiry worker.
Drain public and peer listeners before owner close; owner close then drains the
session registry before the caller shuts down the filesystem. Configuration is
opt-in; `mount-rs serve-http` turns it on with `http.remote_sessions: true`
and sets the limits from `http.sessions`.

Handle RPC uses authenticated POST JSON controls (open/read/stat/truncate/sync/
close; 8KiB maximum), and PUT raw writes (64KiB maximum) with `x-mount-handle`
and optional `x-mount-position`. A PUT with `x-mount-write-file` instead (the
path, percent-encoded; no handle or position header) replaces that file's
contents with the body in one request through the driver's `write_file`, and
answers 204; the capabilities reply advertises it as `"write_file": true`, and
a client must not send it to an owner that does not. All require
`x-mount-session`. Reads return raw
binary bytes, at most 64KiB. Bodies must finish within five seconds before any
operation dispatch. See the remote mount contract for fields and qualification
limits. No automatic replay or remote FsDriver is supplied yet.
# Request-scoped forwarding diagnostics

With `http3`, an in-process diagnostic handler can attach a fresh
`ForwardRequestTrace` to a request's extensions before calling the forwarder.
Keep a clone and call `snapshot()` after consuming the response. This collects
route lookup, pooled connection acquisition, stream opening, response headers,
body completion and total forwarding durations for that one request. It collects
no identifiers, credentials, headers or payload contents and sends no diagnostic
headers over HTTP. Ordinary requests allocate no trace or update trace atomics.

Missing stages are `None`, not zero. `complete` becomes true only after verified
EOF and required request-upload completion; cancellation, body errors or a
consumer that does not poll EOF leave it false. Body duration ends at the declared
final payload, or EOF for unknown-length bodies. Total duration includes consumer
polling delays and ends before downstream network delivery. Completed snapshots
publish preceding stage updates; incomplete snapshots may race further updates.
Use one fresh trace per request, and keep sampling/retention bounded in the
diagnostic consumer. The API does not itself retain a global sample collection.

These request-level samples support correlation of slow calls. Aggregate stage
percentiles still cannot be added or subtracted to reconstruct total p99.
`route_lookup_wait_ns` is the registry/per-volume admission substage of route
lookup, excluding the caller's Redis lookup. It is `None` for an immediate
snapshot-cache hit. Do not add this substage to `route_ns`.
