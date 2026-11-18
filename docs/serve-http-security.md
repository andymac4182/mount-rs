# serve-http threat model and hardening guide

What `mount-rs serve-http` protects, what it does itself, what the deployment
must provide, and a checklist. Every statement about server behaviour has a
code reference in the [appendix](#appendix-code-references) so it can be
checked.

Scope: one server (`remote_sessions: false`, no `forwarding`), and, in
[Cluster trust boundaries](#cluster-trust-boundaries), what remote sessions
and a forwarding cluster add: the session token, the server-to-server
boundary, Redis, and what a forwarding server reveals before it checks a
token.

Related documents:

- Operator runbook (tokens, proxy, health): [`serve-http-runbook.md`](serve-http-runbook.md)
- Compatibility policy: [`serve-http-compatibility.md`](serve-http-compatibility.md)
- Upgrade and rollback: [`serve-http-upgrade.md`](serve-http-upgrade.md)
- Service levels and backup: [`serve-http-slo.md`](serve-http-slo.md)
- API guide: [`transports/mount-rs-http/README.md`](../transports/mount-rs-http/README.md)

## Summary

The server authenticates each request with a bearer token per drive (two
while one is being rotated) and nothing else. It has no TLS, no user identity and no rate limiting, and
its access log is off by default and records no path, client or token. It
is built to sit on a loopback address behind a reverse proxy that supplies
those. A token is the whole of a caller's authority: whoever
holds a drive's token can read, replace and delete everything on that drive.

## Assets

| Asset | Where it lives | What compromise means |
| --- | --- | --- |
| Drive contents | The drive's storage: SQLite files, a host directory, or a metadata store plus a block store | Disclosure, modification or loss of files |
| Drive bearer tokens | The process environment at startup, or a token file on the host; process memory for as long as the token is accepted; the operator's secret store; every consumer that calls the drive | Full read and write access to that drive until the token is rotated: by a reload for a drive whose token is a file, by a restart otherwise |
| Storage credentials | Environment variables named by the driver config (for example a TiDB URL, object-store keys) | Direct access to stored data, bypassing the API and the writer lease |
| The writer lease | A row in the drive's metadata store | A party that can write the metadata store can break single-writer exclusion |
| Drive ids | The config file, URLs, the readiness line on stdout | Low. Ids are not secret, but the API does not reveal which ids exist to a caller without a valid token |
| Peer private keys, the membership file, the Redis route registry | Cluster deployments only | See [Cluster trust boundaries](#cluster-trust-boundaries) |
| Availability | The process, its connection slots, its upload memory budget | Denial of service to all drives in the process |

## Trust boundaries

```text
 consumer ──TLS──▶ reverse proxy / load balancer ──cleartext HTTP──▶ serve-http listener
                                                                          │
                                           ┌──────────────────────────────┼─────────────────────┐
                                           ▼                              ▼                     ▼
                                    metadata store                  block store          local disk
                                 (SQLite file, TiDB, …)       (SQLite file, object      (SQLite files,
                                                                store, local dir)        host directory,
                                                                                          disk cache)
```

| Boundary | What crosses it | Protected by | Who provides the protection |
| --- | --- | --- | --- |
| Consumer to proxy | Token, file contents | TLS | **Deployment** |
| Proxy to listener | Token, file contents, in cleartext | Loopback by default: the listener refuses a non-loopback host unless `allow_non_loopback` is set | Server by default; **deployment** once `allow_non_loopback` is set |
| Listener to request handling | Untrusted paths, headers and bodies | Path validation, bearer check, size and time limits | Server |
| Process to metadata store and block store | Storage credentials, file contents, the lease | Whatever the provider's client is configured with (TLS, credentials) | **Deployment**. The transport adds nothing here |
| Process to local disk | SQLite files, host directory, cache files | File ownership and permissions | **Deployment** |
| Operator to process | Config file, token environment, signals | Host access control | **Deployment** |
| Load balancer to `/healthz`, `/readyz` | Drive count, failed-drive count | Nothing: these routes are unauthenticated | **Deployment** restricts who can reach them |

## Threats and where each is handled

| Threat | Server | Deployment |
| --- | --- | --- |
| Eavesdropping on tokens or file contents | No TLS. Binds loopback only unless told otherwise | Terminate TLS; keep the proxy-to-listener hop on loopback or a private network |
| Guessing a token online | Non-short-circuiting comparison; identical `401` for every failure. **No rate limit, no lockout, no log of failed attempts** | Rate-limit and alert on `401` at the proxy; use tokens of at least 32 random bytes |
| Discovering which drives exist | Unknown drive and wrong token give the same response. See the timing caveat below | Do not treat drive ids as secrets |
| Using drive A's token on drive B | Each token is compared only against the addressed drive's token | Give each drive its own token |
| Path traversal out of a drive | Segment-wise validation rejects `.`, `..`, encoded separators, NUL and non-UTF-8 before authentication; JSON-body paths are validated the same way | Forward the request URI unmodified, so the proxy does not decode what the server would reject |
| Escaping a `host` drive's root through a symlink | The host driver walks every component below the root and rewrites symlink targets back into it | See the race caveat below; do not let untrusted local users change the directory tree under a served root |
| Memory exhaustion by large or many bodies | Declared length checked before the body is read; bounded total upload memory; bounded listings | Cap body size and connection count at the proxy; set the process memory limit above twice `max_buffered_upload_bytes` |
| Connection exhaustion, slow clients | `max_connections`; header-read, request and stalled-download timeouts | Per-client connection and request limits at the proxy. **The server has no per-client limit**: one client can hold every slot |
| Leaking a token through logs or errors | Tokens are never printed; `Debug` output is redacted; stderr diagnostics pass through a redaction filter; error bodies carry only a POSIX code and a generic description | Do not log the `Authorization` header at the proxy; keep tokens off command lines |
| Leaking storage details to a client | A storage failure is reported as its POSIX code with a fixed description; the underlying message is not sent | |
| Cross-site requests from a browser | No CORS headers are ever sent, and there is no cookie authentication | Do not add CORS headers at the proxy |
| A second process corrupting a drive | Split-store drives hold an exclusive writer lease and fail closed when it cannot be renewed | For `sqlite`-kind and `host` drives there is no lease: run exactly one process |
| Tampering with stored data underneath the server | The runbook records that a stored block whose content does not match its hash stops a start with exit code 1. The extent of integrity checking on reads was not examined for this document | Restrict access to the stores; keep backups |
| Stolen token | A token does not expire. A drive whose token is a file drops it at the next reload (`SIGHUP`, or the timer); a drive whose token is an environment variable needs a restart | Rotate on suspicion. Use `token.file` for drives where a restart to revoke is not acceptable |
| Compromised binary or image | Release images carry a provenance attestation | Verify it before deploying (`gh attestation verify`, see the runbook) |

## What the server does itself

Each row is backed by the appendix.

### Authentication

| Behaviour | Detail |
| --- | --- |
| One bearer token per drive | `Authorization: Bearer <token>`. The scheme name is case-insensitive; the token is compared byte for byte. |
| Token comparison does not stop at the first differing byte | The loop runs over the longer of the two inputs and accumulates differences. **Caveat:** its running time grows with the length of the presented token once that exceeds the real token's length, so the comparison does not hide the token's length. With a random token of 32 bytes or more this gives an attacker nothing useful. |
| One response for every authentication failure | Missing header, malformed header, wrong scheme, empty token, token containing whitespace, wrong token and unknown drive id all produce the same `401`, code `EACCES`, message `bearer authorization required`, with `WWW-Authenticate: Bearer`. |
| **Caveat: the responses are identical, the work done is not** | For an unknown drive id the server finds no drive and performs no token comparison; for a known drive it performs one. A caller able to measure sub-microsecond differences over a network could in principle tell the two apart. The server makes no attempt to equalise this. Drive ids should not be treated as secrets. |
| A token is never accepted for another drive | The comparison is made against the token of the drive named in the URL only. `GET /v1/drives` lists exactly the drives whose token equals the presented one, and answers `401` if there are none. |
| Authentication comes after path and route checks | A malformed path is `400` and an unknown route is `404` without a token. Neither response depends on which drives exist. A `405` on a drive route is only sent to an authenticated caller. |
| Token validity rule | Non-empty, no ASCII whitespace. **There is no minimum length and no entropy check in the v1 configuration**: a one-character token is accepted. (With `remote_sessions` a 32-256 character format is enforced; that is outside v1.) |
| Tokens come only from the environment or a token file | The config accepts `token` only as `{"env": "NAME"}` or `{"file": "PATH"}`; an inline value is a config error. The variable or file is read at startup, before any drive is opened. |
| A reloaded token file cannot weaken a drive | A file is taken whole or not at all: one or two tokens, each by the same rule as at startup. A file that is missing, unreadable or not valid leaves the accepted tokens as they were. A drive never accepts more than two tokens and never none. |
| Every accepted token is compared in constant time | Each of a drive's tokens is compared in full whether or not an earlier one matched, so the time taken does not say which token a guess resembles. A dropped token gets the same `401` as one that was never valid. |
| A reload writes no token | The `http_token_reload` line and every refusal name the drive, the file and a line number, never the file's contents. |
| Tokens are not disclosed by the process | Not printed on stdout or stderr, not returned by any route, redacted from the `Debug` form of a drive. CLI error messages pass through a filter that redacts URL user-info and values of keys such as `token`, `password`, `secret`, `authorization`. |
| `/healthz` and `/readyz` need no token | They return a status word, the number of configured drives and, when not ready, the number of failed drives. They never return a drive id. Responses carry `Cache-Control: no-store` and `X-Content-Type-Options: nosniff`. |

Not done by the server: rate limiting, lockout, logging of failed
authentication, token expiry, more than one valid token per drive, read-only
tokens, and clearing the token from memory after use.

### Network exposure

| Behaviour | Detail |
| --- | --- |
| Metrics are off, apart and on loopback | Nothing serves metrics unless `http.metrics` is set. They are then served by a second listener, never the API's, so that publishing the API cannot publish them. That listener asks for no token, and for that reason refuses an address that is not loopback, in the config parser (exit 2) and again before it binds, unless `metrics.allow_non_loopback` is set. |
| Metrics name no path and no credential | A label is a route template, a method from a fixed list, a status class, a configured drive id or the build version. Nothing a client sent is repeated, and a client cannot create series. Drive ids and the build version are disclosed to whoever can scrape. |
| Loopback only by default | The default host is `127.0.0.1`. A host that is not `localhost` or a loopback IP is rejected twice: by the config parser (exit 2) and again by the listener before it binds. |
| `allow_non_loopback` removes both checks | The listener then binds whatever address it is given. It is still cleartext HTTP. |
| No TLS | The listener is a plain TCP socket served as HTTP/1.1 (or cleartext HTTP/2 with `http2_prior_knowledge`). There is no TLS code path in the transport. |
| No CORS | The server never emits an `Access-Control-*` header and has no handling for preflight requests; an `OPTIONS` request to a `/v1` route gets `401` without a valid token and `405` with one. |

### Request limits

| Limit | Enforcement |
| --- | --- |
| Concurrent connections (`max_connections`, 256) | A semaphore; a connection accepted beyond it is dropped at once, with no HTTP response. |
| Time to send request headers (`request_timeout_ms`, 30 s) | hyper's header-read timeout; the connection is closed without a response. The runbook states that the same timer closes an idle keep-alive connection. |
| Time for one request (`request_timeout_ms`) | The whole handler runs under a timeout and answers `504`. A streamed upload is exempt from the overall limit; instead each body piece, each drive write and the publication must complete within the timeout. |
| Declared body size | `Content-Length` above `max_upload_bytes` (file `PUT`) or `max_request_bytes` (anything else) is refused with `413` after authentication and before the body is used. |
| Actual body size | A JSON body is counted as it is read and refused with `413` past `max_request_bytes`, whatever `Content-Length` said. |
| Refused bodies | Up to 64 MiB or 5 s of a refused request's body is read and discarded so the client sees the refusal; after that the connection is closed. This applies to unauthenticated requests too, so an unauthenticated caller can make the server read up to 64 MiB per request. The bytes are dropped, not buffered. |
| Upload memory (`max_buffered_upload_bytes`) | A semaphore over all in-flight uploads. Uploads beyond it wait, up to the request timeout. |
| Listings | A directory with more than `max_directory_entries` entries, or whose JSON exceeds `max_response_bytes`, is refused with `413`. At most four listings are gzip-compressed at a time. |
| Request header size and count | Not configured by the server. hyper's defaults apply; they were not examined for this document. |

### Path handling

| Behaviour | Detail |
| --- | --- |
| Only the URL path is used | The query string is ignored. |
| One percent-decode per segment, after splitting on `/` | An encoded `/` therefore cannot introduce a new segment; it is rejected. |
| Rejected with `400 EINVAL` before authentication | Empty segments (other than one trailing `/`), `.` and `..` (also when percent-encoded), a segment containing `/`, `\` or NUL after decoding, bytes that are not UTF-8, and any segment starting with `.mount-rs-upload.`. |
| JSON-body paths (`mkdir`, `rename`, `truncate`) | Must start with `/`, must not contain `\` or NUL, and are checked segment by segment for empty, `.`, `..` and reserved names. |
| Drive ids | 1-64 characters from `A-Z a-z 0-9 - _ .`, not `.` or `..`, validated when the config is parsed. |
| What the transport does not do | It hands the validated virtual path to the drive's driver. Confinement below that point is the driver's job. |

`host` drives: the host driver resolves each component below the configured
root and rewrites symlink targets back into the root, so a symlink cannot
point outside it. Its own documentation states the limit: resolution and the
final system call are separate steps, so a component renamed between them is
not caught, and the driver "is not a sandbox against concurrent host
namespace changes". Serve a host directory only if no untrusted local process
can modify its tree.

### Errors and disclosure

- Every server-generated error is the JSON envelope with a POSIX `code` and a
  `message`. For filesystem and storage errors the message is the fixed
  description of the code (for example `no such file or directory`), not the
  underlying error text.
- By default the server writes no access log and no per-request error log:
  nothing about a request is written anywhere. With `"access_log": true` it
  writes one line per request to stderr holding the request id, method,
  route template (`/v1/drives/{id}/fs/{path}`, never the path), status,
  duration, byte counts and, only when the request named a configured
  drive, that drive's id. The path, the query string, the token, every
  request header and the client address are never written, with the log on
  or off.
- Every response carries `X-Request-Id`. A client-supplied value is echoed
  only if it is one header of 1 to 128 characters from `A-Z a-z 0-9 . _ -`;
  anything else is replaced with a generated id, so the header cannot be
  used to inject text into a response, a log line or a forwarded request.
  The id is an identifier, not a credential, and generated ids are not
  designed to be unguessable.
- With the optional `observability` feature, the request span records the
  shape of the path (whether rooted, its depth, a size bucket), not the path.

## What the deployment must provide

| Need | Why the server cannot | How |
| --- | --- | --- |
| TLS | No TLS in the listener | Terminate at a reverse proxy in the same network namespace, or at a load balancer in front of a private network with `allow_non_loopback`. The runbook has nginx and Caddy configurations. |
| Rate limiting and brute-force detection | Not implemented | Limit requests and concurrent connections per client address at the proxy. Alert on the `401` rate. |
| Per-client connection limits | The server has one global connection limit | At the proxy. Keep the proxy's upstream pool below `max_connections`. |
| Access and audit log | The server's optional access log has no path, no client address and no caller identity, so it is not an audit log | The proxy's access log: method, path, status, upstream time, bytes. Never the `Authorization` header. There is no record of which token made a request unless the proxy adds one (for example a hash of the header). |
| Secret storage | Tokens are plain environment variables, or lines in a file whose permissions the server does not check | Give a token file mode 0600 and the service user as owner, or mount it as a secret. Deliver variables through the service manager's secret mechanism (systemd `LoadCredential=` or an `EnvironmentFile=` with mode 0600; a Kubernetes Secret). Not the command line, not the config file. On Linux a process's environment is readable by the same user and by root through `/proc/<pid>/environ`; run the service as a dedicated user. |
| Token rotation | Without a restart, and with an overlap in which old and new both work, **only for a drive whose token is a file**, with or without `remote_sessions` and `forwarding`. A drive whose token is an environment variable still needs a restart and has no overlap. In a cluster the file is per server: the rotation must be made on every server that lists the drive, and nothing checks that they agree | Follow the runbook's rotation steps. Rotate on a schedule so that the procedure is practised. Remove the old token's line when consumers have moved: nothing expires it for you. |
| Network isolation of the metrics listener | With `metrics.allow_non_loopback` anything that can reach that port reads the drive ids, the version, request rates and which drives are down, without a token | Leave it on loopback and scrape from the same host or pod. Otherwise admit only the scraper, by security group or NetworkPolicy. Never route it through the public proxy. |
| Network isolation of the listener | With `allow_non_loopback` anything that can reach the port can try tokens and read cleartext traffic | A security group or NetworkPolicy that admits only the load balancer. |
| Protection of `/healthz` and `/readyz` | Unauthenticated by design | Allow them from the load balancer and monitoring only. |
| Isolation and credentials for the metadata and block stores | The transport passes the configured connection through | Private network, TLS and least-privilege credentials for TiDB and object stores. The TiDB user needs DDL on the mount-rs tables because schema steps run at open. |
| File permissions | Not managed by the server | SQLite database files, the host directory, the disk-cache directory and the config file owned by the service user, not world-readable. The container image runs as uid and gid 10001 with `/var/lib/mount-rs` as its volume. |
| Supply chain | | Pin the image by digest and verify its attestation. |
| Backups | No backup command | See [`serve-http-slo.md`](serve-http-slo.md#backup). |

## Known non-goals

These are absent by design in v1, as listed under "Not supported at launch"
in the API guide. Do not build on an assumption that they exist.

- **Per-user identity, scopes or roles.** A token is a drive-wide read-write
  capability. Two consumers of one drive cannot be told apart by the server.
- **Read-only tokens.**
- **CORS and browser clients.** A token in a browser is a token disclosed.
- **TLS, mutual TLS or client certificates on the public listener.**
- **Token expiry and revocation lists.** A token is accepted until the
  operator removes it.
- **Distributing a token file to the servers of a cluster.** Each server
  that lists a drive reads its own file; keeping them the same is the
  operator's job.
- **Closing a session at the moment its token is dropped.** The token is
  refused at once; the session it opened is closed by idle expiry.
- **Encryption at rest.** The transport stores what it is given; encryption
  is a property of the storage provider and its deployment.
- **Audit logging.**
- **Quotas per token or per client.**
- **Defence against a hostile operator or a hostile host.** Anyone who can
  read the process environment, the storage files or the metadata store has
  everything.

## Cluster trust boundaries

What `remote_sessions` and `forwarding` add to the model above. Operating
detail is in the [cluster chapter of the runbook](serve-http-runbook.md#clusters).

| Boundary | Mechanism | Notes |
| --- | --- | --- |
| Client to any server | The same per-drive bearer token. A server that does not own the drive forwards the request, token included, to the owner; **the owner performs the bearer check.** | Client-supplied `x-mount-owner-route` or `x-mount-hop` headers are refused with `400` before anything is forwarded. |
| Session token | 256 random bits from the operating system, sent once in the `createSession` response and afterwards only in the `x-mount-session` header. Held in the owner's memory only. | It is not a credential by itself: every session request also needs the drive's bearer token, and a session is usable only with the token that created it: during a rotation the other accepted token does not reach it, and once its token is dropped nothing does. It ends on close, idle timeout, owner restart or failover. Proxies must not log `x-mount-session`. |
| Drive tokens with `remote_sessions` | The server refuses to start unless each is 32-256 characters from a fixed set, and refuses a reloaded token file whole if one of its tokens is not. | Length is checked, entropy is not. The file routes and the session routes read the same token set, so a reload changes both at the same moment. |
| Drive tokens with `forwarding` | Only a server that lists a drive holds its tokens. An ingress has none and checks none; the owner checks every request, forwarded or not, against the tokens it accepts when the request reaches it. | A rotation is made on each server that lists the drive (every failover candidate), and a standby reloads its file like an owner. Until all of them have reloaded, which token works depends on which candidate owns the drive. |
| Server to server (peer port, UDP) | HTTP/3 over QUIC with mutual TLS. A peer is accepted only if its certificate chains to `forwarding.ca`, carries the peer role for a node listed in the membership file, **and** its public key matches that node's `spki_sha256` pin. A server dialling an owner checks the same. | The membership file is the root of trust: distribute it as carefully as a CA certificate. It is reloaded on `SIGHUP` and on a timer; a removed or re-keyed server has its open connections closed at once. One record holds one pin, so a key rotation has no overlap period. |
| Server to Redis | The URL in the environment variable named by `forwarding.redis_url`. `rediss://` is verified against the system roots or `redis_ca`, and verification cannot be turned off. `redis://` is refused for any host but loopback unless `allow_plaintext_redis` is set. The URL and its password are never logged. With `forwarding.redis_sentinel` the same rules hold for every sentinel and for the Redis server a sentinel names: TLS unless `tls` is `false`, the same trust roots, no plaintext to another host without the opt-in, and passwords taken from environment variables and never logged. A sentinel is trusted only to name an address; the server at that address must pass the same TLS and login checks and report the `master` role. | Redis only advertises routes and candidate liveness: a route is used only if it matches a pinned member, and the owner checks its own fence before serving. A party that can write Redis can misdirect or deny requests and can delay a failover, but cannot make a server serve a drive or accept a peer. Treat Redis as a denial-of-service surface: authenticate it and isolate it. |
| Peer private keys | Files named by `forwarding.certificate` and `forwarding.private_key`, read once at startup | A stolen peer key plus network reach to a peer port allows impersonating that node to the other servers, which includes receiving forwarded requests with their bearer tokens. Removing or re-pinning the node in the membership file revokes it at the next reload. |

Differences from a single server that matter for security:

- **An unknown drive id is `503`, not `401`, on a forwarding server**, and
  the route is looked up before the token is checked. An unauthenticated
  caller can therefore learn whether a drive id has an owner at the moment
  (a `401` from the owner) or not (the `503`). On a single server both
  cases are the same `401`. Do not treat drive ids as secrets in a cluster,
  or keep the cluster's public listeners behind an authenticating proxy.
- **Forwarding sends the bearer token to the owner** inside the mutually
  authenticated connection. Every server in the membership file can
  therefore see the tokens of requests it is asked to forward or serve.
  The membership is one trust domain.
- `/healthz` and `/readyz` stay unauthenticated on every server and reveal
  how many drives that server owns, not their ids.

Tested: the pin and node checks on both sides, refusal of spoofed internal
headers, membership reload including removal and re-keying
(`crates/mount-rs-cluster` tests and `scripts/serve-http-cluster-e2e.py`);
Redis TLS with a private CA, password and ACL logins, refused plaintext, and
outages (`apps/mount-rs-cli/tests/redis_connection.rs`); the session
authentication cases and the reserved-header refusal through real servers
(`apps/mount-rs-cli/tests/cluster_contract.rs`); servers cut off from
Redis during a failover (`apps/mount-rs-cli/tests/failover.rs`); a Redis
Sentinel deployment with TLS, a private CA and separate passwords for the
sentinels and the Redis servers, a wrong one of each refused at startup
without being printed, and its primary killed and replaced
(`apps/mount-rs-cli/tests/redis_sentinel.rs`). Not tested: a partition of
the server-to-server link, a partition inside a Redis Sentinel deployment,
a sentinel that names a server it should not, and a hostile peer that
holds a valid key.

## Hardening checklist

Listener

- [ ] `host` is a loopback address, or `allow_non_loopback` is set **and** a
      network control admits only the proxy or load balancer to the port.
- [ ] `port` is fixed.
- [ ] `http2_prior_knowledge` is `false`.
- [ ] `remote_sessions` and `forwarding` are absent, or the items under
      "Cluster" below are done.

Tokens

- [ ] Each drive has its own token of at least 32 random bytes
      (`openssl rand -base64 32 | tr '+/' '-_' | tr -d '='`). The server does
      not enforce this.
- [ ] No two drives share a token unless one consumer owns both.
- [ ] Tokens are delivered through a secret mechanism with restricted
      permissions; none appears in the config file, a unit file, a command
      line, an image or a repository.
- [ ] The service runs as a dedicated, unprivileged user.
- [ ] A rotation has been rehearsed, including the consumer's reaction to
      `401`.
- [ ] There is a named owner and a schedule for rotation.

Proxy

- [ ] TLS terminates at the proxy with a current protocol and cipher policy.
- [ ] The request URI is forwarded unmodified (the runbook's `a%2Fb` test
      returns the server's `400`).
- [ ] `Authorization` is forwarded and is not logged, cached or echoed.
- [ ] Responses are not cached.
- [ ] Body size cap equals `max_upload_bytes`.
- [ ] Per-client request rate and connection limits are set.
- [ ] Upstream connections stay below `max_connections`.
- [ ] No automatic upstream retries.
- [ ] `/healthz` and `/readyz` are reachable only from the load balancer and
      monitoring.
- [ ] No CORS headers are added.
- [ ] Access log is on and shipped somewhere the service user cannot alter.

Storage

- [ ] Database files, host directories and cache directories are owned by the
      service user and not readable by others.
- [ ] SQLite files are on local disk, not NFS or SMB.
- [ ] No untrusted local process can modify the tree under a `host` drive.
- [ ] Remote metadata and block stores are reached over TLS on a private
      network with credentials scoped to this service.
- [ ] No `memory` drive; no `writeback` unless consumers accept it.
- [ ] Exactly one process per drive, or every process that lists it is a
      failover candidate with identical storage for it.

Cluster (only with `remote_sessions` or `forwarding`)

- [ ] Every drive token is at least 32 random characters; a rotation has
      been rehearsed on every server that lists the drive, including what
      clients with open sessions do when their token is dropped.
- [ ] The proxy does not log `x-mount-session`.
- [ ] The peer port (UDP) of each owner is reachable only from the other
      servers.
- [ ] The membership file and the peer private keys are readable only by
      the service user, and the file is replaced by rename.
- [ ] Redis is reached with `rediss://` and a password or ACL user, or is
      on loopback; `allow_plaintext_redis` is not set.
- [ ] Redis is not reachable by anything but the servers.
- [ ] Drive ids are not treated as secrets (see
      [Cluster trust boundaries](#cluster-trust-boundaries)).
- [ ] The [known limits](serve-http-runbook.md#known-limits) have been
      read.

Operations

- [ ] Alerts on `401` rate, `5xx` rate and `/readyz`, as in the runbook's
      alert table.
- [ ] The image is pinned by digest and its attestation verified.
- [ ] A cold backup and restore has been rehearsed.
- [ ] `validate-config` runs in the deployment pipeline.

## Appendix: code references

Checked at the revision this document was added in. `server.rs` is
`transports/mount-rs-http/src/server.rs`; `config.rs` is
`apps/mount-rs-cli/src/config.rs`; `runtime.rs` is
`apps/mount-rs-cli/src/runtime.rs`.

| Claim | Where |
| --- | --- |
| Token comparison accumulates over the longer input, no early exit | `server.rs:2894-2901`; used at `server.rs:168-170` |
| Bearer parsing: scheme case-insensitive; empty or whitespace token rejected | `server.rs:2880-2892` |
| The single `401` response | `server.rs:978-988`; `WWW-Authenticate` added at `server.rs:3058-3060` |
| Unknown drive and wrong token take the same branch; no comparison for an unknown drive | `server.rs:1357-1366` |
| `GET /v1/drives` lists only drives whose token matches; `401` otherwise | `server.rs:1451-1473` |
| Order: closing, route parse (path syntax, unknown route), health and discovery, drive and token, declared length, then method inside each handler | `server.rs:1311-1407`; method check example `server.rs:1415-1417` |
| Contract tests for wrong-token and malformed-header `401` | `transports/mount-rs-http/tests/suites/launch_contract.rs`, `an_unknown_drive_is_indistinguishable_from_a_wrong_token`; run directly and behind a handler |
| Token validity rule (non-empty, no whitespace, no minimum length) | `server.rs:153-158` |
| Token read from the environment or the token file before any drive is opened | `runtime.rs`, `serve_http_command` |
| `token` must be `{"env": NAME}` or `{"file": PATH}` | `config.rs`, `parse_token_source` |
| A token file is validated whole before it replaces the accepted tokens; a bad one changes nothing | `apps/mount-rs-cli/src/token_reload.rs`; `DriveTokens::replace` in `server.rs` |
| Every accepted token compared without short-circuit | `DriveTokens::matches` in `server.rs` |
| Tests for the overlap, the drop and the identical `401` | `launch_contract.rs`, `a_drive_accepts_each_of_its_tokens_until_the_set_is_replaced`; `apps/mount-rs-cli/tests/http_subprocess.rs`, `a_token_file_is_rotated_without_a_restart` |
| The same with `remote_sessions`, on an owner, through an ingress and on a failover standby; a session is bound to its token and closed by idle expiry after the token is dropped | `apps/mount-rs-cli/tests/cluster_operations.rs`; `SessionCredentials::for_drive` in `crates/mount-rs-cluster/src/session_auth.rs`, `drive_credentials_follow_the_drive_tokens_as_they_are_replaced` |
| `Debug` redaction of the token, with a test | `server.rs:99-107`, `server.rs:3643-3650` |
| CLI diagnostics are redacted | `config.rs:2184-2200`; applied at `runtime.rs:45`, `runtime.rs:52` |
| Health responses: fields, no drive ids, no authentication | `server.rs:1475-1509` |
| Health response headers | `server.rs:3110-3122` |
| Default host `127.0.0.1`, `allow_non_loopback` default `false` | `server.rs:246-266` |
| Loopback check in the config parser | `config.rs:648-655`, `config.rs:870-880` |
| Loopback check again before bind; skipped with `allow_non_loopback` | `server.rs:593-595`, `server.rs:2946-2961` |
| Plain TCP listener, HTTP/1.1 or prior-knowledge HTTP/2, no TLS | `server.rs:599-601`, `server.rs:717-730` |
| No CORS | No occurrence of `access-control` in `transports/mount-rs-http/src`, `apps/mount-rs-cli/src` or `crates/mount-rs-cluster/src` |
| Connection limit; excess connections dropped | `server.rs:522`, `server.rs:640-643` |
| Header-read timeout | `server.rs:725-729` |
| Request timeout and the streamed-upload exemption | `server.rs:1290-1309` |
| Declared-length check after authentication | `server.rs:1367-1372`; limits chosen at `server.rs:2930-2936` |
| Actual body counted while read | `server.rs:2655-2676` |
| Refused bodies drained up to 64 MiB or 5 s, including on `400`, `404` and `401` | `server.rs:2679-2716`; called at `server.rs:1334`, `:1365`, `:1371` |
| Upload memory budget | `server.rs:503`, `server.rs:2051-2069` |
| Listing limits | `server.rs:1557-1565`, `server.rs:2412-2416`; compression limit `server.rs:2422` |
| Only the URL path is routed | `server.rs:1332` |
| URL path validation | `server.rs:2763-2797` |
| JSON-body path validation | `server.rs:2807-2838` |
| Reserved name prefix | `server.rs:55`, `server.rs:1064-1066` |
| Drive id rule | `server.rs:138-150`; applied at parse time `config.rs:895-896` |
| Unknown JSON body properties are ignored (no `deny_unknown_fields`) | `server.rs:1180-1198` |
| Filesystem errors report the code's fixed description | `server.rs:3008-3019`; status mapping `server.rs:3072-3094` |
| Telemetry records path shape only | `crates/mount-rs-observability/src/lib.rs:227-247` |
| Host driver confinement and its stated race | `filesystems/mount-rs-host/src/lib.rs:1-13` |
| Container user 10001 | `Dockerfile:34-37` |
| Peer accepted only with matching role and SPKI pin | `crates/mount-rs-cluster/src/forwarding.rs:101-108` |
| Ingress checks the owner's pin and node id when dialling | `crates/mount-rs-cluster/src/forwarding.rs:646-651` |
| Client-supplied internal headers refused | `crates/mount-rs-cluster/src/forwarding.rs:563-567` |
| Redis URL opened as given; no TLS requirement | `apps/mount-rs-cli/src/http_sessions.rs:403-420`; TLS support compiled in at `apps/mount-rs-cli/Cargo.toml:32` |
| Statements on rotation, proxy requirements, attestation, lease behaviour | [`serve-http-runbook.md`](serve-http-runbook.md) "Tokens", "Reverse proxy", "In a container", "Health checks" |
