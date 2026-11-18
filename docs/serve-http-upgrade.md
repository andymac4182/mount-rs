# serve-http upgrade and rollback

How to replace the `mount-rs` binary under a running `serve-http` service,
how to go back, and when going back is not safe.

Scope: the v1 launch configuration, which is one process per drive set with
`remote_sessions: false` and no `forwarding` block. Clusters are covered by
[Clusters](#clusters): the rule for servers of different releases in one
cluster, what a test proves about it, and the rolling upgrade.

Related documents:

- Operator runbook (shutdown, lease, backup): [`serve-http-runbook.md`](serve-http-runbook.md)
- Compatibility policy (API and config): [`serve-http-compatibility.md`](serve-http-compatibility.md)
- Security: [`serve-http-security.md`](serve-http-security.md)
- Service levels and backup: [`serve-http-slo.md`](serve-http-slo.md)

## What is and is not supported

| | Status |
| --- | --- |
| Stop the old binary, start the new one on the same storage (a restart upgrade) | Supported. This is the only upgrade procedure. |
| Zero-downtime upgrade of a drive | **Not supported.** A split-store drive has one exclusive writer lease, so the new process cannot open the drive until the old one has released it. The API is unavailable between the old process closing its listener and the new one printing its readiness line. |
| Running the old and new binary against the same drive at the same time (blue/green, canary on the same storage) | **Not supported.** The second process fails at startup with `EAGAIN ... acquire writer`. For `sqlite`-kind and `host` drives nothing stops a second process, and running two is forbidden by the runbook. |
| Configuration reload without a restart | **Not implemented.** There is no reload signal. Every config and token change is a restart. |
| Rollback to the previous binary | Supported only under the conditions in [When a rollback is unsafe](#when-a-rollback-is-unsafe). **Not tested**: no test in the repository starts one release against data written by another. |
| Skipping releases (N to N+2) | **Not tested.** Nothing in the code forbids it: schema steps are idempotent and applied at every open. Rehearse it. |
| A cluster with servers of two releases, and a rolling upgrade of a cluster | Defined by a protocol version, and **tested for one pair of builds**: the current commit with `main` at `031050cc`. Not tested between published releases. See [Clusters](#clusters). |

## Why an upgrade is a restart

These facts come from the runbook's "Shutdown" and "Lease takeover wait"
sections; they decide the procedure.

- SIGTERM starts a graceful shutdown: the listener stops accepting,
  in-flight requests get up to `drain_timeout_ms` (default 35 s), then every
  drive is shut down, which flushes storage and **releases the writer
  lease**. The process prints `http stopped` and exits 0.
- If the drain deadline passes, requests are aborted, the drives are **still**
  shut down and the lease is still released, and the process exits 1 with
  `HTTP server close timed out`. A new process can start at once.
- After SIGKILL, a crash or power loss, the lease is not released. A new
  process fails at startup with `EAGAIN: resource temporarily unavailable,
  acquire writer` and exit code 1 until the lease expires: up to 240 s for a
  `splitstore` drive with a structured `storage` object, 30 s with the legacy
  `database` + `blocks` pair, or the drive's `lease_ttl_ms`. It does not
  wait; the service manager must retry.
- Drive shutdown is bounded by `drive_shutdown_timeout_ms` (30 s by
  default). Against unreachable storage the process gives up at that bound
  and exits with code 1 and `drive shutdown timed out after N ms: ...`. The
  lease of a drive named there was not released, as after SIGKILL. The
  service manager's stop timeout still ends in SIGKILL as the backstop.

So the downtime of a clean upgrade is: drain time of the requests in flight,
plus storage flush, plus the new process's startup (open every drive, bind).
The downtime of an upgrade that ends in SIGKILL adds up to one lease
lifetime.

## How stored data changes between releases

Verified in the provider code at the revision this document was written
against. Line numbers are in [Appendix: code references](#appendix-code-references).

### The rule in the code

- Every provider creates and extends its schema **when a drive is opened**.
  There is no separate migration command for these steps and no way to run
  them ahead of time.
- Every schema step found is **additive**: `CREATE TABLE IF NOT EXISTS`,
  `CREATE TRIGGER IF NOT EXISTS`, `ADD COLUMN` (guarded by `IF NOT EXISTS` or
  by a check that the column is missing), `ADD INDEX IF NOT EXISTS`. Existing
  rows are kept; new columns start as `NULL`.
- **There is no down-migration.** No provider contains code that removes a
  column, table or index it added, and no command does so. Once a newer
  binary has opened a drive, the schema stays in the newer shape.

### Per provider

| Storage | Schema steps at open | Version marker | What a binary does when the stored version is newer than it knows |
| --- | --- | --- | --- |
| SQLite split-store metadata | Additive, in one transaction | Yes: table `mount_rs_schema_versions`, row `mount-rs-versioning` (this build accepts 0 to 2) | **Refuses to open** with `ENOTSUP` (`unsupported mount-rs versioning schema version`) and leaves the database unchanged. A unit test covers this. The process exits 1 at startup. |
| SQLite split-store blocks | Additive | **No** | Not determined by a marker. See below. |
| `sqlite`-kind drive (single file) | Additive (one column) | **No** | Not determined by a marker. See below. |
| TiDB metadata and blocks | Additive; the `ALTER TABLE` statements run on **every** metadata open | **No** | Not determined by a marker. See below. |
| PGlite | Additive; has a `mount_rs_schema_versions` table with a newer-version check | Yes | Read only far enough to confirm the steps are additive and the check exists. Behaviour not examined further. |
| FoundationDB, SlateDB, object-store block providers | **Not examined** for this document | Unknown | Unknown. Use the provider's own runbook in `docs/` and rehearse. |

In addition, the namespace document that every split-store metadata provider
stores carries a `format_version` (currently 1). The core refuses a document
with a higher value with `ENOTSUP` (`namespace format version is newer than
this core`).

Two details of the SQLite metadata marker matter for rollback:

- The marker is raised from 1 to 2 only when the first record of a newer kind
  (a namespace-publication version record) is committed, not when a newer
  binary merely opens the database. The code comment states the purpose: so
  that older readers can reopen a store that has not used the newer feature.
- While the metadata database holds unfolded namespace change rows, a trigger
  raises the marker by 3, past what a build from before those rows accepts,
  so that such a build refuses the database rather than reading a stale
  namespace. The rows are folded back, and the marker lowered, at the start
  of the next job on the connection. **Whether a cleanly stopped process
  always leaves the database folded was not determined for this document.**
  If it does not, a rollback to a build from before the change rows is
  refused with `ENOTSUP` until a current build has opened the database again.

### What "no marker" means

Where there is no version marker (TiDB, SQLite blocks, `sqlite`-kind), an
older binary that meets a schema extended by a newer one is **not refused by
any check**. The current code requires a minimum set of columns and names the
columns it reads, so extra columns do not by themselves stop it from opening.
Whether it then behaves correctly depends on whether the newer binary wrote
data whose meaning lives in a column or record the older binary ignores.
That is a property of the specific pair of releases. **It is untested and
must be rehearsed for each upgrade** (see [Rehearsal](#rehearsal)).

One case is already documented as unsafe: once a volume has been switched to
`concurrent_writes`, older writers do not recognise the marker. The CLI
README says to stop and upgrade older writers before activation. `serve-http`
at launch uses exclusive ownership, so this applies only if the option is
enabled.

### Local caches

A split-store drive with a `disk_cache` keeps cache files on local disk. They
are a cache, not data. `production-readiness.md` records that the cache file
naming changed once, that a newer binary adopts the old layout at startup and
that an older binary ignores the new names. If in doubt after a rollback,
stop the service and delete the cache directory; it is refilled from the
block store.

## When a rollback is unsafe

A rollback is the same restart as an upgrade, with the previous binary. Treat
it as unsafe, and restore from the pre-upgrade backup instead, when any of
these is true.

| Condition | Why | What happens if you roll back anyway |
| --- | --- | --- |
| The newer binary's release notes name a storage-format change, and the service has accepted writes since the upgrade | The older binary may not understand what was written | With a version marker: refused at startup (`ENOTSUP`, exit 1), no damage. Without one: undefined; not tested. |
| An option documented as one-way was enabled after the upgrade (today: `concurrent_writes`) | Documented in the CLI README as needing an offline migration | Older writers do not recognise the mode. |
| The config uses a key or value the older binary does not know | The parser rejects unknown keys | Refused at startup, exit 2. Not harmful, but the service is down until the config is rolled back too. |
| You have not rehearsed this pair of releases | Nothing in CI covers it | Unknown. |

A rollback is reasonable without a restore when all of these hold: the release
notes name no storage-format change, the rehearsal passed for this pair of
releases, and the config file is the one the older binary validated.

Restoring the pre-upgrade backup loses every write acknowledged after the
backup. Tell consumers the restore point, as the runbook's restore procedure
says.

## Pre-upgrade checklist

- [ ] Read the release notes of every release between the running version
      and the target (`mount-rs --version` prints the running one). Look for
      storage-format changes, changed defaults and deprecated config keys.
- [ ] Rehearsal passed for this pair of releases (see [Rehearsal](#rehearsal)).
- [ ] The **new** binary accepts the current config:
      `new/mount-rs validate-config --config /etc/mount-rs/http.json` exits 0.
      It opens no storage and reads no tokens.
- [ ] The config file contains no key that only the new binary knows. Adopt
      new keys in a later change, after the upgrade is confirmed.
- [ ] The old binary and the current config file are kept where a rollback
      can reach them. For containers, the previous image tag is pinned by
      digest.
- [ ] A cold backup exists that was taken after the last write you cannot
      afford to lose, or you accept losing writes since the last one. The
      procedure is in the runbook under
      [Backup and restore](serve-http-runbook.md#backup-and-restore-sqlite-backed-drives);
      step 3 below is the natural point to take it.
- [ ] `/readyz` returns 200 now. Do not upgrade a service whose drive has
      failed closed; restart it on the current binary first and find the
      cause.
- [ ] For TiDB-backed drives: the database user may run `CREATE TABLE` and
      `ALTER TABLE` on the mount-rs tables. The schema steps run at open, so
      a user without DDL rights fails at startup. (They already run on every
      open of the current binary; this matters when rights were tightened
      after first deployment.)
- [ ] The service manager's stop timeout is above `drain_timeout_ms` + 5 s +
      `drive_shutdown_timeout_ms`, with SIGKILL as the fallback
      (`TimeoutStopSec=75`, or `docker stop -t 75` /
      `terminationGracePeriodSeconds: 75`, with the defaults).
- [ ] Consumers know there will be an outage of the length estimated above,
      and that requests during it fail with `502`/`503` from the proxy.
      The API guide already tells them to retry `503` and connection
      failures with back-off.
- [ ] Record a known file for the read check: its path, size and `ETag`
      (`curl -sI -H "Authorization: Bearer $TOKEN" "$BASE/v1/drives/<id>/fs/<path>"`).

## Upgrade procedure (single node)

`$BASE` is the server's own listener (for example `http://127.0.0.1:8080`)
unless a step says "through the proxy".

1. **Take the instance out of the load balancer**, or make the proxy answer
   `503` itself. The runbook's shutdown section asks for this so that the
   drain affects no client.
2. **Stop with SIGTERM** through the service manager
   (`systemctl stop mount-rs`, `docker stop -t 75 mount-rs`). Wait for the
   process to exit.
3. **Check how it stopped.**

   | Exit | stdout / stderr | Meaning | Continue? |
   | --- | --- | --- | --- |
   | 0 | `http stopped` | Clean. Storage flushed, lease released. | Yes. |
   | 1 | `HTTP server close timed out` | Drain deadline passed; requests were aborted. Drives were still shut down and the lease released. | Yes. A mutating request that was aborted has an unknown outcome for its client. |
   | 1 | `drive shutdown timed out after N ms: drive 'X' did not finish ...` | The drives did not shut down within `drive_shutdown_timeout_ms`. The lease of each drive the message names is still held. Writes acknowledged as durable are intact. | Investigate storage first. Then yes, but expect the lease wait in step 6, and do not take a cold backup now. |
   | 1 | Anything else, or a drive-shutdown error | Drive shutdown failed. | Investigate storage before starting anything. |
   | Killed by SIGKILL | | Shutdown did not finish. The lease is still held. | Yes, but expect the lease wait in step 6. Do not take a cold backup now: the copy would carry a live lease. |

4. **Take the cold backup** (SQLite-backed drives) if the checklist called
   for one. Copy both databases together with any `-wal` and `-shm` files, as
   the runbook describes. Skip only if the stop was not clean.
5. **Install the new binary** (or change the image tag). Leave the config
   file and the token environment unchanged.
6. **Start the service.** On the first start the new binary applies its
   schema steps to each drive as it opens it. There is no separate migration
   step and no progress output.
   - If it exits 1 with `EAGAIN ... acquire writer`, the old lease has not
     expired (step 3, last row). Let the service manager retry with back-off;
     with the 240 s default expect about 25 failed starts at `RestartSec=10`.
     Do not edit or delete the lease.
   - If it exits 2, the new binary rejects the config. This should have been
     caught by the checklist. Start the old binary again; nothing was opened.
   - If it exits 1 with a schema, hash or integrity message, **stop**. Do not
     retry. Go to [Rollback](#rollback-procedure).
7. **Verify** with the checklist below, against the listener directly.
8. **Put the instance back in the load balancer** and repeat the
   read-after-write check through the proxy.
9. Keep the old binary, the pre-upgrade backup and the unchanged config until
   the upgrade has been in service for as long as you would want to be able
   to roll it back.

## Verification checklist

Run after an upgrade and after a rollback. `$TOKEN` is the drive's token;
`<id>` its drive id.

- [ ] The readiness line `http listening at ... (drives: ...)` was printed
      and lists every drive.
- [ ] `mount-rs --version` on the host, or the image tag, is the intended
      one.
- [ ] `curl -s -o /dev/null -w '%{http_code}\n' "$BASE/healthz"` prints `200`.
- [ ] `curl -s "$BASE/readyz"` returns `200` with
      `{"status":"ready","drives":N}` and `N` equals the number of configured
      drives. Remember its limits: it does no I/O and only split-store drives
      report health, so a `200` here is necessary, not sufficient.
- [ ] **Read of existing data.** The known file recorded before the upgrade
      has the same size, and a full `GET` returns the same bytes (compare a
      checksum taken before the upgrade). Do not require the same `ETag`
      across releases; the compatibility policy does not promise it.
- [ ] **Listing of existing data.** `GET /v1/drives/<id>/entries/<known dir>`
      returns the entries it had before.
- [ ] **Read after write.** Every command must behave as annotated:

      ```sh
      P="$BASE/v1/drives/<id>/fs/.upgrade-check-$(date +%s)"
      A="Authorization: Bearer $TOKEN"
      curl -sf -H "$A" -X PUT --data-binary 'upgrade-check' "$P"        # 200 {"path":...,"bytes":13}
      test "$(curl -sf -H "$A" "$P")" = 'upgrade-check'                 # same bytes back
      curl -sf -H "$A" -H 'Range: bytes=0-6' "$P"                       # 206, "upgrade"
      curl -s -o /dev/null -w '%{http_code}\n' -H "$A" -X DELETE "$P"   # 204
      curl -s -o /dev/null -w '%{http_code}\n' -H "$A" "$P"             # 404
      ```

- [ ] **Authentication still refuses.** The same `GET` with a wrong token
      returns `401`.
- [ ] Through the proxy: the read-after-write check again, and the proxy's
      access log shows no `5xx` for it.
- [ ] Over the next lease-renewal period (renewals run every 30 s by
      default), `/readyz` stays 200. A drive that cannot renew its lease on
      the new binary fails closed only then, not at startup.
- [ ] Proxy `5xx` and `401` rates return to their pre-upgrade baseline once
      consumers have reconnected.

## Rollback procedure

Decide first whether the rollback is safe; see
[When a rollback is unsafe](#when-a-rollback-is-unsafe).

### A. Binary rollback (storage left as it is)

1. Take the instance out of the load balancer.
2. Stop with SIGTERM and check the exit as in upgrade step 3.
3. Restore the **config file that the old binary validated**. Run
   `old/mount-rs validate-config --config <path>`; it must exit 0. An
   unknown key means the file was changed after the upgrade.
4. Install the old binary and start the service.
   - Exit 1 with `ENOTSUP` and a schema message (`unsupported mount-rs
     versioning schema version`, `namespace format version is newer than
     this core`): the old binary has refused the newer storage. Nothing was
     changed. Either go forward again (reinstall the new binary) or continue
     with procedure B.
   - Exit 1 with `EAGAIN ... acquire writer`: lease wait, as in an upgrade.
5. Run the [verification checklist](#verification-checklist). Pay particular
   attention to files written **while the new binary was running**: read at
   least one of them back in full and list its directory. This is the case no
   test covers.

### B. Rollback with restore (storage returned to the pre-upgrade backup)

1. Stop the service.
2. Restore the pre-upgrade backup as the runbook's restore procedure
   describes: both SQLite databases from the same backup, stray `-wal` and
   `-shm` files removed. For other providers, restore with that system's
   tools.
3. Install the old binary and the old config, and start.
4. Run the verification checklist.
5. Tell consumers the restore point. Every write acknowledged after the
   backup is gone, including writes whose `PUT` returned `200`.

## Rehearsal

Because cross-release behaviour is not tested in the repository, rehearse
each upgrade on a copy before production. The repository's end-to-end probe
exercises one binary, including a restart on the same binary; it does not
cross releases:

```sh
python3 scripts/serve-http-e2e.py ./mount-rs "$(mktemp -d)"
```

A rehearsal that covers what the probe does not:

1. Restore a recent cold backup of the production drive into a scratch
   location, or create a scratch drive of the same provider and driver
   options.
2. Start the **old** binary on it. Write a set of files; record their
   checksums.
3. Stop it with SIGTERM. Start the **new** binary on the same storage. Check
   the old files, write a second set, record their checksums.
4. Stop it with SIGTERM. Start the **old** binary again. This is the rollback.
   Record exactly what happens: a clean start, or a refusal and its message.
5. If it starts: read both sets back and compare checksums, list the
   directories, write and delete a file, and leave it running for a few
   lease-renewal periods with `/readyz` at 200.
6. Repeat step 4 after stopping the new binary with SIGKILL instead of
   SIGTERM, and wait out the lease. This covers a rollback after a crash,
   where the database may not be in its at-rest form.

Keep the result with the release notes. It is the only evidence that a
rollback of that pair of releases works.

## Clusters

### The peer protocol version

Servers of a forwarding cluster read two things that another server wrote:
a forwarded request and its response, and the drive's route record in
Redis. Together these are the peer protocol, and it has a version: a
single number, currently **1**. Nothing else passes between servers. A
failover hand-off goes through the storage lease and a Redis set of
candidate names, and is covered by the same test.

- An owner publishes the range of versions it accepts in its route record
  (`protocols`, with `min` and `max`). A record without the field means
  version 1 only, which is what a version 1 owner writes, so the record is
  unchanged from earlier builds.
- A forwarding server picks the highest version that it and the owner both
  speak and names it on every forwarded request, in the
  `x-mount-peer-protocol` header. A request without the header is
  version 1.
- A build speaks the versions from `MIN_PEER_PROTOCOL_VERSION` to
  `PEER_PROTOCOL_VERSION` (`crates/mount-rs-cluster/src/peer_protocol.rs`).
  Both are 1 today.

**A server never runs or sends a request in a version it does not speak:**

| Case | What the server does | What the client sees | What is logged |
| --- | --- | --- | --- |
| A forwarding server reads a route record whose owner accepts no version it speaks | Sends nothing to the owner | `503`, `x-mount-dispatch: not-dispatched`, `Retry-After: 1`, the standard cluster error body | On the forwarding server: `{"event":"peer_protocol_refused","role":"forwarder","drive":...,"peer":...,"accepts":"2-3","speaks":"1-1",...}` |
| An owner receives a forwarded request in a version it does not speak, or with a version header it cannot read | Answers the peer `426` with `x-mount-dispatch: not-dispatched` before reading anything else. The request is not run. | The same `503` from the forwarding server | On the owner: `"role":"owner"` with the peer's name and the version `offered`. On the forwarding server: `"role":"forwarder"` |
| A build from before the marker (see below) reads a record that carries `protocols` | Rejects the record as invalid and sends nothing | The same `503` | Nothing: those builds have no such event |

The `503` lasts until the two servers run compatible releases or the drive
moves to an owner the forwarding server can speak to. `peer_protocol_refused`
lines are limited to one every 5 seconds per process; `suppressed` counts
the refusals since the last line. The owner's own clients, and requests that
reach an owner directly, are not affected: the version applies to the
server-to-server hop only.

Tested in `crates/mount-rs-cluster/tests/peer_protocol.rs` (a real owner
and forwarder over HTTP/3: versions absent and `1` are served; `2`, `0`,
unreadable and repeated headers are refused and the write does not happen;
a record accepting only later versions is not dialled; a record accepting
1 and a later version is sent version 1), in unit tests of the record and
header forms, and with real processes by the "a later owner's record"
checks of `scripts/serve-http-cluster-e2e.py`.

### What is tested between builds

CI job `cluster-mixed-release` builds two binaries, the commit under test
and a pinned **baseline**, and runs `scripts/serve-http-cluster-e2e.py`
with the two in one cluster, twice: baseline as node a with the current
build as nodes b and c, then the reverse.

The job chooses the baseline itself (`scripts/mixed-release-baseline.sh`):
the newest `v*-cli-preview*` release, by version, that descends from a
fixed floor commit and is not the commit under test. Until such a release
exists the baseline is the floor: `main` at
`031050cc78032e4827b06264fd771344d1f6967a` (the merge of #148). **That is
the case today:** the published releases (up to `v0.2.0-cli-preview`) were
cut before the floor, so every run so far has used the floor commit. It
predates the version marker, so it sends no version header and writes no
`protocols` field: this is the "no version means version 1" case. The job
prints the baseline it used on a `MIXED_RELEASE_BASELINE` line and in its
summary. From the first release cut after the floor, every later commit is
tested with the newest release, without an edit to the workflow.

Each run covers, between a server of one build and a server of the other:

- file requests forwarded in both directions (create, read, range, `HEAD`,
  `stat`, delete, wrong token, conditional requests, 200 alternating
  writes, 64 parallel writes and 256 parallel reads, an upload at the
  request cap and a streamed one above it, the `413` over the upload
  limit);
- a session created through the server that does not own the drive, with
  positional reads and writes on one handle through both servers (each
  run does this in one direction; the two runs cover both);
- an owner restart; an owner serving more than 16 peer connections; a
  membership reload that adds and removes an ingress;
- failover of one drive on shared SQLite storage between a candidate of
  each build: after `kill -9` and after a clean stop, with writers
  running, no acknowledged write lost and one lease holder at a time. The
  drive is opened by one build, then the other, then the first again;
- the baseline answering `503` for a record with a `protocols` field.

What this does **not** prove:

- Anything about a **published release**, yet. The baseline has so far
  been a commit on `main`, and no pair of releases has been run together.
  When the baseline is a release, the pair tested is that release with a
  later commit of `main`; two releases have been run together only once
  the later one's own commit has passed the job.
- Any pair other than the one in the job, including builds older than the
  baseline.
- A version change. Only version 1 exists, so the test proves that the
  marker is compatible with builds that lack it, and that refusal works;
  it has not exercised two builds that speak different versions and still
  interoperate.
- Storage other than SQLite metadata with SQLite blocks. The failover
  tests on TiDB (`tidb-rustfs`) run one build.
- Storage format compatibility in general. In a mixed cluster with
  failover a drive can be opened by the new release and then by the old
  one again, which is the rollback case of
  [When a rollback is unsafe](#when-a-rollback-is-unsafe). The mixed test
  does this on SQLite for this pair only.

### Policy

- A cluster may run servers of **two adjacent releases** for the length of
  an upgrade. This is the intent from here on, and it is enforced by
  `cluster-mixed-release`, whose baseline moves to the newest release by
  itself when one is cut. Until a release pair has passed that job,
  treat a rolling upgrade between published releases as untested and
  rehearse it.
- A release that changes what servers send each other raises the protocol
  version and keeps speaking the previous one for one release, so the
  adjacent pair still shares a version.
- Releases that are not adjacent, and any pair that does not share a
  version, are upgraded by a full stop (below). A server refuses such a
  peer as in the table above; it does not misread it.

### Rolling upgrade

For two builds that the mixed-release job has passed for.

1. Check that neither release's notes forbid opening the other's storage
   (the storage rules of this document apply while the cluster is mixed,
   in both directions).
2. Run the new binary's `validate-config` on every server's config.
3. One server at a time:
   1. Stop it with SIGTERM. With `failover`, wait until each of its drives
      is `published` by another candidate (`drive_failover` lines; one to
      two seconds after a clean stop). Without `failover`, its drives
      answer `503` until step 3.3.
   2. Replace the binary.
   3. Start it and wait for `/readyz`.
   4. Send one request per drive through it and through another server.
      Look for `peer_protocol_refused` on stderr of every server: there
      must be none.
4. Do not adopt a new optional config key until every server runs the new
   release.

Sessions and open handles on a drive that moves are lost, as in any
failover. Requests in flight on a stopping owner are drained; forwarded
requests that arrive while it stops are answered `503` or `502` as
described in [Placement and failover](serve-http-runbook.md#placement-and-failover).
A rollback is the same procedure with the old binary.

### Full stop

For any pair of releases the rolling upgrade does not apply to:

1. Run the new binary's `validate-config` on every server's config.
2. Stop every server with SIGTERM. Each withdraws its routes, closes its
   sessions and releases its leases, also when its drain runs out of time,
   so nothing has to expire before the new release can start.
3. Start every server on the new release. Start order does not matter.
4. Check `/readyz` on each and one request per drive through the cluster.

The API is unavailable between steps 2 and 3, and remote mount clients lose
their sessions. A rollback is the same procedure with the old binary, under
the storage rules in the rest of this document.

### Rolling restart of one release

A rolling restart of failover candidates on one release (for a config
change or a host reboot) is supported and tested: a clean stop
hands each drive to another candidate in one to two seconds
(`scripts/serve-http-cluster-e2e.py`; see
[Placement and failover](serve-http-runbook.md#placement-and-failover)).
Sessions on the drive that moves are still lost. Without failover, a
restart of an owner makes its drives unavailable for its duration, and a
request whose cached route names the stopped owner re-reads the route and
answers `503` rather than waiting for a handshake timeout.

## Appendix: code references

Checked at the revision this document was added in.

| Claim | Where |
| --- | --- |
| Config accepts exactly `version: 1` | `apps/mount-rs-cli/src/config.rs:21`, `:416-422` |
| Unknown config keys are rejected | `apps/mount-rs-cli/src/config.rs:1781-1796`; `http` keys `:608-634` |
| Shutdown order: close listener, then shut drives down even after a failed drain | `apps/mount-rs-cli/src/runtime.rs:1085-1098` |
| Drain timeout then 5 s abort grace | `transports/mount-rs-http/src/server.rs:60`, `:775-787` |
| TiDB schema: `CREATE TABLE IF NOT EXISTS` | `providers/mount-rs-tidb/src/storage.rs:51-84` |
| TiDB additive `ALTER TABLE ... IF NOT EXISTS` at every metadata open | `providers/mount-rs-tidb/src/storage.rs:215-229` |
| TiDB has no schema-version table | No `schema_version` in `providers/mount-rs-tidb/src/` |
| SQLite metadata schema and change-row triggers | `providers/mount-rs-sqlite/src/storage.rs:741-780` |
| SQLite version marker constants | `providers/mount-rs-sqlite/src/storage.rs:937-942` |
| SQLite refuses a marker outside its range with `ENOTSUP` | `providers/mount-rs-sqlite/src/storage.rs:1225-1237`, `:1716-1720`; test `:8656-8691` |
| SQLite additive `ADD COLUMN`, guarded by a column check | `providers/mount-rs-sqlite/src/storage.rs:1249-1308` (metadata), `:1094-1114` (blocks) |
| SQLite marker raised to 2 on the first namespace-publication record | `providers/mount-rs-sqlite/src/storage.rs:4300-4310` |
| SQLite change rows folded at the start of a job | `providers/mount-rs-sqlite/src/storage.rs:606-617`, `:670-679`, `:901-927` |
| No `DROP` of schema objects in SQLite provider code outside tests | Tests start at `providers/mount-rs-sqlite/src/storage.rs:4996` |
| `sqlite`-kind drive: one additive column, no marker | `providers/mount-rs-sqlite/src/lib.rs:55-96` |
| PGlite additive steps and version check | `providers/mount-rs-pglite/src/storage.rs:51-65`, `:84-88`, `:585` |
| Namespace `format_version` check | `src/storage.rs:173`, `:386-395` |
| Lease default of 240 s for a structured `storage` object | `apps/mount-rs-cli/src/config.rs:1153`. The 30 s figure for the legacy pair is taken from the runbook and was not traced in the code. |
| Disk-cache layout note | `docs/production-readiness.md:218` |
| `concurrent_writes` and older writers | `apps/mount-rs-cli/README.md:118-121` |
