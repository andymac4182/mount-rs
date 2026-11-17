# serve-http compatibility policy

What a consumer and an operator of `mount-rs serve-http` may rely on across
releases, what may change without notice, and how a breaking change is
introduced.

Related documents:

- API guide: [`transports/mount-rs-http/README.md`](../transports/mount-rs-http/README.md)
- Machine-readable contract: [`transports/mount-rs-http/openapi.yaml`](../transports/mount-rs-http/openapi.yaml) (`info.version` `1.0.1`),
  checked against a running server by
  `transports/mount-rs-http/tests/openapi_conformance.rs` on every change
- Operator runbook: [`serve-http-runbook.md`](serve-http-runbook.md)
- Upgrade and rollback: [`serve-http-upgrade.md`](serve-http-upgrade.md)
- Security: [`serve-http-security.md`](serve-http-security.md)
- Service levels and backup: [`serve-http-slo.md`](serve-http-slo.md)

## Status of this policy

| Part | Status |
| --- | --- |
| The description of what v1 covers and what clients must tolerate | Descriptive. It restates the existing contract documents and the code. |
| The deprecation window, the `/v2` procedure and the announcement channels | **Proposed policy.** Not yet adopted by the project owner, and not yet exercised: no breaking change has been made to v1 and no `/v2` route exists in the code. |
| Cluster features (forwarding, remote sessions) | **Not covered.** See [Cluster features](#cluster-features). |

Where this document and the contract documents disagree about present
behaviour, the contract documents and the code win, and this document has a
bug.

## Version numbers

They are independent. Do not infer one from another.

| Number | Where | Current value | Changes when |
| --- | --- | --- | --- |
| API version | URL prefix `/v1` | `v1` | A breaking change to the HTTP API |
| Contract document version | `openapi.yaml` `info.version` | `1.0.1` | Any change to the document. Proposed rule: additions raise the minor part, corrections raise the patch part, and the major part equals the API version. |
| Config file version | Top-level `"version"` in the JSON config | `1` | A breaking change to the config format |
| Binary version | `mount-rs --version` | Cargo package version | Every release |

The config parser accepts exactly one config version. Any other value is
rejected with `unsupported version N; expected 1` and exit code 2.

## What v1 covers

The v1 contract is plain `serve-http` with `remote_sessions: false` and no
`forwarding` block. Within that configuration the following are stable.

### HTTP API

| Covered | Defined in |
| --- | --- |
| Every route under `/v1` listed in the README's Routes table, with its methods | README "Routes"; `openapi.yaml` `paths` |
| `/healthz` and `/readyz`: methods, status codes, and the fields `status`, `drives`, `failed` | README "Health" |
| The meaning of each success status and the documented fields of each success body | README "Routes", "Semantics" |
| The error envelope `{"error":{"code","message"}}`, and the mapping from condition to HTTP status and `code` | README "Status codes and errors" |
| The order in which a request is checked (shutdown, path syntax, route, drive and token, body size, method, operation) | README "Status codes and errors"; `openapi.yaml` "Order of checks" |
| Documented request headers: `Authorization`, `Range`, `If-Range`, `If-Match`, `If-None-Match`, `Accept-Encoding`, `Content-Length`, `X-Request-Id` | README "Authentication", "Request ids", "Reading", "Entity tags" |
| Documented query parameters: `limit` and `cursor` on the `entries` route, the page object `{"entries","next_cursor"}`, and that a request without `limit` gets the complete array. The cursor's content is opaque and not covered | README "Listings" |
| Documented response headers: `ETag`, `Content-Range`, `Accept-Ranges`, `Allow`, `WWW-Authenticate: Bearer`, `Retry-After`, `Content-Encoding`, `Vary`, `Connection: close` where documented, `Cache-Control: no-store` on probes, `X-Request-Id` on every response (the rule for echoing a client's value; not the format of a generated id beyond its character set) | Same |
| Path rules: one percent-decode per segment, the rejected forms, the reserved prefix `.mount-rs-upload.` | README "Paths" |
| Semantics: atomic `PUT`, durable before `200` (with the two documented operator exceptions), conditional requests, non-recursive `DELETE`, POSIX `rename`, the idempotency table | README "Semantics", "Retries and idempotency" |
| The existence and meaning of each limit, and the response when it is exceeded | README "Limits" |

### Operator surface

| Covered | Defined in |
| --- | --- |
| The command line: `mount-rs serve-http --config <path>`, `mount-rs validate-config --config <path>` | Runbook "Start" |
| Config keys under `http` and `http.drives[]`: name, type, meaning and default, except those marked experimental | Runbook "Configuration reference" |
| Exit codes 0, 1 and 2 and what each class means | Runbook "Exit codes" |
| Signals: SIGINT and SIGTERM start a graceful shutdown | Runbook "Shutdown" |
| The two stdout lifecycle lines: `http listening at <url> (drives: ...)` and `http stopped` | Runbook "Readiness line", "Shutdown" |
| Token provisioning: one environment variable per drive, read once at startup | Runbook "Tokens" |

## What may change without a version bump

Clients and operators must tolerate everything in this section. None of it
is a breaking change.

### In responses

| May change | What a client must do |
| --- | --- |
| The `message` text of any error | Branch on HTTP status and `code` only. This is already stated in the README. |
| New properties in a JSON response object (success bodies, `Stats`, `StatsFs`, listing entries, the health bodies, the error object) | Ignore properties you do not know. `openapi.yaml` does not close its schemas with `additionalProperties: false`. |
| New response headers | Ignore headers you do not know. |
| The order of properties in a JSON object, and JSON whitespace | Parse JSON; do not compare bytes. |
| The order of entries in a listing | Already documented as not guaranteed. |
| The format of an `ETag` value | Treat tags as opaque, as documented. The README promises that a split-store tag survives a restart; it makes no promise across releases. If a release changes how tags are derived, a tag read before the upgrade no longer matches and a conditional write fails with `412`, which a client already has to handle by reading the file again. |
| A new `file_type` value for a kind of file that cannot be created through v1 today | Treat an unknown `file_type` as "other". |
| New `code` values, but only on a status that already allows "others" (500), or on a new route or new optional request feature | Treat an unknown `code` by its HTTP status class. |
| Errors generated by the reverse proxy | These never had the API's error body. |

### In requests the server accepts

| May change | Consequence |
| --- | --- |
| New routes under `/v1` | A path that is `404` today may be a route in a later release. Do not rely on a `404` for an undocumented path. |
| New methods on an existing route | A `405` today may succeed later. The `Allow` header is authoritative for the release that sent it. |
| New optional request headers and new optional JSON body properties | Unknown JSON body properties are ignored today (documented for `mkdir`; the `rename` and `truncate` parsers behave the same way). A client must not send properties it does not mean. |
| Looser validation: a request that is rejected today with `400`, `405`, `413`, `416` or `501` may be accepted later | Do not use a rejection as a feature probe. |
| Items on the README's "Not supported at launch" list becoming supported | Additive. |

### In limits and performance

| May change | Note |
| --- | --- |
| The **default** value of a limit | Defaults are operator configuration, not contract: the README lists them as "Defaults; the operator can change them". A consumer must read a limit from its operator, not from the documents. Proposed rule: a changed default is listed in the release notes. For what a changed default means to an existing config file, see [What is a breaking change](#what-is-a-breaking-change). |
| Latency, throughput, memory use | No performance figure is part of the contract. See [`serve-http-slo.md`](serve-http-slo.md). |
| Internal buffering thresholds (for example the 1 KiB gzip threshold for listings, the 64 MiB / 5 s discard of a refused body) | Observable, documented for information, not contract. |

### On the operator surface

| May change | Consequence |
| --- | --- |
| New optional config keys with a default that preserves existing behaviour | An existing config file keeps working on a newer binary. The reverse is not true; see [Config file compatibility](#config-file-compatibility). |
| New accepted values for an existing key | Same. |
| The text of error messages on stderr, beyond the `mount-rs: ` prefix | Alert on exit codes, not on message text. The runbook quotes several messages (`EAGAIN ... acquire writer`, `HTTP server close timed out`); treat those as aids to diagnosis. |
| New JSON event lines on stderr, and new fields in `mount_cache_stats`, `http_accept_failed` and `http_drive_storage` lines | Parse by `event` name; ignore unknown lines and fields. |
| Signals recorded by the optional `observability` feature | Not part of v1. |
| On-disk and in-database storage formats | Governed by [`serve-http-upgrade.md`](serve-http-upgrade.md), not by the API version. |

## What is a breaking change

Any of the following, for a request that is valid under the v1 documents:

- Removing or renaming a route, a method, a documented request header, a
  documented response header, or a documented JSON property.
- Changing the type or the meaning of a documented JSON property.
- Changing the HTTP status or the `code` returned for a documented condition,
  or the order of checks in a way that changes which error a request gets.
- Rejecting a request that v1 accepts, or making a previously optional request
  element required.
- Weakening a documented guarantee: `PUT` atomicity, durable before `200`,
  conditional-write exclusivity, rename atomicity, the idempotency table.
- Changing the authentication scheme, or making `/healthz` or `/readyz`
  require authentication.
- Removing or renaming a config key, changing its type, changing a default in
  a way that changes behaviour for an existing config file, or changing the
  meaning of an exit code.
- Changing the config file `version`.

Two things are deliberately **not** breaking:

- **A fix that makes the server match the contract documents.** If the server
  disagrees with the README or `openapi.yaml`, the server is fixed. If
  consumers are known to depend on the wrong behaviour, the fix is announced
  as a deprecation would be.
- **A security fix.** A change needed to close a vulnerability may be made
  without a deprecation window. It is announced in the release notes of the
  release that carries it.

## How a breaking change is introduced (proposed)

1. **New prefix.** The changed API is served under `/v2`. Routes are not
   changed in place under `/v1`.
2. **Side by side.** The same binary serves `/v1` and `/v2` against the same
   drives for the whole deprecation window, so a consumer migrates without
   an operator action. A drive's token is valid for both.
3. **Contract documents.** `/v2` gets its own OpenAPI document. The v1
   document stays in the repository, unchanged except for corrections, until
   `/v1` is removed.
4. **Window.** `/v1` keeps being served for **at least 12 months after the
   first tagged release that serves the replacing `/v2` route, and for at
   least two tagged releases after it, whichever is later.**
5. **Removal.** After the window, a release may stop serving `/v1`. The
   release notes of that release say so in their first line. Requests to a
   removed prefix get `404` with the standard error body.

The 12-month figure is a proposal. The reasoning is in
[Why 12 months](#why-12-months).

### Config file changes (proposed)

- A breaking change to the config format raises the top-level `version` to
  `2`. A binary that understands `version: 2` keeps accepting `version: 1`
  files for the same window as above.
- A single key is deprecated without a version change by keeping it accepted,
  with its old meaning, for **at least two tagged releases and six months**.
  While deprecated, `validate-config` and `serve-http` print one warning line
  to stderr that names the key and its replacement. (Not implemented: the
  parser has no deprecated-key warning today, because no key has been
  deprecated.)

## How deprecations are announced (proposed)

| Channel | Content | Implemented? |
| --- | --- | --- |
| The GitHub release notes of the release that starts the window | What is deprecated, its replacement, the earliest removal date | Release notes exist per CLI preview tag. There is no `CHANGELOG.md` in the repository. |
| A "Deprecations" table in this document | One row per deprecated item: item, replacement, deprecated in, earliest removal | The table is below and is empty. |
| `openapi.yaml` | `deprecated: true` on the operation, parameter or property | Standard OpenAPI; nothing is marked today. |
| Response headers `Deprecation` (RFC 9745) and `Sunset` (RFC 8594) on responses from a deprecated route | Lets a consumer detect use of a deprecated route in its own telemetry | **Not implemented.** The server sends neither header. |
| A stderr warning at startup for a deprecated config key | Names the key and replacement | **Not implemented.** |

### Deprecations

| Item | Replacement | Deprecated in | Earliest removal |
| --- | --- | --- | --- |
| None | | | |

## Config file compatibility

The parser rejects unknown keys at every level of the config, including
inside `http`, `http.drives[]`, `http.forwarding`, `token` and each driver and
provider object. An unknown key is a configuration error: the process prints
the offending path and exits with code 2 before opening any drive.

Consequences:

| Direction | Result |
| --- | --- |
| Older config, newer binary | Works. New keys are optional and default to existing behaviour. |
| Newer config, older binary (a rollback) | **Fails at startup with exit code 2** if the config uses any key, or any value, that the older binary does not know. The service does not start. |
| Typo in a key | Fails at startup. A misspelt limit is never silently ignored. |

Rules for operators:

- Version the config file together with the binary. A rollback rolls back
  both.
- Before a rollback, run the **older** binary's
  `mount-rs validate-config --config <path>` against the file you intend to
  start it with. It opens no storage and reads no token.
- Do not adopt a new optional key in the same change as the binary upgrade.
  Upgrade first, confirm, then add the key. A rollback of the binary alone
  then still starts.

The procedure is in [`serve-http-upgrade.md`](serve-http-upgrade.md).

## Not under this policy

| Surface | Status |
| --- | --- |
| `http2_prior_knowledge: true` | Accepted by the parser. The runbook says to leave it `false` at launch. Behaviour with it enabled is not part of v1. |
| The `mount-rs-http` crate's Rust API (`HttpServer`, `HttpServerOptions`, `DriveRegistry`, `DriveConfig`, `HttpRequestHandler`) | Crate version `0.1.0`. No stability promise is made here. |
| The `observability` and `observability-otlp` features and the signals they record | Optional, off by default, no stability promise. |
| The checked-in example configs under `apps/mount-rs-cli/examples/` | Demos. They change with the code. |
| Driver and provider objects inside `http.drives[].driver` | Shared with the native mount configs. Each provider has its own maturity label; see [`maturity.md`](maturity.md). Options documented as experimental in the CLI README (for example `concurrent_writes`, the owner serving window) are outside this policy. |
| Storage formats | See [`serve-http-upgrade.md`](serve-http-upgrade.md). |

## Cluster features

**Status: experimental. Not covered by this policy. Not part of v1.**

This is the one place that status is stated in this document. When the
cluster features are accepted, change the line above and move the rows below
into the tables in [What v1 covers](#what-v1-covers).

Until then, everything in this section may change in any release, including
in ways that would be breaking under the rules above, without a deprecation
window.

| Surface | Enabled by |
| --- | --- |
| Session and handle routes under `/v1/drives/{id}/sessions`, the `x-mount-session`, `x-mount-handle`, `x-mount-position` and `x-mount-write-file` headers, and the session token format | `http.remote_sessions: true` |
| The stricter drive-token format required with sessions (32-256 characters from a fixed set) | `http.remote_sessions: true` |
| Forwarding of `/v1` requests between servers; the status codes and error bodies an ingress produces for routing and peer failures | `http.forwarding` |
| The config keys `http.remote_sessions` and every key under `http.forwarding` (`peers`, `certificate`, `private_key`, `ca`, `redis_url`, `namespace`, `bind`, `owner`, `refresh_active_routes`, `route_refresh_idle_ttls`, `peer_connections`) | |
| The membership file format (`node`, `boot`, `address`, `tls_server_name`, `spki_sha256`) | `http.forwarding.peers` |
| The peer protocol: HTTP/3 between servers, the `x-mount-owner-route` and `x-mount-hop` headers, the peer certificate profile | `http.forwarding` |
| The Redis route-registry key layout and record format | `http.forwarding.redis_url`, `namespace` |
| Running servers of different releases in one cluster | Not tested. See [`serve-http-upgrade.md`](serve-http-upgrade.md). |

The intent is that enabling a cluster feature does not change the behaviour
of the v1 file routes. That is not verified: with `remote_sessions` or
`forwarding` set, requests are dispatched through a different handler than
the one the v1 contract tests exercise, and no test runs the v1 contract
suite against a cluster-enabled server. It is therefore not a promise.

## Why 12 months

- Consumers are other services that integrate once, with a static token, and
  are then left alone. Their owners need a window long enough to cover a
  planning cycle; a window counted only in releases could be a few weeks.
- The project has no established release cadence: there are two tagged CLI
  preview releases. A time-based floor does not depend on cadence. The
  two-release floor guards against the opposite case, a year with no release.
- Serving `/v1` beside `/v2` costs maintenance, not runtime: both prefixes
  route to the same drives through the same driver interface.
- A shorter window should be chosen only with knowledge of who the consumers
  are and how quickly they can ship a change. This document does not have
  that knowledge, so it proposes the conservative figure.
