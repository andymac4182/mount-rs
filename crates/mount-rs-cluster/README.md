# mount-rs-cluster

Server-side advisory volume routing. Clients never connect to Redis. An ingress
server resolves a volume route and forwards directly to the authenticated owner.
TiDB grants ownership and fencing generations; Redis only advertises them.

`RedisRouteRegistry` accepts a caller-established authenticated TLS Redis
connection, a deployment namespace and a bounded request timeout. Register a
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
of the u64 range, stale cleanup, exact refresh, expiry and lookup. The current
acceptance used disposable Redis 8.4.0, loopback plaintext; production TLS,
authorization, outages and full server lifecycle remain separate acceptance work.

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
supervisor before filesystem shutdown. CLI configuration is not wired yet.
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
1–100 attempts per outage. It uses capped exponential backoff, publishes
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
failover qualification. Fleet deployment tuning remains open. This
library API is not yet wired into production CLI/server startup.

Use `RedisRouteRegistry::with_manager` with a caller-configured
`redis::aio::ConnectionManager` when recovery must survive a dropped socket.
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
resolution and ingress forwarding across three processes. Production
`serve-http` startup still needs to wire the peer listener, authenticated peer
directory, and registration supervisor into the configured service lifecycle.

## Ingress forwarding

`IngressForwarder::forward` accepts an HTTP request body, discovers the volume
route on the server, verifies it against a bounded operator-authenticated
`PeerDirectory`, and streams to the owner using a shared `PeerClient`. The peer
certificate must match the configured public-key pin and node role. Redis cannot
supply its own trusted endpoint, boot identity or certificate. Replace the
configured directory/forwarder when membership changes; live membership reload
is not yet implemented.

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

The public ingress listener, CLI wiring, local-owner bypass, membership refresh,
stream cancellation/failure stress tests and combined TiDB/RustFS routed tests
remain outstanding. The current fixture starts at the ingress library boundary.

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
process and a shared peer client. Production public TLS, independent-process
failure testing, CLI startup configuration and native remote mounts remain open.

## Remote handle lifecycle foundation

`OwnedHandleSession` retains SDK handles for one authenticated session and owner
generation. IDs are session-local references, not authorization tokens. It
preserves inode identity and cursor serialization, rejects capacity overflow
before open side effects, and provides retryable explicit close/drain. The
future transport must own authentication, bounded session counts, expiration
and cleanup; dropping the table alone is not cleanup. Pending opens reserve capacity without holding the
table lock across backend work; cancellation releases the reservation and drain
waits for admitted opens. This is not an implemented HTTP session protocol or
remote driver.

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
The helper is not yet wired into routes and does not manage TLS or rotation.

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
opt-in and not yet exposed by the production CLI startup path.

Handle RPC uses authenticated POST JSON controls (open/read/stat/truncate/sync/
close; 8KiB maximum), and PUT raw writes (64KiB maximum) with `x-mount-handle`
and optional `x-mount-position`. Both require `x-mount-session`. Reads return raw
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
