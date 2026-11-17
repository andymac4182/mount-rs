# serve-http threat model and hardening guide

What `mount-rs serve-http` protects, what it does itself, what the deployment
must provide, and a checklist. Every statement about server behaviour has a
code reference in the [appendix](#appendix-code-references) so it can be
checked.

Scope: the v1 launch configuration (`remote_sessions: false`, no
`forwarding`). The node-to-node boundary of a forwarding cluster is described
in [Cluster trust boundaries](#cluster-trust-boundaries) for completeness;
those features are experimental.

Related documents:

- Operator runbook (tokens, proxy, health): [`serve-http-runbook.md`](serve-http-runbook.md)
- Compatibility policy: [`serve-http-compatibility.md`](serve-http-compatibility.md)
- Upgrade and rollback: [`serve-http-upgrade.md`](serve-http-upgrade.md)
- Service levels and backup: [`serve-http-slo.md`](serve-http-slo.md)
- API guide: [`transports/mount-rs-http/README.md`](../transports/mount-rs-http/README.md)

## Summary

The server authenticates each request with one static bearer token per drive
and nothing else. It has no TLS, no user identity and no rate limiting, and
its access log is off by default and records no path, client or token. It
is built to sit on a loopback address behind a reverse proxy that supplies
those. A token is the whole of a caller's authority: whoever
holds a drive's token can read, replace and delete everything on that drive.

## Assets

| Asset | Where it lives | What compromise means |
| --- | --- | --- |
| Drive contents | The drive's storage: SQLite files, a host directory, or a metadata store plus a block store | Disclosure, modification or loss of files |
| Drive bearer tokens | The process environment at startup; process memory for its lifetime; the operator's secret store; every consumer that calls the drive | Full read and write access to that drive until the token is rotated, which needs a restart |
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
| Stolen token | None. A token does not expire and cannot be revoked without a restart | Rotate on suspicion; plan for the restart |
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
| Tokens come only from the environment | The config accepts `token` only as `{"env": "NAME"}`; an inline value is a config error. The variable is read once at startup, before any drive is opened. |
| Tokens are not disclosed by the process | Not printed on stdout or stderr, not returned by any route, redacted from the `Debug` form of a drive. CLI error messages pass through a filter that redacts URL user-info and values of keys such as `token`, `password`, `secret`, `authorization`. |
| `/healthz` and `/readyz` need no token | They return a status word, the number of configured drives and, when not ready, the number of failed drives. They never return a drive id. Responses carry `Cache-Control: no-store` and `X-Content-Type-Options: nosniff`. |

Not done by the server: rate limiting, lockout, logging of failed
authentication, token expiry, more than one valid token per drive, read-only
tokens, and clearing the token from memory after use.

### Network exposure

| Behaviour | Detail |
| --- | --- |
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
| Secret storage | Tokens are plain environment variables | Deliver through the service manager's secret mechanism (systemd `LoadCredential=` or an `EnvironmentFile=` with mode 0600; a Kubernetes Secret). Not the command line, not the config file. On Linux a process's environment is readable by the same user and by root through `/proc/<pid>/environ`; run the service as a dedicated user. |
| Token rotation | **Needs a restart.** A drive accepts one token; there is no reload and no overlap in which old and new both work | Follow the runbook's rotation steps. Budget an outage of one restart per rotation, and rotate on a schedule so that the procedure is practised. |
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
- **Token rotation without a restart, token expiry, revocation lists.**
- **Encryption at rest.** The transport stores what it is given; encryption
  is a property of the storage provider and its deployment.
- **Audit logging.**
- **Quotas per token or per client.**
- **Defence against a hostile operator or a hostile host.** Anyone who can
  read the process environment, the storage files or the metadata store has
  everything.

## Cluster trust boundaries

**Experimental; outside v1.** Summarised from
[`crates/mount-rs-cluster/README.md`](../crates/mount-rs-cluster/README.md)
and the forwarding code, so that an operator evaluating a cluster knows what
to isolate.

| Boundary | Mechanism | Notes |
| --- | --- | --- |
| Server to server (peer port, UDP) | HTTP/3 over QUIC with mutual TLS. A peer is accepted only if its certificate carries the peer role for a node listed in the membership file **and** its public key matches that node's `spki_sha256` pin. An ingress dialling an owner checks the same pin and node id. | The membership file is the root of trust. Distribute it as carefully as a private key. Changing membership needs a restart; live reload is not implemented. |
| Client to ingress | The same per-drive bearer token as v1. The ingress forwards it; the owner performs the bearer check. | Client-supplied `x-mount-owner-route` or `x-mount-hop` headers are refused. |
| Server to Redis | The Redis URL from the environment variable named by `forwarding.redis_url`. | **The CLI does not require TLS or authentication on this connection**: a `redis://` URL is accepted as readily as `rediss://`, and the repository's own cluster tests and benchmark use plaintext loopback Redis. Redis only advertises routes: a route is used only if it matches a pinned member, and the owner checks its own fencing generation before serving. A party that can write Redis can therefore misdirect or deny requests but cannot authorise itself as an owner. Treat Redis as a denial-of-service surface and isolate it. |
| Peer private keys | Files named by `forwarding.certificate` and `forwarding.private_key` | A stolen peer key plus network reach to the peer port allows impersonating that node to other servers. |

Not established by any test in the repository, per the cluster README:
production TLS and authorisation on Redis, Redis outage and failover
behaviour, network partitions between servers.

## Hardening checklist

Listener

- [ ] `host` is a loopback address, or `allow_non_loopback` is set **and** a
      network control admits only the proxy or load balancer to the port.
- [ ] `port` is fixed.
- [ ] `http2_prior_knowledge` is `false`.
- [ ] `remote_sessions` is `false` or absent and there is no `forwarding`
      block, unless you have accepted the experimental cluster features.

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
- [ ] Exactly one process per drive.

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
| Contract tests for wrong-token and malformed-header `401` | `transports/mount-rs-http/tests/launch_contract.rs:1005-1013`, `:1058-1066` |
| Token validity rule (non-empty, no whitespace, no minimum length) | `server.rs:153-158` |
| Token read from the environment before any drive is opened | `runtime.rs:943-959` |
| `token` must be `{"env": NAME}` | `config.rs:897`, `config.rs:2135-2150` |
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
