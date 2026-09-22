# W05 Cloudflare R2 progress ledger

Last updated: 2026-09-22 21:50 AEST (2026-09-22 11:50 UTC)

This is the working ledger for the W05 Cloudflare R2 workstream. Percentages
and time estimates are provisional. They separate implementation work from
provider, hosted-CI, and native-platform gates; a local pass does not close a
hosted or native gate.

## Overall position

Current shared-main observation: remote `origin/main` is
`849c76a2c642141579f4ad75cf504e7e49e818df` (`849c76a2`) at this capture.
The exact pushed W05 candidate boundary is the same SHA, held immutable on
branch `andymac4182/c/w05-production-candidate-20260922e`; its fresh
same-SHA hosted packet is running. The prior immutable W05 candidate boundary
`d1a81ad45158dc80647b723f175c01a98b9ed458` (`d1a81ad4`) was superseded by
the FUSE/NFS code successors now on shared main; its seven queued hosted
workflows were cancellation-requested and contribute no acceptance. The earlier exact pushed W05
implementation boundary
`515bdc00792a62403b0ee7b94e904434d067f43b` (`515bdc00`) was fully qualified
locally before the concurrent W01/AWS/W08/NFS mainline changes were rebased.
That packet passed the locked Rust workspace, strict Clippy, formatting, the
optimized macOS N-API build, the complete pinned-oracle Node SDK/CLI suite,
real PGlite lifecycle/provider/CLI checks, Rust SDK `6/3/0`, Node SDK `5/3/0`,
CLI `12/2`, upstream `1200/82`, and all 40 five-seed/eight-backend traces at
621 operations. The package helper makes Rust versions before 1.98 produce a
dyld-loadable Darwin artifact on macOS 27; the exact binary reported aligned
`LC_SYMTAB.stroff`, minimum macOS 11.0, SDK 26.0, and loaded through Node.
The PGlite provider harness also required a one-line standalone lock refresh
for the current `md-5` dependency and then passed under `--locked`. R2,
TiDB/RustFS, AWS, and privileged native mounts remain explicit provider or
platform gates. The prior immutable candidate CI run remains terminal-failed
with the classifications below, and no current R2 run was admitted because
the monthly cap remains closed. The current candidate's local packet is green,
but hosted CI, package publication/provenance, native platform acceptance,
live provider gates, support scope, and W20.6 remain open. Production remains
**NO-GO**.

Latest immutable release-candidate boundary (2026-09-22 20:31 AEST): the
candidate branch
`andymac4182/c/w05-production-candidate-20260922c` is pinned to exact
`7efded5424f8ad6c2335e7be1cde98c22b9315fe` (`7efded54`). It is deliberately
stable while shared `origin/main` continues to move, so hosted results cannot
be cancelled by unrelated pushes. The candidate contains the macOS N-API
package repair, the provider-matrix lock refresh, the workflow dispatch packet,
and the locally qualified runtime ancestry.

On the exact candidate checkout, `git diff --check`, formatting, the full
locked Rust workspace (`./scripts/cargo-shared test --workspace
--all-targets --locked`), and strict workspace Clippy all passed. The S3
gateway suite passed `40/40`, the optimized N-API build passed, and the full
Node/N-API SDK/CLI suite passed with pinned MountX oracle revision
`85361a8212ff9bff8e69f62fa8993ef2c2ec51e8`. The exact-candidate PGlite
harness also passed real PGlite reconnect/versioning/VFS/lifecycle/
split-store/FUSE checks, Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`,
upstream `1200 passed / 82 skipped`, and all 40 five-seed/eight-backend
trace combinations at 621 operations. Provider credentials, live R2, TiDB /
RustFS, FoundationDB opt-in, and privileged native mounts remain explicit
skips rather than failures.

### Latest exact local qualification packet (2026-09-22 17:53 AEST)

The repaired immutable candidate branch
`andymac4182/c/w05-production-candidate-20260922b` is pinned to exact
`87f3cdf0a8b3d29c89ff6c1e8d6cbd2409d0c01d` (`87f3cdf0`). On that exact
checkout, the shared-target Rust qualification passed formatting, `git
diff --check`, the full locked workspace test suite, and strict workspace
Clippy with `-D warnings`. The optimized release N-API build completed
successfully in 3m31s. The complete pinned-oracle Node/N-API suite passed
typecheck, SDK/CLI, WebDAV durability/concurrency, S3 restart and scope,
FUSE/NFS/9P codec and differential coverage, Rust-backed sessions, host
restart parity, distribution/export checks, and artifact aggregation; the
suite produced only explicit capability/provider skips for unset R2/PGlite/
FoundationDB credentials and opt-in privileged mounts.

The same exact checkout passed `scripts/test-pglite.sh`: Rust SDK `6 pass /
3 skip / 0 fail`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200 passed / 82
skipped`, and all 40 five-seed/eight-backend trace combinations at 621
operations each against MountX oracle revision
`85361a8212ff9bff8e69f62fa8993ef2c2ec51e8`. The packet includes real PGlite
reconnect/versioning/VFS/lifecycle/split-store/FUSE, backup/restore rollback,
cleanup-failure fail-closed, and chunked integration checks. No R2 secret,
AWS value, or Keychain item was read. This is a complete local
implementation/package/SDK/CLI qualification, not hosted/provider/native
release acceptance; production remains **NO-GO**.

Hosted candidate runs, all on the same exact SHA at the capture boundary,
are: CI `35694325175` terminal `failure`; Fault injection `35694329117` successful; W04
production policy `35694326676` successful; W08 release policy `35694327019`
successful; W08 release targets `35694328409` successful, superseded as the
provenance authority by attestation-enabled `35697156046`; W07 FoundationDB
production qualification `35694328112` successful with both jobs green; and
Native 9P `35694328753` successful. Candidate push-triggered Fault injection
`35694307087` also completed successfully on all three operating systems, but
the manual run remains the primary same-SHA fault record. CI is not release
acceptance: the terminal run has five failed job-level gates and one cancelled
native-FUSE gate, even though the aggregate-native job passed. These are
current hosted observations, not a final release decision.

The read-only AWS audit remains blocked with the exact safe summary
`AWS_S3_OIDC_AUDIT_BLOCKED invalid_role_arn_shape github_repository_unreadable
github_environment_unreadable missing_environment_variable_MOUNT_RS_AWS_S3_TEST_BUCKET
missing_environment_variable_MOUNT_RS_AWS_S3_TEST_REGION
missing_environment_variable_MOUNT_RS_AWS_S3_ACCOUNT_ID
missing_environment_variable_MOUNT_RS_AWS_S3_TEST_VERSIONING_STATUS
missing_environment_secret_MOUNT_RS_AWS_S3_CI_ROLE_ARN`. Live R2 remains
fail-closed at the September monthly envelope and no new provider attempt is
admitted. Production remains **NO-GO** until the candidate hosted packet,
security-provisioned provider inputs, native/platform and package gates,
scope decision, and W20.6 audit are complete.

Security request [#3](https://github.com/andymac4182/mount-rs/issues/3) is now
open for the protected AWS environment, immutable OIDC trust, least-privilege
short-lived role, and a fail-closed AWS test budget of no more than `$100`
per month. The request contains no credential value and explicitly excludes
Keychain access.

### Terminal candidate CI classification (2026-09-22 17:35 AEST)

Candidate CI `35694325175` is now terminal at the immutable SHA
`25e275ab6d4f918be72dcd8f62a5baca6bbcd251`, with overall `failure`. The
terminal failure set is separated here so implementation work is not promoted
to a provider or hosted acceptance:

- TiDB `106637769088` and TiDB/RustFS `106637769305` failed the ignored
  ambiguous-publication test because the proxy dropped only an explicit
  `COMMIT`, while the current provider publishes through an autocommit
  conditional `UPDATE`. The failure-injection harness is corrected and pushed
  in `f94b53d8`; the real TiDB service rerun remains required.
- Ozone/TiDB `106637769210` and Ozone/FoundationDB `106637769419` completed
  lifecycle correctness and cleanup but failed the hard W26 IOPS floor of
  `1000`: observed approximately `389.44` and `83.82` successful lifecycle
  IOPS respectively. These are hosted/provider performance gates; the floor
  is not being reduced or bypassed.
- W26 evidence `106648759164` failed because the benchmark artifact recorded
  the source checkout dirty (`dirtyEntryCount=1`). The W26 composition and
  TiDB jobs now build the real N-API binary in `$RUNNER_TEMP` and use
  `NAPI_RS_NATIVE_LIBRARY_PATH`, then assert the checkout is clean before
  qualification; a new candidate run must verify that repair.
- Native FUSE `106637769402` failed its actual rootless kernel file-operation
  step and was later cancelled while GitHub's completion step hung. No
  passing native-FUSE evidence is inferred from the cancellation.

The same run's Rust, Node, Windows Node, NFS, WebDAV, 9P, Ozone base/
composition, RustFS, FoundationDB/RustFS, HTTP-observability, macOS Node, and
aggregate-native jobs passed. This is useful diagnostic evidence, not a green
release packet: the implementation fix is on shared main, but the hosted
rerun, hard performance gates, native FUSE, AWS security/OIDC, R2 reset and
rotation, package/publication, support-scope, and W20.6 gates remain open.

**W05 functional completion: 100%; production-readiness completion: 68%
provisional.** The scoped CI credentials, fail-closed cost admission guard,
local provider coverage, Rust/Node SDK and CLI matrices, bounded benchmark
packet, and hosted Cloudflare R2 acceptance all pass. The authoritative W05
run is `35579757447` at commit `3db491e`; its budget gate accepted run `12/20`
with an estimated maximum monthly envelope of `$80.00`, and its live job
passed the full packet and uploaded the benchmark artifact. Since that
acceptance, hosted CI exposed and the implementation fixed the shared S3
streaming-publication and staged-upload retention regressions. The latest
N-API packaging defect was also fixed in `postbuild.mjs` and pushed as
`90df85b`: clean release generation now preserves the public
`P9AttachOptions` declaration referenced by `P9Server.attach()`.

Historical current-main qualification captured at 10:46 AEST was exact
`db431f4ccfff45c329fcc12f20f4e04f332a77c3` (`db431f4c`). This revision is
an earlier fetched and pushed `origin/main` boundary. It contains the same generated
N-API P9 absence-shape fix as `730a3de4`; clean release generation now keeps
the public `undefined` contract for P9 lock/session optional values. On that
exact SHA, formatting, the full locked Rust workspace with permitted
loopback binds, strict workspace Clippy, the optimized N-API build, the
complete current Node/N-API suite, and the complete real PGlite matrix all
passed. The current Node suite includes P9 metadata/locks, WebDAV direct and
network concurrency, provider concurrency, SQLite/NodeFs crash/restart,
in-flight recovery, CLI, distribution, and artifact aggregation. The exact
PGlite packet reports Rust SDK `6 pass / 3 skip / 0 fail`, Node SDK `5/3/0`,
CLI `12/2`, upstream `1200 passed / 82 skipped`, and all 40
five-seed/eight-backend traces at 621 operations. Provider rows remain
explicitly skipped when credentials are absent. This is the current local
packet, not hosted/provider/native/package-publication acceptance.

Current-tip boundary captured at 10:54 AEST: `origin/main` is now exact
`1bdd6adfe6e984a5e7e27ddf0edfb3bbd9d24462` (`1bdd6adf`), adding the FUSE
busy-unmount forced-teardown handoff. The focused current-tip gate passed
formatting, all 61 non-native `mount-rs-fuse` package tests, and strict
package Clippy; the latest complete full Rust/Node/PGlite packet remains
`db431f4c`, and this FUSE delta has not been promoted to a new full packet.
The hosted Native 9P dispatch observed during this moving-mainline window was
revision-specific and remains non-terminal; no native mount acceptance is
inferred locally on this macOS host.

Current moving-mainline boundary at 11:02 AEST: exact `origin/main` is now
`ff29dad70e5b3d3a832e99ef55f4fce2967448aa` (`ff29dad7`). The successor adds
documentation and site-provider/transport display changes only over the
qualified implementation; no Rust or Node runtime source changed after the
`1bdd6adf` FUSE focused gate. The exact `03cca165` hosted Native 9P run
passed both jobs, but CI, fault, W04, and W08 runs on that revision were
cancelled when this moving mainline advanced. Therefore no terminal hosted
release packet is promoted to `ff29dad7`.

An earlier complete local qualification spans exact pushed revision
`9354362a`:
format, the full locked Rust workspace, strict Clippy, optimized N-API build,
the complete pinned-oracle Node/N-API suite, and the real PGlite matrix were
exercised. The packet includes the 9P property-shaped server clients and
observability/driver surface, NFS session error hooks, serialized S3 multipart
terminal transitions, N-API structural factory parity, expanded chunked
stale-read/concurrency coverage, WebDAV lifecycle safety, FUSE `syncfs`,
forced cancellation, W04 lease-TTL validation, and WebDAV SQLite reopen. It
passed real PGlite reconnect/versioning/VFS/lifecycle/split-store/FUSE checks,
Rust SDK `6 pass / 3 skip / 0 fail`, Node SDK `5/3/0`, CLI `12/2`, upstream
`1200 passed / 82 skipped`, and all 40 five-seed/eight-backend traces at 621
operations. The full packet therefore qualifies `9354362a`. It includes the
WebDAV timed-out-drain and concurrent-client coverage, NFS owner/lease
callback bridge and session-member views, bounded connection-shutdown
cancellation repair, chunked atime coalescing, FoundationDB lease-policy
validation, and 9P mount-helper coverage. No hosted/native/package/provider
acceptance is promoted from the local boundary.

The current release decision is **NO-GO**. Production readiness requires the
dependency and release gates in the production-readiness register below, not
just the W05 provider pass. The locally qualified working tree contains the
S3 publication and staged-upload repairs, absolute Windows long-path repair
`9b396c2`, FoundationDB/RustFS harness and authority-clock work, AWS
protected-input preflight and PGlite pairing work, W08 release-manifest
checks, Ozone provider matrix work, the production-readiness documentation,
the latest FoundationDB fail-closed changes, the N-API declaration fix, the
chunked immutable-write overlap repair, the FUSE syncfs codec, W04 explicit
lease-TTL/configuration code, and the WebDAV SQLite reopen path. The full
packet revision `9354362a` is locally qualified and includes the 9P
property-shaped server client surface plus the newer NFS/S3/chunked/N-API/WebDAV
changes. Concurrent shared work has since advanced origin/main to `61b904c3`,
which includes the bounded chunked mutation queue, 9P mount-source/policy
parity, FUSE forced-teardown/read-worker/native-session fixes, WebDAV crash
recovery and concurrency coverage, and newer public 9P constants plus NFS
oracle-refresh documentation; it is not yet promoted from its focused
qualification boundary.
Hosted W07 `35623069365` completed successfully on `1acc15f`, with
all durable FoundationDB/RustFS/Node/CLI/restart/evidence steps green, but it
is still revision-specific. The 2026-09-22 08:23 AEST Actions snapshot shows
current origin/main head `b27dd2bb` with CI `35662346488` queued (and duplicate
CI dispatch `35662257713` queued), W04 production policy `35662257709` pending,
W08 release policy `35662257731` pending, fault injection `35662257717` queued,
and W08 release targets `35662257718` pending. The preceding R2 workflow
`35662206094` on `5026a230` remains queued only at the admission job and was
superseded before any live job; it is not provider evidence. No terminal
hosted workflow for the tested `6f29d9ce` or current `b27dd2bb` was present in
that snapshot.

Current-boundary update captured at 08:41 AEST: `origin/main` is now
`ccd3f671`, adding direct 9P mount-signal teardown, wider WebDAV session
concurrency, Ozone content-addressing coverage, and a FoundationDB
qualification lock refresh after the `b27dd2bb` snapshot. The exact pushed
`eaf59894` packet is green locally; `ccd3f671` is the next unqualified
successor. Current Actions are queued/pending on `ccd3f671`, and R2
`35663627575` failed closed at the monthly envelope on predecessor
`578cd794` before the live job and secret handoff.
Current-boundary update captured at 08:50 AEST: `origin/main` is now
`e7888067`, adding hosted 9P native-lifecycle coverage, WebDAV session-member
tests, Ozone cleanup deduplication, and fragmented HTTP early-rejection
coverage after the `a0c73c54` packet. The exact pushed `a0c73c54` packet is
green locally; `e7888067` is the next unqualified successor. Current Actions
are queued/pending on `e7888067`, while R2 `35664438822` failed closed at the
monthly envelope on predecessor `229a9cd5` before the live job and secret
handoff.
Current-boundary update captured at 08:56 AEST: `origin/main` is now
`d391f9b7`, which widens the WebDAV direct-session and network concurrency
probes to 64 concurrent PUT/GET pairs and carries the immediately preceding
NFS process-crash fence and hosted native-9P workflow. On exact `d391f9b7`,
the direct-session WebDAV probe passed 64 concurrent PUT/GET pairs, and the
network probe passed 64 concurrent HTTP PUT/GET pairs plus streaming,
authentication, and error-callback assertions. The exact current-tip NFS
package passed 38 Rust tests, process-crash/restart tests, rootless wire,
transport, and v4 suites, with strict package Clippy green. The local direct
9P N-API probe correctly skipped because this macOS host has no opted-in
Linux/kernel 9P mount boundary. Current Actions for `d391f9b7` are
R2 `35665105299` pending, Native 9P `35665105178` queued, W08 policy
`35665105293` pending, fault `35665105186` queued, W08 targets
`35665105417` pending, CI `35665105286` pending, and W04 policy
`35665105441` pending. None is terminal release evidence.
Current-boundary update captured at 09:13 AEST: exact pushed `b65d4e31` passed
the complete local release packet: format, full locked Rust workspace,
strict Clippy, optimized N-API build, the complete pinned-oracle Node/N-API
suite, and the full real-PGlite matrix. The packet reports Rust SDK `6/3/0`,
Node SDK `5/3/0`, CLI `12/2`, upstream `1200 passed / 82 skipped`, and 40
five-seed/eight-backend traces at 621 operations. The exact packet also
passed the 64-way WebDAV probes, NFS process-crash/restart tests, injected
PGlite cleanup failure gate, and artifact aggregation. `origin/main` has
since advanced to `9b2dabd7` with bounded FUSE handoff changes, chunked
concurrent-create batching, 9P mount-source parity, WebDAV NodeFs crash
recovery and session-parity metadata, and related documentation; those are
the next unqualified successor surfaces. Current Actions for `9b2dabd7` are
R2 `35666473009` queued, Native 9P `35666472979` pending, W08 policy
`35666472973` pending, fault `35666473104` queued, W08 targets
`35666473078` pending, CI `35666472961` pending, and W04 policy
`35666473004` pending. No current-head hosted gate is terminal.
Current-boundary update captured at 09:30 AEST: exact pushed `9354362a` passed
the missing format check plus the complete local release packet: full locked
Rust workspace tests, strict workspace Clippy, optimized N-API build, complete
pinned-oracle Node/N-API suite, and the full PGlite/provider matrix. The exact
packet reports Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream
`1200 passed / 82 skipped`, and all 40 five-seed/eight-backend traces at 621
operations. PGlite lifecycle, backup/restore rollback, split-store/VFS,
cleanup-failure fail-closed, WebDAV NodeFs crash/restart, 64-way WebDAV
concurrency, NFS process-crash/restart, FUSE package tests, chunked concurrent
publication, and artifact aggregation also passed. This is exact local
evidence only: before ledger publication, `origin/main` had advanced to
`61b904c3` with new WebDAV/9P/NFS changes. The ledger was then rebased onto
concurrent `0ad4928e` and published as `0aa25ad7`; current `origin/main` is
that ledger checkpoint, and its underlying successor code remains unqualified.
The GitHub API was unreachable during this refresh; no hosted status is
inferred, and no hosted/native/provider/package-publication or final-audit
acceptance is promoted.
The latest completed R2 safety refusal `35628414156` failed closed at
`count=139 limit=20`, and the latest documented AWS gate failed closed at
`missing_bucket`; current provider, native, package-publication, and
final-audit gates remain open.
These are not a release pass. The final release revision still needs the
complete local and hosted release packet, including the provider, native,
package, and scope gates when those surfaces are in scope.
AWS remains blocked by security-provisioned inputs, and no current-head live
R2, FoundationDB, native, package-publication, or final audit acceptance is
claimed. No credential value is stored in the repository or in this document.
Native/platform gates remain explicitly bounded: macOS native NFS
qualification is local evidence; Linux FUSE, Windows, and signed/activated
FSKit acceptance are separate platform workstreams.

## Active completion goal

The production-readiness goal remains active. It is not complete merely
because the W05 R2 slice is green: completion requires the production
dependency register below to reach 100% or have an explicit scope exclusion,
one revision with terminal green required CI/fault/provider/native/package
evidence, security-provisioned external credentials, clean Rust and Node
SDK/CLI installation checks, documented support boundaries, and a written
W20.6 GO decision. Open provider, hosted, native, packaging, AWS, and scope
gates therefore remain actionable work in this session.

## Work-item ledger

| Work item | Work type | Status | Completion | Evidence | Remaining actions | Provisional engineering time | External blockers / boundaries |
| --- | --- | --- | ---: | --- | --- | --- | --- |
| W05.0 Land the R2 object-store driver and configurable endpoint support | Implementation | Complete | 100% | `integrations/mount-rs-r2`, endpoint validation, signed HTTP adapter tests, focused Rust tests and Clippy passed. | None for the implementation item. | 0 h | Full acceptance is tracked separately below. |
| W05.1 Unblock D01 and authenticate against actual Cloudflare R2 | Provider / CI configuration | Complete; rotation is scheduled maintenance | 100% | Bucket-scoped Object Read & Write credentials are stored as encrypted GitHub `r2-ci` environment secrets: `MOUNT_RS_R2_ENDPOINT`, `MOUNT_RS_R2_BUCKET`, `MOUNT_RS_R2_ACCESS_KEY_ID`, and `MOUNT_RS_R2_SECRET_ACCESS_KEY`. The active CI token is one-week TTL and limited to `mount-rs-integration-tests`. Final hosted budget and live jobs consumed the secrets successfully without repository plaintext. | Rotate the four secrets before 2026-09-28 and rerun the bounded lane after rotation if the workstream remains active. | 0.5–1 h provisional maintenance | The macOS Keychain item was not readable through the requested security authorization path; CI was provisioned through the authenticated Cloudflare UI and GitHub encrypted secrets instead. |
| W05.2 Verify immutable writes, ranges, retries, reconnect, cleanup, and concurrent publication with independent metadata providers | Implementation + provider gate | Complete locally and hosted | 100% | Local live R2 passed filesystem/CAS, SQLite-metadata/R2-block, PGlite-metadata/R2-block, fresh-client/reopen, concurrent publication, range, retry, prefix isolation, and exact cleanup cases. Hosted run `35579757447` passed the Rust backend gate, the PGlite/R2 block matrix, and the live bounded R2 trace; all cleanup-owned prefixes were verified by the packet. | None for the requested W05 acceptance. | 0–0.5 h provisional follow-up | Cloudflare HTTP latency is variable; hosted evidence remains distinct from local signed-S3-compatible evidence. |
| W05.3 Run Node SDK, Rust/Node CLI, N-API/native, parity, and benchmark lanes on live R2 | Mixed implementation + hosted/native/provider gates | Complete for requested hosted acceptance; native/platform boundaries remain separate | 100% | Local evidence: Rust SDK `pass=9 skip=0 fail=0`; Node SDK `pass=7 skip=1 fail=0`; CLI `pass=14 skip=1 fail=0`; upstream `4` files, `1200 passed`, `82 skipped`; five-seed trace parity passed; live R2 benchmark and full public-NAPI benchmark passed. Final hosted run `35579757447` also passed the Rust/Node/CLI matrices, bounded live R2 trace, live CLI, N-API, service evidence, and benchmark artifact. | None for the requested W05 acceptance. Rotate the short-lived CI token as scheduled maintenance. | 0 h acceptance work; 0.5–1 h provisional rotation maintenance | macOS native NFS qualification passed locally. Linux FUSE/privileged native acceptance is a separate platform boundary and is not claimed by this W05 ledger. |
| W05.4 Record redacted service identity and revision without credentials | Implementation / evidence hygiene | Complete | 100% | `scripts/r2-service-evidence.sh` reports service, endpoint authority, bucket, revision, and owned-prefix counts without secret values; embedded endpoint credentials are rejected. Local and final hosted service-evidence gates passed. | None. | 0 h | Hosted log review must stay redacted; secret-bearing environment values are masked by GitHub. |
| W05.5 Fix the live Node factory expected-byte assertion and guarantee unique fixture keys with exact cleanup | Implementation + provider gate | Complete locally and hosted | 100% | PGlite/R2 Node factory, DELETE/HEAD cleanup, chunked factories, restart/fencing, userspace FUSE, and full N-API test suite passed locally with the pinned oracle. Final hosted run `35579757447` passed the Node SDK/provider rows, hosted N-API suite, exact cleanup, and artifact upload. | None for the requested W05 acceptance. | 0 h | Hosted runner queue and Cloudflare latency can affect elapsed time, not the implementation result. |
| W05.6 Run the configuration-driven CLI gate against the canonical Cloudflare endpoint | Provider / CLI hosted gate | Complete locally and hosted | 100% | Standalone live CLI gate passed locally for PGlite-metadata/R2-block and SQLite-metadata/R2-block, ranged reads, auth isolation, graceful reopen, presence checks, and owned-prefix cleanup. Final hosted run `35579757447` passed the provider-matrix CLI rows and dedicated live Rust CLI script, including graceful shutdown and owned-prefix cleanup. | None. | 0 h | Requires the encrypted `r2-ci` secrets and a live GitHub runner; no credential is permitted in the CLI fixture or repository. |
| W05.7 Add usage-capped CI credentials, cost guard, and full hosted acceptance | Implementation + hosted/provider gate | Functional acceptance complete; monthly admission exhausted | 100% | `.github/workflows/cloudflare-r2.yml` uses the encrypted `r2-ci` environment and a fail-closed budget job. Final run `35579757447` accepted `12/20` for September with an estimated maximum monthly envelope of `$80.00`, then passed Rust backend, Rust SDK `9/0/0`, Node SDK `7/1/0`, CLI `14/1/0`, upstream `1200 passed/82 skipped`, five-seed local/PGlite trace coverage, bounded live R2 trace `621/621`, live CLI, N-API, service evidence, and benchmark artifact `10630468958`. Later attempts, including `35589628621`, were refused before secret use at `count=29 limit=20`; the Cloudflare account-wide `$80` alert remains configured. | Rotate the four `r2-ci` secrets before 2026-09-28. Do not request another live-R2 attempt until the UTC month resets and the rotated token is ready; then run exactly one bounded requalification on the release revision. | 0.5–1 h rotation/requalification setup; hosted wait separate | The run-count/per-run envelope is the enforced fail-closed control and is intentionally conservative because it counts workflow attempts. Cloudflare alerts notify but do not hard-pause account usage; a provider-side hard stop at exactly `$100` remains unavailable as an R2 alert feature. |
| W05.9 Requalify the release revision and make Node packaging deterministic | Implementation + local release gate | Full packet green on `6f29d9ce`; current shared successor hosted/provider gates remain open | 100% | On exact `6f29d9ce`, format, the full locked Rust workspace, strict Clippy, optimized N-API build, complete pinned-oracle Node/N-API suite, and the full PGlite matrix passed. The matrix reports Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and 40 five-seed/eight-backend traces at `621` operations each. The packet includes WebDAV session/member differential coverage, NFS session-member views and cross-process recovery ancestry, FoundationDB lease-policy validation, 9P mount helpers, chunked concurrent mutation coverage, FUSE drain behavior, S3 cancellation/ETag/allocation-order coverage, and the current generated/package surfaces. Clean release generation retains the public N-API declarations, including 9P property-shaped clients and NFS callback options. | Requalify current `b27dd2bb` after the chunked/9P/Ozone/NFS/FUSE changes; retain terminal hosted CI/package evidence on the final release revision; rerun live R2 only after cap reset and security-approved token rotation; obtain AWS inputs; close native/platform and package-publication rows. | 0 h local implementation; 2–6 h hosted/provider follow-up | R2 is blocked by the deliberate monthly cap; AWS OIDC inputs are absent; native mounts, platform runners, registries, and signing remain external gates. |
| W05.10 Resolve the hosted S3 streaming-publication regression | Implementation + integration/release gate | Implementation complete; current shared-tip requalification pending | 96% | Hosted CI at `7ed1075` exposed nested streaming PUT `404 NoSuchKey` and test-driver `501 NotImplemented`. `39a19fd` creates destination parents before streaming/multipart publication, makes root-parent creation a no-op, and delegates the test-driver `rename`; `56dfcb2` adds staged-upload retention repair; `99e9729d` adds cancelled-staging cleanup; `db369dfb` preserves multipart completion ETags; `14517cb2` matches multipart completion allocation order. Exact `6f29d9ce` local gateway coverage passed all 27 S3 tests, R2 HTTP interop, the optimized N-API suite, and the full PGlite matrix; current `b27dd2bb` adds chunked/9P/Ozone/NFS/FUSE changes and still needs the full packet; live R2 remains cost-denied. | Retain a terminal unsuperseded hosted CI pass for the final release revision, including Rust HTTP interop and Node HTTP parity, and keep the regression tests as release evidence. | 0.75–1.5 h active engineering completed; hosted queue time separate | Hosted workflow cancellation and provider/network behavior remain external; no live R2 attempt is allowed before the UTC-month cap resets. |
| W05.11 Bound S3 staging retention, quota, and honest cleanup | Security hardening + implementation + integration gate | Implementation complete; exact `6f29d9ce` local retention/quota tests green; current shared-tip qualification pending | 92% | `89992ce` bounds multipart/temporary streaming staging at 8 GiB and 24 h, reaps stale staging, returns `SlowDown` on quota exhaustion, and makes `DeleteObjects` remove or report the backing staging tree. `56dfcb2` preserves active uploads when timestamp capability is missing/zero; `99e9729d` adds cancellation cleanup; `db369dfb` preserves completion ETags; `14517cb2` preserves allocation order. Exact `6f29d9ce` gateway tests passed all 27 multipart retention/quota/reaping, terminal races, honest cleanup, completion-ordering, cancellation, and session-close cases; current Node/PGlite paths also pass. | Requalify current `b27dd2bb`, retain a terminal current-head hosted pass, validate retention/quota under advertised providers, and record operational cleanup and alerting in the release runbook. | 0 h active implementation; 1–2 h hosted/operations follow-up | Capacity, clock/mtime behavior, long-running uploads, and provider-specific cleanup remain hosted/provider gates; R2 is cap-blocked. |
| W05.12 Track the production closure path and security-gated provider preflights | Release engineering + security/provider + hosted gate | Active; external gates open | 68% | The active goal and PR-00 through PR-10 register enumerate implementation/native/hosted/provider/package/scope boundaries. The fresh read-only `./scripts/audit-aws-s3-ci-oidc.sh` audit returns `AWS_S3_OIDC_AUDIT_BLOCKED invalid_role_arn_shape github_repository_unreadable github_environment_unreadable missing_environment_variable_MOUNT_RS_AWS_S3_TEST_BUCKET missing_environment_variable_MOUNT_RS_AWS_S3_TEST_REGION missing_environment_variable_MOUNT_RS_AWS_S3_ACCOUNT_ID missing_environment_variable_MOUNT_RS_AWS_S3_TEST_VERSIONING_STATUS missing_environment_secret_MOUNT_RS_AWS_S3_CI_ROLE_ARN`; it never reads a secret value. The latest observed current-tip hosted R2 admission is still absent; prior R2 `35660482762` failed closed before live admission on `6a19ff65`, and current R2 attempts remain cost-denied and are not provider evidence; AWS `35625592317` failed closed at `missing_bucket`, and the latest captured AWS run remains non-terminal. W07 `35623069365` completed successfully on `1acc15f` but is revision-specific. The full local Rust/Node/PGlite/CLI packet is green on exact `6f29d9ce`, including 27 S3 gateway tests, WebDAV session/member differential coverage, NFS cross-process recovery ancestry, chunked concurrent mutation coverage, FUSE drain behavior, FoundationDB lease-policy, 9P mount helpers, and current generated/package surfaces; current `2d459e15` adds chunked cancellation, 9P policy mapping, Ozone lockfile, and NFS recovery changes without terminal acceptance. The current W25 security record says Standard scan `02d2c6eb-66e1-41f8-be59-d14aab9fde87` has zero reportable findings across six W25 surfaces but partial repository coverage; hosted run `35629600687` passed provenance and synthetic contracts, then failed closed at `AWS_S3_CI_CONFIG_BLOCKED missing_bucket`, and the existing test-role trust still permits only the SSO administrator rather than GitHub OIDC. | Security must provision the protected AWS environment inputs and immutable OIDC trust through the approved security path (not keychain access); rotate R2 secrets after 2026-09-28; obtain terminal CI, fault, W07, native, package, and provider results on one revision; resolve PR-08 scope; run W20.6 and record GO/NO-GO. | 2–5 h active release coordination/documentation; 8–20 h remaining implementation/provider closure plus hosted wait | Security administration, provider accounts, hosted concurrency, R2 UTC-month cap, platform signing/entitlements, registries, and product scope decisions are external gates. |
| W05.13 Requalify current shared head across Rust, Node, SDK, CLI, PGlite, and oracle paths | Local release qualification | Full packet complete on `6f29d9ce`; current `b27dd2bb` successor remains open | 100% | On exact `6f29d9ce`: format, full locked Rust workspace tests, strict Clippy, optimized N-API build, complete N-API suite including 9P property-shaped clients and mount helpers, P9 observability/driver, FUSE syncfs and drain behavior, chunked stale-read/concurrency plus atime and concurrent mutation coverage, WebDAV lifecycle/concurrency/session-member differential coverage, NFS owner/lease callbacks plus shutdown cancellation, FoundationDB lease-policy tests, and the repaired `scripts/test-pglite.sh` all passed. The matrix reports real PGlite backend/reconnect/versioning/VFS/lifecycle/split-store/FUSE, Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and 40 five-seed/eight-backend combinations at 621 operations. Native mount and live-provider rows remain explicit skips/blockers; hosted CI, live R2/AWS, FoundationDB/TiDB/RustFS, Windows, FSKit, and package-publication gates remain open. | Carry the evidence onto current `b27dd2bb` and then one final hosted release revision; rerun bounded live R2/AWS/provider packets only with security-approved credentials and retain terminal run IDs. | 0 h implementation; 1–3 h hosted evidence reconciliation | The pinned oracle and local PGlite are not hosted provider acceptance; native mount, live R2, AWS, FoundationDB/TiDB/RustFS, Windows, FSKit, and package publication remain external gates. |

| W05.14 Qualify the pushed current tip after FUSE/9P/WebDAV/NFS/R2 changes | Local release qualification + hosted evidence | Full packet green on `eaf59894`; current `ccd3f671` successor remains open | 100% local packet / 0% current-tip hosted closure | Exact pushed `eaf59894` passed format, full locked Rust workspace, strict Clippy, optimized N-API, complete Node SDK/CLI suite, real PGlite reconnect/versioning/VFS/lifecycle/split-store/FUSE checks, Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40 five-seed/eight-backend traces at 621 operations; injected cleanup failure remained fail-closed. Current `ccd3f671` adds direct 9P mount-signal teardown, wider WebDAV session concurrency, Ozone content-addressing coverage, and a FoundationDB qualification lock refresh, and is not locally requalified or hosted-accepted. | Re-run the complete packet on `ccd3f671` if it remains the selected release SHA; retain one terminal hosted CI/fault/W04/W08/provider/package/native result on that same SHA. | 1–3 h local requalification; hosted/provider wait separate | The current R2 lane is cap-blocked/nonterminal; AWS security inputs, platform runners, native mount privileges, signing, and package registries remain external gates. |
| W05.15 Requalify the next pushed release tip after 9P/WebDAV/Ozone/HTTP gates | Local release qualification + hosted evidence | Full packet green on `a0c73c54`; current `e7888067` successor remains open | 100% local packet / 0% current-tip hosted closure | Exact pushed `a0c73c54` passed format, full locked Rust workspace including 7-case NFS v4 concurrent-client coverage, strict Clippy, optimized N-API, complete Node SDK/CLI suite including direct WebDAV session concurrency, real PGlite reconnect/versioning/VFS/lifecycle/split-store/FUSE checks, Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40 five-seed/eight-backend traces at 621 operations; injected cleanup failure remained fail-closed. Current `e7888067` adds hosted 9P native-lifecycle coverage, WebDAV session-member tests, Ozone cleanup deduplication, and fragmented HTTP early-rejection coverage and is not locally requalified or hosted-accepted. | Re-run the complete packet on `e7888067` if it remains the selected release SHA; retain one terminal hosted CI/fault/W04/W08/provider/package/native result on that same SHA. | 1–3 h local requalification; hosted/provider wait separate | The R2 lane is cap-blocked/nonterminal; AWS security inputs, platform runners, native mount privileges, signing, and package registries remain external gates. |
| W05.16 Requalify the latest shared tip after NFS process-crash fencing and widened WebDAV probes | Local changed-surface qualification + hosted/native evidence | Focused current-tip gates green; full packet and hosted closure open | 65% | Exact current `d391f9b7` passed direct WebDAV session concurrency with 64 concurrent PUT/GET pairs and network WebDAV concurrency/auth/streaming/error-callback coverage with 64 concurrent HTTP PUT/GET pairs. The same exact tip passed the NFS package: 38 Rust tests, process-crash/restart tests, rootless wire, transport concurrency/errors/lifecycle, v4 commit barrier, and seven v4 wire tests; strict package Clippy passed with `-D warnings`. The local N-API direct-9P probe reported the documented skip because macOS lacks the opted-in Linux/kernel 9P mount boundary. | Run the full format/locked Rust/strict Clippy/optimized N-API/complete Node/PGlite packet on the selected final SHA; retain terminal CI, fault, W04, W08, Native 9P, package, AWS, and provider results on that same SHA. Do not admit live R2 before the UTC-month reset and security-approved token rotation. | 1–3 h local full requalification; hosted/provider wait separate | Native kernel 9P, Linux FUSE, Windows, FSKit/signing, AWS security inputs, package registries, hosted concurrency, and the R2 monthly cap remain external gates. |
| W05.17 Complete the full current-tip Rust/Node/SDK/CLI/PGlite packet | Local release qualification + hosted evidence | Full local packet green on `b65d4e31`; current `9b2dabd7` successor open | 100% local packet / 0% current-tip hosted closure | Exact pushed `b65d4e31` passed format, full locked Rust workspace, strict Clippy, optimized N-API, complete pinned-oracle Node/N-API suite, and real PGlite lifecycle/reconnect/versioning/VFS/split-store/FUSE checks. Rust SDK was `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40 five-seed/eight-backend traces passed at 621 operations; WebDAV direct/network concurrency, NFS process-crash/restart, cleanup fail-closed, and artifact aggregation also passed. Current `9b2dabd7` adds FUSE handoff, chunked batching, 9P source parity, WebDAV NodeFs crash recovery, and session-parity metadata changes and has not been requalified or hosted-accepted. | Re-run the complete packet on `9b2dabd7` or the selected final SHA; retain terminal CI, fault, W04/W08, Native 9P, package, AWS, and provider evidence on that same SHA. After the UTC-month reset and security-approved token rotation, run exactly one bounded live-R2 requalification. | 1–3 h local successor qualification; hosted/provider wait separate | R2 monthly cap, AWS security/OIDC inputs, hosted concurrency, Linux/kernel 9P and FUSE, Windows, FSKit/signing, registries, and package publication remain external gates. |
| W05.18 Complete the exact successor full Rust/Node/SDK/CLI/PGlite packet | Local release qualification + hosted evidence | Full local packet green on `9354362a`; current `61b904c3` successor open | 100% local packet / 0% current-tip hosted closure | Exact pushed `9354362a` passed format, full locked Rust workspace tests, strict workspace Clippy, optimized N-API build, complete pinned-oracle Node/N-API suite, and the full PGlite/provider matrix. Rust SDK was `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40 five-seed/eight-backend traces passed at 621 operations. PGlite lifecycle, backup/restore rollback, split-store/VFS, injected cleanup-failure fail-closed, WebDAV NodeFs crash/restart, 64-way WebDAV concurrency, NFS process-crash/restart, FUSE package tests, chunked concurrent publication, and artifact aggregation also passed. `origin/main` then advanced to `61b904c3` with additional WebDAV, 9P, and NFS changes; no current-head hosted status was available because the GitHub API was unreachable during refresh. | Requalify `61b904c3` or the selected final SHA; retain terminal CI, fault, W04/W08, Native 9P, package, AWS, and provider evidence on that same SHA. After the UTC-month reset and security-approved token rotation, run exactly one bounded live-R2 requalification. | 1–3 h local successor qualification; hosted/provider wait separate | R2 monthly cap, AWS security/OIDC inputs, hosted concurrency, Linux/kernel 9P and FUSE, Windows, FSKit/signing, registries, package publication, and unavailable hosted status remain external gates. |
| W05.19 Qualify the exact latest `origin/main` after WebDAV/9P/NFS successors | Local release qualification + hosted/provider evidence | Full current local packet green; hosted/provider/native/package closure open | 100% local packet / 0% current-tip release closure | Exact fetched `origin/main` `8ca6c2572782420ae65bd709e3e08b9670d78148` passed `cargo fmt --all -- --check`, the full locked Rust workspace with loopback permission, strict workspace Clippy (`-D warnings`), optimized `pnpm --dir integrations/mount-rs-napi build`, the complete current Node/N-API suite, and `scripts/test-pglite.sh`. The current Node suite passed WebDAV provider concurrency and in-flight crash/restart in addition to the prior W05 surfaces. The packet reports Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and 40 five-seed/eight-backend traces at 621 operations. The workflow-path helper passed both AWS/R2 workflows; the read-only AWS audit returned the exact missing security/OIDC inputs without reading any credential value. | Obtain terminal hosted CI, fault, W04/W08, AWS, Native 9P/platform, package/provenance, and provider evidence on this exact release SHA or a deliberately selected successor; after the UTC-month reset and security-approved R2 token rotation, run exactly one bounded live-R2 packet; complete PR-01 through PR-10 and record W20.6 GO/NO-GO. | 1–3 h local packet already spent; 2–8 h remaining release closure plus hosted/provider wait | R2 monthly cap and token rotation, AWS security/OIDC provisioning, hosted concurrency, Linux/FUSE/9P, Windows, FSKit/signing, registries, provider services, scope decisions, and final audit ownership remain external gates. |
| W05.20 Requalify the exact pushed `db431f4c` after the current N-API packaging fix | Local release qualification + package/integration gate | Complete locally; hosted/provider/native/package-publication closure open | 100% local packet / 0% current-tip release closure | Exact `db431f4c` passed `cargo fmt --all -- --check`, the full locked Rust workspace with permitted loopback binds, strict workspace Clippy (`-D warnings`), optimized N-API build, complete Node/N-API suite, and the full `scripts/test-pglite.sh` matrix. The clean build regenerated P9 `getlock`, `userFor`, `msize`, and `version` as the public `undefined` shapes; typecheck passed. Node passed P9 metadata/locks, WebDAV direct/network/provider concurrency (64 direct pairs and 32 HTTP provider pairs), SQLite/NodeFs crash/restart, in-flight recovery, CLI, distribution, and artifact aggregation. The exact PGlite packet passed Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40×621 oracle traces. | Carry this exact SHA through terminal hosted CI/fault/W04/W07/W08/AWS/provider/native/package/provenance jobs; keep live R2 admission blocked until the UTC-month reset and security-approved token rotation; close PR-01 through PR-10 and issue W20.6 GO/NO-GO. | 3–5 h active local qualification completed in this session; hosted/provider/platform wait remains separate | R2 monthly envelope, AWS protected environment/OIDC inputs, hosted concurrency, Linux/FUSE/9P privileges, Windows/FSKit/signing, provider services, registries, scope decisions, and final audit ownership remain external. |
| W05.21 Qualify the current FUSE busy-unmount forced-teardown successor | Focused implementation + local native-package gate | Focused current-tip gate green; full release packet and hosted/native closure open | 100% focused implementation / 0% current-tip release closure | Exact `1bdd6adf` passed `cargo fmt --all -- --check`, the full `mount-rs-fuse` package `--all-targets` suite with 61 passed and 0 failed tests, and strict package Clippy (`-D warnings`). The change classifies Linux helper `EBUSY`/resource-busy unmount failures, requests stop/abort, runs the bounded forced-unmount handoff, and records a transport error if the mount remains present. Native mount tests were not run on this macOS host. | Re-run the complete Rust/Node/PGlite packet on the selected final SHA; obtain terminal revision-matched CI, fault, W04/W07/W08, Native 9P/FUSE/Windows/FSKit, package/provenance, AWS, provider, and final-audit evidence. | 0.75–1.5 h focused qualification completed; 2–8 h remaining release closure plus hosted/platform wait | Linux `/dev/fuse`, native helper behavior, Windows/FSKit/signing, hosted concurrency, AWS security/OIDC inputs, provider services, registries, R2 monthly cap, and scope decisions remain external. |
| W05.22 Reconcile the latest moving mainline after the current hosted dispatches | Release engineering / hosted evidence | Current tip docs-only over qualified code; hosted final packet still open | 100% reconciliation / 0% final hosted closure | Exact `ff29dad7` is the latest fetched `origin/main` and contains documentation/site-only successors over the `1bdd6adf` code. Native 9P `35673957522` passed both jobs on exact `03cca165`; CI `35673926517`, fault `35673926515`, W04 `35673926516`, and W08 `35673926467`/`35673926542` were cancelled by subsequent mainline advancement, so none qualifies the final tip. The full local Rust/Node/PGlite packet remains green at `db431f4c`, with the focused FUSE delta green at `1bdd6adf`. | Let the mainline settle on a selected release SHA, rerun the complete local packet and required hosted CI/fault/W04/W07/W08/native/package/provider gates on that same SHA, and issue W20.6 GO/NO-GO. Keep R2 blocked by the monthly cap and AWS blocked until security/OIDC provisioning. | 0.5–1 h active reconciliation; hosted queue and external gates separate | Concurrent shared-main pushes, hosted concurrency cancellation, AWS security administration, R2 cap/reset and token rotation, provider accounts, platform runners, registries, and scope decisions remain external. |
| W05.23 Qualify the exact pushed N-API/S3/P9 successor and full PGlite packet | Local release qualification + package/integration gate | Complete on exact `9cb2eef1`; current `1670ceba` successor and hosted/provider/native/package closure remain open | 100% local packet / 0% current-tip release closure | Exact `9cb2eef1` passed the clean optimized N-API build and generated typecheck after the clean-build regression at `eb2f5602` exposed `Record<string, number>` versus the public `Map<string, number>` P9 stats declaration; `9cb2eef1` restores one canonical postbuild normalization. The complete Node/N-API suite passed, including S3 session/member differential, WebDAV structural-driver durability, P9 metadata/locks, chunked, restart, distribution, and artifact aggregation. Rust N-API package tests passed 18/18 and strict package Clippy passed. The exact real PGlite packet passed Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40×621 oracle traces. This is exact local evidence only; the later `1670ceba` 9P/S3/W07 successor needs fresh qualification. | Re-run the full format/locked Rust/strict Clippy/optimized N-API/complete Node/PGlite packet on `1670ceba` or the selected final SHA; obtain terminal same-SHA CI, fault, W04/W07/W08, AWS, provider, native, package/provenance, and final-audit evidence. Keep R2 admission blocked until the UTC-month reset and security-approved token rotation. | 2–4 h local/package qualification completed; 2–8 h remaining hosted/provider/platform closure | Concurrent shared-main pushes, R2 monthly cap and token rotation, AWS security/OIDC provisioning, hosted concurrency, Linux/FUSE/9P, Windows, FSKit/signing, provider services, registries, scope decisions, and final audit ownership remain external. |
| W05.24 Qualify the exact R2 block-upload successor and repair its standalone consumer lock | Local implementation + full release qualification | Complete on exact code tip `2d2b66ef` with lock repair; current `412c422e` successor and hosted/provider/native/package closure remain open | 100% tested local packet / 0% current-tip release closure | Exact `2d2b66ef` passed format, the full locked Rust workspace, strict workspace Clippy, optimized N-API build, and complete Node/N-API suite. The full PGlite packet initially exposed stale `tests/provider_matrix/Cargo.lock` after the R2 `tokio` in-flight-upload dependency landed; offline lock regeneration added exactly that direct edge, after which the packet passed Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40×621 traces. The R2 unit/gateway packet passed 19 unit tests, 2 HTTP interop tests, and 28 gateway tests in the full workspace run. The later `412c422e` 9P frame-assembler and WebDAV native-concurrency successor is not included in this evidence. | Commit and publish the standalone lock repair, requalify `412c422e` or the selected final SHA, and retain terminal same-SHA CI, fault, W04/W07/W08, AWS, provider, native, package/provenance, and final-audit evidence. Keep live R2 blocked until the UTC-month reset and security-approved token rotation. | 1–2 h lock repair and exact local packet; 2–8 h remaining hosted/provider/platform closure | Shared-main advancement, R2 monthly cap/token rotation, AWS security/OIDC provisioning, hosted concurrency, Linux/FUSE/9P, Windows/FSKit/signing, provider services, registries, scope decisions, and final audit ownership remain external. |
| W05.25 Qualify the exact integrated `6797a2d8` packet and reconcile same-SHA hosted outcomes | Local release qualification + hosted evidence | Full local packet green; same-SHA hosted partial, current `819c663e` successor open | 100% local packet / 0% current-tip release closure | Exact `6797a2d8` passed format, full locked Rust workspace tests, strict workspace Clippy, optimized N-API build, complete Node/N-API suite, WebDAV package tests/Clippy, and the complete real PGlite matrix. The packet reports Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40×621 traces. Same-SHA W04 `35677127007`, fault `35677126925`, W08 policy `35677126941`, and W08 targets `35677126934` were terminal successes; Live R2 `35677126924` failed and CI `35677127048` was cancelled, so neither is release acceptance. `origin/main` then advanced to `819c663e` with additional 9P/N-API/Ozone/RustFS/site changes that need fresh qualification. | Qualify `819c663e` or the selected final SHA; obtain a terminal same-SHA CI pass, determine and record the redacted R2 cost-gate outcome after the UTC-month reset, retain W04/W07/W08/fault/AWS/provider/native/package/provenance evidence, and complete the final W20.6 audit. | 2–4 h current full qualification; 2–8 h remaining hosted/provider/platform closure | R2 monthly cap and token rotation, AWS security/OIDC provisioning, hosted concurrency, Linux/FUSE/9P, Windows, FSKit/signing, provider services, registries, scope decisions, and final audit ownership remain external. |
| W05.26 Qualify exact shared tip `42ecd21e` and reconcile the next moving successor | Local release qualification + hosted evidence | Complete on exact `42ecd21e`; current `3de49e33` successor and hosted/provider/native/package closure remain open | 100% exact local packet / 0% current-tip release closure | Exact `42ecd21e` passed format, full locked Rust workspace tests, strict workspace Clippy, optimized N-API build, complete Node/N-API suite, and the full real PGlite matrix. Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40×621 traces passed. New S3 conditional PUT/CAS behavior passed in the 29-test gateway suite and the Node S3/restart/parity surfaces; 9P typed declarations/codecs and refreshed AWS/FoundationDB lockfiles passed. Same-SHA CI `35679133377`, W08 policy `35679133114`, and W08 targets `35679133090` were cancelled after shared-main advancement; no terminal same-SHA hosted release packet is claimed. | Requalify current `3de49e33` or deliberately select a final release SHA; obtain terminal same-SHA CI, fault, W04/W07/W08, AWS, provider, Native 9P/FUSE/Windows/FSKit, package/provenance, and final-audit evidence. Keep R2 admission blocked until the UTC-month reset and security-approved token rotation; do not treat the queued R2 `35679778383` as provider acceptance. | 3–5 h exact local packet completed; 2–8 h remaining hosted/provider/platform closure | R2 monthly cap and token rotation, AWS security/OIDC provisioning, hosted concurrency/cancellation, Linux/FUSE/9P, Windows, FSKit/signing, provider services, registries, scope decisions, and final audit ownership remain external. |

| W05.27 Qualify exact pushed `f1ee869a` and reconcile the 9P/FoundationDB successor | Local release qualification + hosted evidence | Complete on exact `f1ee869a`; current `7389be4d` successor and hosted/provider/native/package closure remain open | 100% exact local packet / 0% current-tip release closure | Exact pushed `f1ee869a` passed format, full locked Rust workspace tests, strict workspace Clippy, optimized N-API build, complete Node/N-API suite, and the full real PGlite matrix. The packet reports Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40×621 traces. It includes the chunked failed-shutdown lease-release test, 9P synchronous live-mount inspection/type surfaces, S3 conditional PUT/CAS gateway coverage, WebDAV detached-cleanup handling, cleanup-failure fail-closed, and artifact aggregation. Same-SHA W04 `35680042918` and W08 targets `35680043018` succeeded; CI `35680042985`, fault `35680043013`, and W08 policy `35680042906` were cancelled, so no hosted full-release acceptance is claimed. | Requalify current `7389be4d` or select a final release SHA; obtain terminal same-SHA CI, fault, W04/W07/W08, AWS, provider, Native 9P/FUSE/Windows/FSKit, package/provenance, and final-audit evidence. Keep R2 blocked until the UTC-month reset and approved token rotation. | 3–5 h exact local packet completed; 2–8 h remaining hosted/provider/platform closure | R2 monthly cap and token rotation, AWS security/OIDC provisioning, hosted cancellation, Linux/FUSE/9P, Windows, FSKit/signing, provider services, registries, scope decisions, and final audit ownership remain external. |
| W05.28 Qualify exact pushed `1bdf8846` after 9P/FoundationDB/WebDAV successors | Local release qualification + hosted evidence | Complete exact local packet; current `6d65716f`/`75248969` docs successors and hosted/provider/native/package closure remain open | 100% exact local packet / 0% current-tip release closure | Exact pushed `1bdf8846` passed `cargo fmt --all -- --check`, full locked Rust workspace tests, strict workspace Clippy, optimized N-API build, complete Node/N-API suite, and the full real PGlite/oracle matrix. It reports Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40×621 traces, plus WebDAV provider/network concurrency and crash/restart, S3 restart/barrel scope, P9/9P/FUSE/NFS/WebDAV differential surfaces, FoundationDB shape checks, chunked integration, distribution, and artifact aggregation. | Select the final release SHA and obtain terminal same-SHA CI, fault, W04/W07/W08, AWS, provider, Native 9P/FUSE/Windows/FSKit, package/provenance, and final-audit evidence. Keep R2 blocked until the UTC-month reset and approved token rotation. | 1–3 h exact local packet completed; 2–8 h remaining hosted/provider/platform closure | Current provider credentials were intentionally not injected locally; AWS security/OIDC inputs, R2 cap/reset/token rotation, hosted concurrency, Linux/FUSE/9P, Windows, FSKit/signing, provider services, registries, scope decisions, and final-audit ownership remain external. |
| W05.29 Record the post-push hosted checkpoint and preserve the production boundary | Release engineering / hosted evidence | Recorded; hosted release gate remains open | 100% snapshot / 0% release closure | Pushed ledger checkpoint `75248969` was followed by W04 success `35681358469`; fault `35681358424`, CI `35681358476`, and W08 policy `35681358387` were cancelled, while W08 targets `35681358456` remained in progress at the final bounded poll. The documentation-only checkpoint does not change the tested implementation or admit credentials. | Wait for a deliberate final SHA rather than repeatedly pushing documentation into a cancelling workflow set; then run and retain one terminal all-gates packet, including security-provisioned AWS and post-reset R2. | 0.5–1 h active reconciliation; hosted wait separate | Hosted concurrency, provider security administration, R2 UTC-month cap, native/platform runners, package registries, scope decisions, and final release approval remain external. |

| W05.30 Stabilize the WebDAV peer-reset and bounded-listing integration boundary | Implementation + Node integration gate | Complete locally; exact pushed revision requalified | 100% local implementation / 100% local gate / 0% hosted closure | Commit `de78011a` replaces the timing-sensitive complete-response WebDAV reset with a deterministic partial-request reset, preserves the original exercise failure instead of masking it during cleanup, and asserts `501` for the pinned structural JavaScript driver that cannot enforce bounded enumeration. Native and structural WebDAV phases pass, and the complete Node SDK/CLI suite passes including structural factory subprocesses. | Retain the test and behavior in the selected release SHA; obtain terminal same-SHA hosted CI/native/package evidence after hosted concurrency settles. | 0.5–1 h completed; hosted wait separate | The pinned JavaScript driver exposes only unbounded `readdir`, so WebDAV must fail closed for remote `Depth: 1` listings until a provider-side bounded API exists; this is an explicit support boundary, not a reason to materialize unbounded data. |
| W05.31 Requalify the exact pushed production candidate after the WebDAV fix | Local release qualification + hosted/provider/native evidence | Full local packet green on `de78011a`; current `d43f5ea4` successor requires fresh qualification; production closure open | 100% historical local packet / 0% current-tip closure | Exact pushed `de78011a` passed format, full locked Rust workspace, strict Clippy, optimized N-API, complete Node suite, real PGlite, Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and 40×621 traces. The exact hosted snapshot was revision-matched but non-accepting: R2 `35683264337` failed closed at the monthly cap before secrets; CI `35683264288` was queued with cancellation activity; Native 9P `35683264325` was queued; W08 policy `35683264328` was in progress; W04 `35683264285`, fault `35683264298`, and W08 targets `35683264327` were cancelled. | Requalify current `d43f5ea4` or deliberately select a later release SHA; get terminal green CI, fault, W04/W07/W08, Native 9P/FUSE/Windows/FSKit, package/provenance, AWS, provider, scope, and W20.6 evidence on that same SHA. Do not rerun live R2 until the UTC-month reset and approved short-lived token rotation. | 1–3 h historical packet completed; 2–8 h remaining hosted/provider/platform closure | R2 cap/reset, AWS security/OIDC provisioning, hosted concurrency, privileged native platforms, signing, registries, provider services, product scope, and final-audit ownership remain external. |
| W05.32 Requalify the exact d43/S3-observability successor and reconcile the current shared head | Local release qualification + hosted evidence | Complete exact local runtime packet on `175615c7`; current docs-only `9563d2db` head and production closure remain open | 100% local code packet / 0% hosted-provider-native-package closure | Exact pushed `175615c7265a49b4d07ed16c57e63e9ae771df26` passed `cargo fmt --all -- --check`, `git diff --check`, the full locked Rust workspace, strict workspace Clippy (`-D warnings`), optimized N-API build, complete Node SDK/CLI integration suite, and `scripts/test-pglite.sh`. It reports Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200 passed / 82 skipped`, and all 40 five-seed/eight-backend traces at 621 operations; the real PGlite lifecycle, backup/restore/rollback, split-store/VFS/FUSE, cleanup-failure, WebDAV, S3, 9P, NFS, differential, distribution, and artifact checks passed. Fetched/pushed `origin/main` `9563d2db` adds only documentation over this tested runtime and therefore does not require a new runtime packet, but it has no hosted release acceptance yet. Exact-SHA hosted W04 `35684095331`, fault injection `35684095300`, and W08 release policy `35684095289` succeeded; CI `35684095286` and W08 release targets `35684095309` were still in progress at capture. | Let the current head settle, then retain terminal same-SHA CI, W08 targets/policy, fault, W04/W07, AWS, provider, Native 9P/FUSE/Windows/FSKit, package/provenance, scope, and W20.6 evidence on the selected release revision. Do not admit live R2 while the monthly cap is closed; rotate the short-lived token through the approved security path after reset. | 3–5 h exact local packet completed; 2–8 h remaining hosted/provider/platform closure | R2 cap/reset and token rotation, AWS protected inputs/OIDC trust, hosted concurrency, privileged native platforms, signing, registries, provider services, product scope, and final-audit ownership remain external. No credential value was read, stored, printed, or placed in Keychain. |
| W05.33 Requalify the exact pushed S3 error-hook head and preserve cancelled hosted evidence | Local release qualification + hosted evidence | Complete exact local packet on `b3cfbcb4`; current `1179d9e3` 9P/WebDAV successor requires fresh qualification | 100% local code packet / 0% current-tip closure | Exact pushed `b3cfbcb419a2a24bd89750358c96701f98ba3130` passed the focused `delete_objects_reports_per_key_driver_errors_to_error_hook` regression, format, diff check, full locked Rust workspace (including 30 S3 gateway tests), strict Clippy, optimized N-API build, complete Node SDK/CLI suite, and real PGlite/oracle matrix. It reports Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200 passed / 82 skipped`, and all 40×621 traces. Same-SHA W04 `35684755110` succeeded; W08 targets `35684755036`, CI `35684755170`, W08 policy `35684755009`, and fault injection `35684755061` were cancelled, so no hosted full-release acceptance is claimed. | Requalify current `1179d9e3` or select a final SHA; retain terminal same-SHA CI, fault, W04/W07/W08, AWS, provider, Native 9P/FUSE/Windows/FSKit, package/provenance, scope, and final-audit evidence. Keep live R2 closed until the UTC-month reset and security-approved short-lived-token rotation. | 2–4 h exact local packet completed; 2–8 h remaining hosted/provider/platform closure | Concurrent shared-main pushes and hosted cancellation remain external; R2 cap/reset, AWS protected inputs/OIDC, native privileges/signing, provider services, registries, product scope, and final-audit ownership remain open. No credential value was read, stored, printed, or placed in Keychain. |
| W05.34 Requalify the exact 9P/WebDAV current head and preserve mixed hosted evidence | Local release qualification + hosted evidence | Complete exact local packet on `8d7e9f3d`; current `4f6e1048` WebDAV successor requires fresh qualification | 100% local code packet / 0% current-tip closure | Exact pushed `8d7e9f3d577daa22735d1e31079c0971c2e7eddd` passed format/diff checks, full locked Rust workspace, strict Clippy, optimized N-API build, complete Node SDK/CLI suite with 9P teardown/backpressure and WebDAV structural phases, and real PGlite/oracle matrix. It reports Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200 passed / 82 skipped`, and all 40×621 traces. Same-SHA W04 `35685285569`, fault injection `35685285680`, W08 policy `35685285584`, and W08 targets `35685285524` succeeded; CI `35685285624` was cancelled, so no hosted full-release acceptance is claimed. | Requalify current `4f6e1048` or select a final SHA; retain terminal same-SHA CI, fault, W04/W07/W08, AWS, provider, Native 9P/FUSE/Windows/FSKit, package/provenance, scope, and final-audit evidence. Keep live R2 closed until the UTC-month reset and security-approved short-lived-token rotation. | 2–4 h exact local packet completed; 2–8 h remaining hosted/provider/platform closure | Concurrent shared-main pushes and hosted cancellation remain external; R2 cap/reset, AWS protected inputs/OIDC, native privileges/signing, provider services, registries, product scope, and final-audit ownership remain open. No credential value was read, stored, printed, or placed in Keychain. |

| W05.35 Requalify the exact pushed current tip after the streamed S3 response-framing fix | Local release qualification + hosted evidence | Full local packet green; hosted/provider/native/package closure open | 100% local / 0% current-tip hosted closure | Exact pushed `9e1879553a5db47932eb286dd1db3a772419ca25` is documentation-only over runtime fix `4f9120a2e61a260ea14af0953bcc3f7dc5cafe3e`. The focused `http_server_aborts_short_streamed_response_without_reusing_connection` regression passed `1/1`, the full locked Rust workspace passed, strict workspace Clippy passed, format and diff checks passed, optimized N-API build passed, and the complete Node SDK/CLI plus real PGlite/provider/CLI/oracle packet passed. Results: Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, 40×621 traces, and 33/33 S3 gateway tests. Same-SHA CI `35690379471`, fault `35690379495`, W04 `35690379497`, W08 targets `35690379501`, and W08 policy `35690379506` were cancelled; no current R2 run was admitted. | Select one final release SHA after shared-main movement; obtain terminal same-SHA CI, fault, W04/W07/W08, AWS, provider, Native 9P/FUSE/Windows/FSKit, package/provenance, scope, and final-audit evidence. After the UTC-month reset and security-approved R2 token rotation, run exactly one bounded live-R2 packet. | 0 h additional implementation; 1–3 h evidence reconciliation plus hosted/provider/platform wait | R2 monthly cap/token rotation, AWS security/OIDC provisioning, hosted concurrency, privileged native platforms, signing, registries, provider services, scope decisions, and final-audit ownership remain external. |

| W05.36 Requalify the exact structural-9P N-API successor | Package/API implementation + local release qualification | Complete locally; hosted/provider/native/package closure open | 100% local / 0% current-tip hosted closure | Exact current `496ed42b3cfaca4a379f7e061d24bd27b5e23372` adapts direct `P9Session` to accept structural `FsDriver` values, preserves callback error translation, and releases the adapted driver on destroy. The optimized N-API build, typecheck, direct `P9Session` lifecycle, structural-driver lifecycle, complete Node/N-API SDK/CLI suite, full locked Rust workspace, strict Clippy, format/diff checks, and real PGlite/provider/CLI/oracle packet passed. Results remain Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, 40×621 traces, and 34/34 S3 gateway tests. | Select one final settled SHA; retain terminal same-SHA CI, fault, W04/W07/W08, AWS, provider, Native 9P/FUSE/Windows/FSKit, package/provenance, scope, and final-audit evidence. Keep live R2 blocked until the UTC-month reset and security-approved token rotation. | 0 h additional implementation; 1–3 h evidence reconciliation plus hosted/provider/platform wait | Structural Node API is locally green but hosted Native 9P, platform runners, signing, registries, AWS security/OIDC, R2 cap/reset, provider services, and final audit remain external gates. |

| W05.37 Requalify the pushed HTTP framing-boundary test successor | S3 integration test coverage + current-tip release qualification | Complete locally; hosted/provider/native/package closure open | 100% local / 0% current-tip hosted closure | Exact pushed `7440a68a81d3464ded01f914aa8469fa1410b8d5` includes the current W05 ledger over the S3 framing-boundary successor `a861032f`. The full current S3 gateway suite passed `37/37`, including pipelined response ordering, `Expect: 100-continue`, transfer-encoding refusal without content length, HEAD length without a body, short streamed-response abort, multipart/CAS, interop, and lifecycle cases. The direct/structural 9P N-API, full Rust/Clippy/N-API/Node/PGlite/SDK/CLI packet remains green from the immediately preceding runtime qualification. | Select one final settled SHA; obtain terminal same-SHA hosted CI, fault, W04/W07/W08, AWS, provider, Native 9P/FUSE/Windows/FSKit, package/provenance, scope, and final-audit evidence. Keep live R2 blocked until the UTC-month reset and security-approved token rotation. | 0 h additional implementation; 1–3 h evidence reconciliation plus hosted/provider/platform wait | This delta is test coverage only; hosted cancellation/mainline movement, R2 cap/reset, AWS security/OIDC, native platforms, signing, registries, providers, scope, and final audit remain external gates. |

| W05.38 Requalify the S3 rejected-request-body drain runtime fix | S3 transport implementation + current-tip integration qualification | Complete on changed surface; hosted/provider/native/package closure open | 100% changed-surface local / 0% current-tip hosted closure | Runtime fix `d870f900` drains a rejected HTTP request body after the S3 session refuses it, preventing unread-body connection contamination. Exact pushed `2278f32b57809d8a224023636ba045683955898e` is documentation-only over that runtime; the full S3 gateway suite passed `37/37` after the fix, the optimized N-API artifact was rebuilt, and the complete Node/N-API SDK/CLI suite passed with only documented provider/native opt-in skips. The immediately preceding exact packet remains green for Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and 40×621 traces. | Select one final settled SHA; retain terminal same-SHA hosted CI, fault, W04/W07/W08, AWS, provider, Native 9P/FUSE/Windows/FSKit, package/provenance, scope, and final-audit evidence. Keep live R2 blocked until the UTC-month reset and security-approved token rotation. | 0 h additional implementation; 1–3 h changed-surface reconciliation plus hosted/provider/platform wait | This runtime delta is locally green but hosted concurrency/cancellation, R2 cap/reset, AWS security/OIDC, native platforms, signing, registries, providers, scope, and final audit remain external gates. |
| W05.39 Record terminal hosted status for the published W05 release candidate | Hosted evidence / release control | Open; external gate | 0% terminal hosted closure | Exact published `b72d02f4` has W04 policy run `35692639780` successful. Fault injection `35692639786`, W08 release policy `35692639797`, CI `35692639806`, and W08 release targets `35692639831` all completed `cancelled`; no same-SHA terminal full release packet is therefore promoted. | Select a settled release SHA and obtain non-cancelled same-SHA CI, fault, W04/W07/W08, AWS/OIDC, provider, Native 9P/FUSE/Windows/FSKit, package/provenance, scope, and final-audit evidence; preserve the R2 cap and wait for security-approved credential rotation before any live R2 retry. | 0.25–1 h evidence reconciliation; hosted/provider/platform wait separate | Mainline supersession/cancellation is an external hosted gate. R2 monthly admission is closed; security provisioning, provider services, native runners/signing, registries, scope ownership, and W20.6 remain external. |
| W05.40 Stabilize and qualify an immutable production candidate | Release engineering + hosted CI/native/provider evidence | Exact-candidate local packet green; same-SHA hosted packet terminal but failed | 100% local / 68% overall closure | Candidate branch `andymac4182/c/w05-production-candidate-20260922` is pinned to `25e275ab6d4f918be72dcd8f62a5baca6bbcd251`. Exact-candidate format/diff checks, full locked Rust workspace, strict Clippy, 40/40 S3 gateway tests, optimized N-API build, full Node/N-API suite, real PGlite/Rust/Node/CLI/oracle packet, credential-free W07/W08/AWS-control fixtures, both npm pack dry-runs, and the locked Cargo license inventory passed. Same-SHA hosted W04 `35694326676`, W08 policy `35694327019`, Native 9P `35694328753`, Fault `35694329117`, and W07 `35694328112` succeeded; W07 had both the macOS FoundationDB compile and durable FoundationDB/RustFS/Node/CLI/restart jobs green. Attestation-enabled W08 `35697156046` also succeeded for Linux and macOS, including CycloneDX SBOM attestation publication and verification. Candidate CI `35694325175` is terminal `failure`; its exact job classification is in the W05.41 row and terminal addendum below. Security request [#3](https://github.com/andymac4182/mount-rs/issues/3) is open for the protected AWS/OIDC inputs and <=$100 monthly test budget. The candidate branch remains stable against unrelated mainline pushes. | Create a new immutable candidate from the repaired `f94b53d8` mainline, rerun the complete same-SHA hosted matrix, and retain package/provenance, AWS/provider, scope, and W20.6 evidence. Do not admit R2 before the UTC-month reset and security-approved short-lived-token rotation. | 1–2 h active evidence reconciliation; 2–8 h hosted/provider/platform wait | Hosted runner capacity and workflow scheduling are external. AWS protected inputs/OIDC, R2 cap/reset, live provider services, Linux/macOS/Windows native privileges/signing, registries, product scope, and final-audit ownership remain external. |

| W05.41 Reconcile terminal candidate CI and release-artifact evidence | Hosted CI failure classification + release control | Complete for terminal classification; production gate remains open | 100% evidence classification / 68% overall closure | Same-SHA candidate CI `35694325175` is terminal `failure` at `25e275ab`. Rust/Node/NFS/WebDAV/9P/Ozone base/RustFS/FoundationDB-RustFS/HTTP-observability/aggregate-native jobs passed. TiDB `106637769088` and TiDB/RustFS `106637769305` failed the stale explicit-COMMIT failure-injection assertion; Ozone/TiDB `106637769210` and Ozone/FoundationDB `106637769419` failed the hard IOPS target with lifecycle correctness and cleanup otherwise green; W26 evidence `106648759164` failed the source-clean attestation with `dirtyEntryCount=1`; native FUSE `106637769402` was cancelled after its rootless file-operation step failed. W08 attestation-enabled run `35697156046` remains terminal-successful with repository attestation IDs `49141286` and `49141266`. | Build a new immutable candidate from shared `f94b53d8`, rerun the full CI matrix and W26 evidence after the staged N-API build/TiDB harness fixes, obtain a passing native-FUSE result or record a supported-scope exclusion, and retain terminal provider performance evidence. CI is not acceptance while any job is failed, cancelled, skipped, or non-terminal. | 1–2 h active classification/follow-up; 2–8 h hosted/provider/native wait | Ozone IOPS capacity, TiDB/Ozone/FoundationDB service startup, native runner/kernel behavior, AWS security/OIDC, R2 cap/reset, package/signing, support scope, and W20.6 approval remain external boundaries. |

| W05.42 Repair and rerun the production candidate after terminal CI failures | Implementation + hosted CI/native/provider qualification | Implementation repair pushed; immutable hosted rerun pending | 35% | Shared `origin/main` `f94b53d8` contains the TiDB ambiguous-publication proxy fix and the W26 out-of-tree N-API build with a source-clean assertion. Local `./scripts/cargo-shared test -p mount-rs-tidb --tests --locked` passed 8 unit tests; the two real-service tests remain correctly ignored without TiDB. `node benchmarks/storage/test.mjs`, workflow YAML parsing, formatting, and `git diff --check` passed. No R2 credential was read or used. | Create a new stable candidate branch from the repaired mainline; run exact-SHA local full Rust/Clippy/N-API/Node SDK/CLI/PGlite/packaging qualification; dispatch CI, Fault, W04, W07, W08, Native 9P, and attestation workflows; classify every terminal result; close hard Ozone IOPS and native-FUSE gates or record explicit support-scope exclusions; then run W20.6. | 2–6 h active engineering/release work; 4–16 h hosted/provider/platform wait | Live TiDB/Ozone/FoundationDB services, runner/kernel privileges, AWS protected OIDC inputs, R2 UTC-month reset and token rotation, package registries/signing, product support scope, and final-audit approval are external/provider gates. |
| W05.43 Qualify the repaired immutable candidate across Rust, Node, SDK, CLI, N-API, PGlite, and oracle paths | Local release qualification + release control | Complete locally; same-SHA hosted/provider/native/package closure open | 100% local / 68% overall closure | Exact candidate branch `andymac4182/c/w05-production-candidate-20260922b` at `87f3cdf0a8b3d29c89ff6c1e8d6cbd2409d0c01d` passed format/diff checks, the full locked Rust workspace, strict workspace Clippy, optimized release N-API build, the complete pinned-oracle Node/N-API suite, and `scripts/test-pglite.sh`. Node coverage passed SDK/CLI, WebDAV, S3 restart/scope, FUSE/NFS/9P differential paths, Rust-backed sessions, host restart, distribution, and artifact aggregation. The PGlite packet reports Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and 40×621 seeded traces; real PGlite reconnect/versioning/VFS/lifecycle/split-store/FUSE and backup/restore rollback passed. R2, AWS, TiDB/RustFS, FoundationDB opt-in, and privileged native mount rows remained explicit skips or security/platform gates; no secret or Keychain value was read. | Dispatch the immutable candidate through CI, Fault, W04, W07, W08, Native 9P, release-target attestation, and package/provenance workflows; retain terminal same-SHA results; rerun live R2 only after the UTC reset and security-approved token rotation; provision AWS through security/OIDC; close Ozone IOPS, native-FUSE, advertised-platform/package, support-scope, and W20.6 gates. | 0 h active implementation; 1–3 h release coordination plus 4–16 h hosted/provider/native/platform wait | Local qualification does not close hosted runner/kernel behavior, live provider services, R2 budget/reset, AWS protected inputs, registries/signing, native privileges, product scope, or final-audit ownership. |
| W05.44 Dispatch and track the same-SHA hosted release-gate packet | Hosted CI/native/provider/release evidence | In progress; Fault/W04/W08 policy/Native 9P terminal green; W07/W08 target companions still open; CI queued | 76% provisional | On immutable `87f3cdf0a8b3d29c89ff6c1e8d6cbd2409d0c01d`, dispatched CI `35702348089`, Native 9P `35702349894`, Fault injection `35702350927`, W04 policy `35702351026`, W07 FoundationDB qualification `35702352395`, W08 release targets with `attest=true` `35702352896`, and W08 release policy `35702353241`. Fault injection is terminal-successful on Windows, Ubuntu, and macOS; W04 policy, W08 release policy, and the full Native 9P workflow are terminal-successful. W07's durable FoundationDB/RustFS job is green but its macOS feature-compile companion remains queued. W08 Linux/macOS builds and macOS downloaded-asset verification are green, while Linux downloaded-asset verification remains queued and the overall run is not terminal. CI remains queued. The dispatch set intentionally excludes Live Cloudflare R2 while its monthly cap is closed, Live AWS while security issue [#3](https://github.com/andymac4182/mount-rs/issues/3) lacks protected inputs, and the production-release publisher. | Poll every run to terminal; inspect redacted logs/artifacts; classify implementation versus provider/runner/native failures; repair any actionable implementation failure on a new immutable candidate; retain terminal same-SHA package/provenance/attestation evidence; then close AWS, post-reset R2, support-scope, and W20.6 gates. | 0.5–1 h active tracking; 2–16 h hosted/provider/native wait | GitHub runner capacity, Linux/macOS kernel privileges, TiDB/Ozone/FoundationDB services, AWS security administration, R2 UTC reset/token rotation, signing/registries, and product release-scope ownership are external gates. |
| W05.45 Recheck exact-candidate package contents and workspace license inventory | Local packaging / dependency release gate | Complete locally; cross-platform publication and signing open | 100% local / package-publication closure open | On exact `87f3cdf0`, `npm pack --dry-run --ignore-scripts` passed for `@mount-rs/core` and `@mount-rs/virtual-fs`; the core packet contains the Darwin arm64 `.node` artifact, declarations, `LICENSE`, and `THIRD_PARTY_NOTICES.md`, while virtual-fs contains its declarations and notices. `./scripts/cargo-shared metadata --locked --format-version 1` reported 25/25 workspace packages with `Apache-2.0` and zero non-Apache packages. An initial pnpm-specific `pack --ignore-scripts` probe was rejected as an unsupported option and was not treated as evidence; the intended npm dry-runs were then run successfully. | Repeat package dry-runs on the selected final release SHA after any runtime change; complete Linux/Windows/macOS artifact aggregation, clean-install consumer tests, registry publication, signing/provenance, and rollback ownership evidence. | 0.5 h active local verification; 2–8 h platform/registry/signing wait | Platform toolchains, native artifact hosts, package registries, Sigstore/provenance, supported-version policy, and publication ownership are external gates. |

| W05.46 Record terminal Fault and partial W07/W08 hosted results | Hosted CI/native/provider/release evidence | Fault terminal-successful; W07/W08 and CI remain open | 82% provisional | Exact candidate `87f3cdf0` now has Fault injection `35702350927` terminal-successful across Windows (`106663114945`), Ubuntu (`106663115212`), and macOS (`106663115140`), with format, default/all-feature tests, and strict Clippy green on every runner. W07 `35702352395` has its durable FoundationDB/RustFS/Rust/Node/CLI/restart job `106663122874` terminal-successful, but the macOS FoundationDB native feature-compile job `106663122748` remains queued. W08 targets `35702352896` has Linux and macOS release builds successful (`106663124664`, `106663124371`) and macOS downloaded-asset verification successful (`106671605531`); Linux downloaded-asset verification `106671605507` remains queued, so target attestation and the overall W08 run are not terminal. CI `35702348089` remains queued. | Poll CI, W07, and W08 to terminal; inspect W07 durable logs/artifacts and W08 Linux verification plus attestation markers; classify any failures; repair implementation failures on a new immutable candidate; then close AWS security/OIDC, post-reset R2, platform/package publication, support-scope, and W20.6 gates. | 0.5–1 h active tracking; 2–16 h hosted/provider/platform wait | Hosted runner capacity and macOS/Linux native/toolchain availability are external. Ozone/TiDB/FoundationDB service performance, AWS security administration, R2 UTC-month reset/token rotation, registries/signing, and release-scope ownership remain open; no provider or package acceptance is inferred from these partial results. |

| W05.47 Record terminal W08 release-target provenance and current CI boundary | Hosted release/package/provenance evidence | W08 terminal-successful; CI and W07 workflow remain open | 86% provisional | Exact candidate `87f3cdf0` W08 targets run `35702352896` is terminal-successful. Linux build `106663124664`, macOS build `106663124371`, Linux download verification `106671605507`, macOS download verification `106671605531`, Linux attestation `106673506027`, and macOS attestation `106673505969` all passed. Downloaded artifact ZIP digests matched the uploaded digests: Linux `a5537b896b906c3cb4f57d7ffae36566b48f64728467b898fe0cc583638abaaa`; macOS `66a4d74b09cb86d00e12ddc38033ca0e72466b9ada967852d4a17c1b9735ad65`. Both targets emitted `W08_RELEASE_TARGET_ATTESTATION_PASS` for source `87f3cdf0`; repository attestation IDs are Linux provenance `49160460`, Linux SBOM `49160463`, macOS provenance `49160584`, and macOS SBOM `49160588`, with Rekor log indices `2908782598`, `2908782719`, `2908785011`, and `2908785055`. CI `35702348089` is not terminal: completed jobs currently include failures in Windows Node, W26 Ozone evidence, TiDB, TiDB/RustFS, Ozone/TiDB, Ozone/FoundationDB, Ubuntu Rust, and Ubuntu Node, while macOS Node and native NFS remain queued; no failure is classified until the parent run and redacted logs are terminally available. W07 `35702352395` remains workflow-queued around its macOS compile companion. | Wait for CI and W07 to terminate; retrieve and classify each failed CI job; repair implementation failures on a new immutable candidate, or record provider/capacity/support-scope blockers with evidence; then close AWS security/OIDC, post-reset R2, platform/package publication, scope, and W20.6. | 0.5–1 h active tracking; 2–16 h CI/provider/platform wait | GitHub runner capacity, incomplete CI logs while the parent is non-terminal, TiDB/Ozone/FoundationDB service behavior, AWS security administration, R2 cap reset/token rotation, native/platform support, registries/signing, and product-scope ownership remain external. |

| W05.48 Record terminal W07 cross-platform FoundationDB/RustFS evidence | Hosted provider/platform evidence | W07 terminal-successful; CI remains open | 90% provisional | Exact candidate `87f3cdf0` W07 run `35702352395` is terminal-successful. Durable job `106663122874`, macOS feature-compile job `106663122748`, and cross-platform assembly `106676099279` all passed. The durable packet verified rollout/evidence/config policy, optimized N-API, Linux FUSE prerequisite, durable FoundationDB metadata, RustFS chunks, service/VFS restart and reopen, Rust/Node/CLI paths, and fail-closed evidence fixtures. The bounded workload marker measured `466.18` lifecycle IOPS over 400 iterations/64 concurrency with `minimum_iops=1`; the assembled packet emitted `W07_PLATFORM_QUALIFICATION_PASS linux=terminal macos=feature-compile-only source_revision=87f3cdf0`. Linux evidence download digest was `2229862b90ca978f9a9d205ab59bb8414a4396e2d381f35380ad9df22543d5fd`, macOS evidence digest `84185d564272f3a360f9d20606b0c99af31162d685a9fb26b103843c4b13ceeb`, and the preserved aggregate artifact is `10683929336` with upload digest `082db52c7760bf8cad5420b0539dbf7f6658c42a6b0f572fbf3181690c9ac8ab`. | Inspect the terminal W07 artifact against the production support matrix; retain the explicit `feature-compile-only` macOS boundary and do not promote the bounded W07 IOPS result to the separate W26 hard `1000` IOPS gate; wait for CI terminal classification, then close AWS/R2/provider/platform/package/scope/W20.6 gates. | 0.5–1 h active evidence review; 2–16 h CI/provider/platform wait | W07 does not prove live macOS service/cluster/mount, production capacity, identity/ACL, backup/restore, failover, observability, signing, or owner evidence. CI’s failed provider-composition jobs, AWS security administration, R2 cap reset/token rotation, registries, and support scope remain external gates. |

| W05.49 Repair terminal candidate CI failures and preserve hard provider boundaries | Implementation + hosted CI/provider qualification | Terminal candidate failure classified; Rust test repair compile-checked; rerun pending | 18% of this repair chunk / 72% provisional overall closure | Candidate CI `35702348089` at exact source `87f3cdf0` is terminal `failure`. Ubuntu Rust job `106663105271` timed out `destroy_overrides_late_driver_error_for_inflight_call` at `session.rs:2023`; the test helper published its entered signal before registering the `Notify` release waiter. The repair registers/enables the waiter first, and `./scripts/cargo-shared check -p mount-rs-9p --tests --locked` passes; a local executable test remains blocked by the host Xcode license gate, so no local runtime pass is claimed. Ubuntu Node job `106663105160` timed out in `9P server boundary` at `+19842ms`; Windows Node job `106663105335` timed out in the pinned-oracle parity phase. TiDB jobs `106663105037` and `106663105174` returned `1` from the publication instead of exercising the intended unknown-outcome assertion, showing that the proxy does not yet intercept the prepared autocommit statement path. Ozone/TiDB measured successful lifecycle IOPS `362.85`; Ozone compositions measured `445.24` for one provider and `1720.01` for the other, so the hard `1000` target remains correctly failed and must not be lowered. W26 evidence consequently emitted `W26_OZONE_EVIDENCE_PACKET_FAIL` because the required composition marker was absent. | Finish the TiDB prepared-statement failure injector; reproduce/fix or classify the Node/Windows 9P timeout; run Linux Rust/Node focused and full local gates where the host permits; create a new immutable candidate without mutating `87f3cdf0`; dispatch one complete same-SHA CI/Fault/W04/W07/W08/Native 9P/attestation packet; retain the Ozone capacity failure as a provider/platform blocker unless the service meets `1000` without weakening the gate; then close AWS security/OIDC, post-reset R2, native-FUSE, package/publication, advertised support scope, and W20.6. | 2–6 h active repair and qualification; 4–16 h hosted/provider/native wait | Xcode license approval blocks executable Rust tests on this macOS host. TiDB/Ozone service behavior and capacity, GitHub runner/kernel timing, AWS protected OIDC provisioning, R2 UTC-month reset/token rotation, native-FUSE privileges, signing/registries, product scope, and final-audit ownership remain external. |

| W05.50 Repair the prepared TiDB failure injector and harden the Node 9P boundary cleanup | Integration-test implementation + local hosted-reproduction gate | Complete locally for this repair slice; hosted requalification pending | 100% repair slice / 76% provisional overall closure | The TiDB ambiguity proxy now recognizes `COM_STMT_PREPARE`, records the statement ID from `COM_STMT_PREPARE_OK`, and drops the response to the matching `COM_STMT_EXECUTE`; a focused packet-classification test covers the prepared path. The Node 9P port-conflict test now only closes the loser when it actually bound, avoiding a potentially wedged close after `EADDRINUSE`. `./scripts/cargo-shared check -p mount-rs-tidb --test ambiguous_commit --locked`, Rust formatting, `git diff --check`, and `node --check integrations/mount-rs-napi/test/servers.mjs` pass. `MOUNT_RS_SERVER_PHASE=p9 node test/servers.mjs` passed four consecutive local loopback runs. No live TiDB service or executable Rust integration test was claimed locally. | Create a new immutable candidate from `be98aca9` plus this slice; run the live TiDB/TiDB-RustFS ambiguous-outcome jobs, Linux/Windows Node parity and full Rust/Node suites, and classify any remaining 9P timeout; then close Ozone capacity, native-FUSE, AWS/OIDC, post-reset R2, package/publication, support-scope, and W20.6 gates. | 0 h remaining for this local slice; 2–6 h active candidate/hosted triage plus 4–16 h provider/native wait | Live TiDB/TiDB-RustFS services, GitHub Linux/Windows runner timing, Ozone capacity, Xcode license for local executable Rust tests, AWS protected OIDC, R2 cap reset/token rotation, native-FUSE privileges, registries/signing, product scope, and final-audit ownership remain external. |
| W05.51 Make macOS N-API artifacts dyld-safe and complete the exact pushed Rust/Node/PGlite matrix | Package/platform implementation + local release qualification | Complete for the local implementation slice on exact pushed `515bdc00`; hosted/provider/native/publication closure remains open | 100% local slice / 78% provisional overall closure | `integrations/mount-rs-napi/scripts/build-native.mjs` now detects Darwin with Rust `<1.98` and supplies `MACOSX_DEPLOYMENT_TARGET=11.0` plus the linker platform-version floor, preserving existing `RUSTFLAGS`; this works around the macOS 27 dyld rejection of Rust 1.95/LLVM22 artifacts with misaligned `LC_SYMTAB.stroff` (upstream context: [Rust #157750](https://github.com/rust-lang/rust/issues/157750)). On exact pushed `515bdc00792a62403b0ee7b94e904434d067f43b`, the optimized package build produced a Darwin addon with aligned `stroff=22058472`, `minos=11.0`, `sdk=26.0`, and `node -e require(...)` loaded the real addon. Format, the full locked Rust workspace, strict Clippy, the complete pinned-oracle Node/N-API suite, PGlite lifecycle/N-API/FUSE checks, Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, and the provider-matrix Rust `6 pass / 3 skip / 0 fail` plus Node `5/3/0` and CLI `12/2` packets all passed. The standalone provider lock was refreshed by one checked-in `md-5` dependency entry so the harness executes under `--locked`. No R2/AWS secret or Keychain item was read. | Create a new immutable candidate from the current settled `origin/main` (do not mutate the old `87f3cdf0`/`25e275ab` candidates); rerun exact-SHA Rust/Node/SDK/CLI/PGlite qualification after the concurrent mainline changes; obtain terminal same-SHA CI, Fault, W04, W07, W08/attestation, Native 9P/FUSE, and package/provenance results; close Ozone hard `1000` IOPS and native-FUSE or record approved support-scope exclusions; provision AWS through security/OIDC, run one post-reset bounded R2 packet within the `$100` envelope, close package publication and advertised-provider scope, and issue W20.6 GO/NO-GO. | 1.5–3 h active implementation/qualification; 4–16 h hosted/provider/native/publication wait | Current mainline moved during qualification; GitHub runner and Windows/macOS/Linux native behavior, TiDB/Ozone/FoundationDB services, AWS protected inputs, R2 reset/token rotation, package registries/signing, support scope, and final-audit ownership remain external. |
| W05.52 Qualify an immutable current-main candidate across local and hosted release gates | Release engineering + local Rust/Node qualification + hosted CI coordination | Local packet complete on `7efded54`; seven same-SHA hosted workflows queued; provider/native/publication gates open | 100% local / 10% hosted provisional | Candidate branch `andymac4182/c/w05-production-candidate-20260922c` is pinned to `7efded5424f8ad6c2335e7be1cde98c22b9315fe`. Local format, full locked Rust workspace, strict Clippy, optimized dyld-safe Darwin addon (`stroff % 8 = 0`, minos 11.0, SDK 26.0, Node load), full Node/N-API suite, PGlite lifecycle/provider/CLI matrix, Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, and explicit R2/native skips all pass. Same-SHA runs: CI `35714144497`, Fault `35714146067`, W04 `35714141926`, W07 `35714147247`, W08 policy `35714144646`, W08 targets with `attest=true` `35714146811`, and Native 9P `35714145176`; all were confirmed queued at the capture boundary with matching head SHA. | Poll every run to terminal and retain artifacts; repair any actionable implementation failure on a new immutable candidate; then close Ozone hard `1000` IOPS, native-FUSE/mount scope, AWS/OIDC, post-reset R2, package publication/provenance, advertised support, and W20.6. Never promote queued/partial/cancelled/old-SHA evidence. | 0.5–1.5 h active tracking; 2–16 h hosted/provider/native/publication wait | GitHub runner backlog, live TiDB/Ozone/FoundationDB/R2 services, protected AWS inputs, native kernel/FSKit privileges, signing/registries, scope ownership, and final audit remain external. |
| W05.53 Track terminal same-SHA hosted results and classify the first artifact-service failure | Hosted CI evidence + release engineering | Active; functional candidate jobs are green where complete, but the packet is non-terminal | 100% local / 24% hosted provisional | At 2026-09-22 10:31 UTC, exact candidate `7efded54` remains the source for every result. W04 policy `35714141926` is terminal-successful. CI `35714144497` is overall queued: its TiDB TLS compile and Ubuntu ARM Node jobs passed; macOS Intel job `106701528015` failed only at `actions/upload-artifact` after its Node SDK/CLI/N-API, parity, PGlite, benchmark, structural macOS mount, and upstream conformance steps passed. Fault `35714146067` remains queued with Windows and Ubuntu success and macOS pending. W07 `35714147247` is in progress with macOS FoundationDB feature compilation successful and the durable FoundationDB/RustFS/Node/CLI/restart job still running. W08 policy `35714144646` is terminal-successful; W08 targets `35714146811` is in progress with the Linux release build successful and macOS build pending. Native 9P `35714145176` remains queued: native-9P, N-API lifecycle, and pinned conformance jobs passed, while the root-gated companion is queued. No queued, partial, failed-upload, or non-terminal workflow is promoted to release acceptance. | Let parent workflows reach terminal state and retrieve the artifact-service log; if the macOS failure is hosted artifact infrastructure, rerun or retain a clean artifact upload on the same candidate, and if any product step fails, create a new immutable candidate. Then close Ozone hard `1000` IOPS, native-FUSE or approved support scope, AWS/OIDC, post-reset R2, package publication/provenance, advertised support, and W20.6. | 0.5–1.5 h active tracking and classification; 2–16 h hosted/provider/native/publication wait | GitHub runner/artifact service and root-gated native capacity are external. R2 remains closed by the monthly envelope; AWS protected inputs remain absent; no credential or Keychain value was read. |
| W05.54 Record terminal Fault success and partial W07/W08/native release evidence | Hosted fault/platform/package evidence | Fault terminal-successful; remaining same-SHA packet non-terminal | 100% local / 39% hosted provisional | Fault injection `35714146067` is terminal-successful with Windows job `106701532629`, Ubuntu job `106701532753`, and macOS job `106701533083` all passed. W07 `35714147247` has macOS FoundationDB feature compilation and durable FoundationDB/RustFS/Node/CLI/restart successful, but cross-platform assembly `106709151497` is queued. W08 targets `35714146811` has both Linux job `106701533824` and macOS job `106701533638` successful, but both downloaded-asset verification jobs remain queued. Native 9P `35714145176` has native, N-API, and pinned conformance success; root-gated job `106706609031` remains queued. Candidate CI `35714144497` remains queued with completed `foundationdb-rustfs`, `rustfs`, TiDB TLS compile, and Ubuntu ARM Node success, macOS Intel artifact-upload failure, and the remaining provider/native/Windows jobs queued or in progress. No partial packet is promoted to release acceptance. | Complete W07 assembly, W08 asset/provenance verification, Native 9P root conformance, and CI terminal classification; retain exact artifacts and repair any product failure on a new immutable candidate. Then close Ozone hard `1000` IOPS, native-FUSE or support scope, AWS/OIDC, post-reset R2, package publication, advertised support, and W20.6. | 0.5–1.5 h active tracking; 2–16 h hosted/provider/native/publication wait | GitHub runner queue, artifact service, root privileges, security administration, provider services, R2 reset, registries/signing, and final scope/audit remain external. |
| W05.55 Classify the macOS native-artifact upload timeout and preserve exact hosted evidence | Hosted artifact-service diagnosis + release engineering | Failure classified as hosted artifact-service timeout; parent CI non-terminal | 100% local / 45% hosted provisional | The completed macOS job log for CI `35714144497`, job `106701528015`, shows every functional step successful, including the full Node/N-API suite, parity, PGlite, benchmark, structural macOS mount, and upstream conformance. The storage benchmark artifact finalized as ID `10689034619`; the native addon upload sent `10,536,521` bytes and computed digest `31c94fa8043eac34289b4ef8a4d310256195f963c195c1b994ae9c7b9d7c7f58`, then failed only at `Finalizing artifact upload` with `Failed to FinalizeArtifact: Unable to make request: ETIMEDOUT`. The parent remains queued at exact SHA `7efded5424f8ad6c2335e7be1cde98c22b9315fe`; this is not a product-test failure or a release acceptance. | Let the parent run reach terminal state, then use the supported failed-job rerun or retain the already-uploaded functional evidence if the artifact service recovers; do not create a new code candidate for this infrastructure-only timeout. Continue W07 assembly, W08 asset verification, Native 9P root conformance, CI provider jobs, and the remaining AWS/R2/Ozone/native/package/scope/W20.6 gates. | 0.5–1 h active diagnosis; 2–16 h hosted artifact/runner wait | GitHub artifact-service/network availability is external. No credential or Keychain value was read; R2 and AWS remain explicitly gated. |
| W05.56 Record terminal Native 9P lifecycle and root-conformance acceptance | Hosted native/platform evidence | Terminal-successful same-SHA native 9P gate; broader release packet remains open | 100% local / 55% hosted provisional | Native 9P run `35714145176` is terminal-successful at exact candidate SHA `7efded5424f8ad6c2335e7be1cde98c22b9315fe`. Native job `106701527815`, N-API lifecycle job `106701528172`, pinned conformance job `106701528173`, and root conformance job `106706609031` all passed. The root conformance log reports `1` test file and `146` tests passed. The root job emitted a post-job Rust-cache save permission warning while preserving the successful test result; no cache artifact is promoted as product evidence. | Retain the terminal Native 9P result, complete W07 assembly and W08 asset verification, let CI reach terminal state, and close the remaining Ozone, AWS/OIDC, post-reset R2, native-FUSE, package/provenance, advertised-scope, and W20.6 gates. | 0.25–0.75 h active evidence review; 2–16 h hosted/provider/native/publication wait | Root runner cache permissions are a hosted hygiene warning; Linux native 9P acceptance is green, but other native platforms and final product scope remain separate gates. |
| W05.57 Classify the same-SHA Ozone/TiDB hard-IOPS failure | Hosted provider-capacity gate | Terminal provider-capacity failure; implementation tests and cleanup are green | 100% local / 42% hosted provisional | CI `35714144497`, Ozone/TiDB job `106701527631`, exact candidate SHA `7efded5424f8ad6c2335e7be1cde98c22b9315fe`: the durable composition completed `1200/1200` successful write/read/delete lifecycles with `timeoutCount=0` and `cleanupFailureCount=0`, but the required `minIops=1000` gate measured `84.01050956100686` and emitted `IOPS_TARGET_NOT_MET`. The job emitted `RUSTFS_COMBO_FAIL` and correctly failed closed; evidence artifact `10689997396` finalized. This is a provider capacity gate, not evidence to lower the production floor or a product-test assertion failure. | Wait for Ozone/FoundationDB and remaining CI jobs to classify; retain the hard `1000` target, determine whether the advertised Ozone surface can meet it or must be excluded, and only open implementation work if a reproducible product bottleneck—not hosted provider capacity—is demonstrated. Continue W07/W08/CI/provider/security/package/scope/W20.6 closure. | 0.5–2 h active evidence/decision work; 2–16 h hosted provider wait | Apache Ozone service capacity and hosted runner conditions are external; AWS/OIDC, post-reset R2, native-FUSE, package publication and support scope remain open. |
| W05.58 Classify the same-SHA Ozone/FoundationDB hard-IOPS failure | Hosted provider-capacity gate | Terminal provider-capacity failure; implementation tests and cleanup are green | 100% local / 44% hosted provisional | CI `35714144497`, Ozone/FoundationDB job `106701528236`, exact candidate SHA `7efded5424f8ad6c2335e7be1cde98c22b9315fe`: the durable composition completed `1200/1200` successful operations across `400` lifecycle iterations with `timeoutCount=0` and `cleanupFailureCount=0`, but the required `minIops=1000` gate measured `298.6107927173534` and emitted `IOPS_TARGET_NOT_MET`; the job then emitted `RUSTFS_COMBO_FAIL` and `OZONE_CLEANUP_PASS`. This is the second independent same-SHA Ozone capacity miss and does not justify lowering the production floor. | Keep Ozone excluded from production acceptance unless the hosted provider can meet `1000` IOPS or an approved scope decision changes the requirement; finish the remaining CI/provider/native/package/security/R2/scope/W20.6 gates and rerun only on a new immutable candidate if implementation evidence—not provider capacity—requires it. | 0.5–2 h active evidence/decision work; 2–16 h hosted provider wait | Apache Ozone/FoundationDB service capacity and hosted runner conditions are external; AWS/OIDC, post-reset R2, native-FUSE, package publication and support scope remain open. |
| W05.59 Record terminal W07, native-FUSE, cross-platform Rust/NFS, and W08 asset-verification sub-gates | Hosted provider/platform/native/package evidence | W07, native FUSE, Rust macOS/Windows, NFS macOS, and W08 downloaded-asset checks passed; release packet remains open | 100% local / 68% hosted provisional | Exact candidate `7efded5424f8ad6c2335e7be1cde98c22b9315fe`: W07 `35714147247` is terminal-successful with assembly job `106709151497`; it emitted `W07_PLATFORM_QUALIFICATION_PASS linux=terminal macos=feature-compile-only provenance=bound`, and retained artifacts `10690300620` (durable evidence) and `10690995180` (cross-platform assembly, digest `a342f32cbd7c8b6fc2336da202fa0c19a8032f4dd5fe0a631f5197d5b57d7953`). W08 `35714146811` build jobs `106701533638`/`106701533824` and downloaded-asset jobs `106708898505`/`106708898619` passed; Linux and macOS manifests/SBOM checks reported 288 components and source commit `7efded54`, with package digests `b93450472c0ed1d2bd9c5e06998e60d65071e4ac1af3050ebd07d8b650130db4` and `a6fab3b9374ebcc5e150d71c6493f2005dec60cb7ee90892045dee68c6dda67b`. W08 attestations remain queued. CI `35714144497` native-FUSE job `106701527830`, Rust macOS `106701527825`, Rust Windows `106701527935`, and native NFS macOS `106701527843` passed; native-FUSE logs show injected panic recovery and all exercised tests passing. | Wait for W08 attestation and the remaining CI jobs to reach terminal state; retain exact artifacts and classify any failure. Then close Ozone capacity or approved scope, AWS/OIDC, post-reset R2, package publication/signing, advertised support, and W20.6. | 0.75–1.5 h active evidence reconciliation; 2–16 h hosted/provider/publication wait | W08 attestation queue, CI runner capacity, Ozone service capacity, AWS security administration, R2 UTC-month reset/token rotation, native platform scope, registries/signing, and final audit remain external. |
| W05.60 Requalify the latest shared code after NFS, FoundationDB, and W08 succession | Current-tip local release qualification + candidate preparation | Current shared code requalified locally; no new same-SHA hosted packet yet | 100% local / 0% current-tip hosted provisional | Current `origin/main` is `2bb846247aca02e9ce47a80a808314ca4beeed29`; its intervening ancestry after tested code `65776be8` is documentation/lockfile-only, with no code delta. On the code-equivalent snapshot, focused NFS (`42` unit tests, v4 wire `23`, process/restart/concurrency/lifecycle suites) and FoundationDB locked tests passed; full locked Rust workspace, strict Clippy, formatting and diff checks passed; optimized Darwin N-API build/load reported `253` exports, `stroff=22007680`, `stroff % 8 = 0`, minimum macOS `11.0`, SDK `26.0`; complete Node/N-API/oracle suite passed; and `scripts/test-pglite.sh` passed Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, with only explicit live R2/TiDB/RustFS skips. | Freeze `2bb84624` as a new immutable candidate, rerun the same-SHA hosted CI/Fault/W04/W07/W08 policy/targets/Native 9P packet, retain terminal artifacts and classify failures, then close post-reset R2, security-provisioned AWS/OIDC, Ozone capacity or approved scope, package publication/signing, advertised support, and W20.6. | 1.5–3 h active candidate/qualification work; 4–16 h hosted/provider/native/publication wait | Concurrent mainline movement, GitHub queues, R2 UTC-month cap and token rotation, AWS security administration, Ozone capacity, native privileges, registries/signing, support scope, and final audit remain external. |
| W05.61 Freeze the current-tip candidate and dispatch the same-SHA hosted packet | Release engineering + hosted qualification coordination | Candidate `d1a81ad4` published; seven non-R2/AWS workflows dispatched and pending | 100% local / 5% hosted provisional | Immutable branch `andymac4182/c/w05-production-candidate-20260922d` resolves to `d1a81ad45158dc80647b723f175c01a98b9ed458`. Fresh runs are CI `35721887870`, Fault `35721895670`, W04 policy `35721891003`, W07 FoundationDB `35721892642`, W08 policy `35721891447`, W08 release targets with `attest=true` `35721892593`, and Native 9P `35721891380`. The candidate inherits the current-tip local packet recorded in W05.60. Live R2 was not dispatched because the monthly cap remains closed; AWS was not dispatched because protected security/OIDC inputs remain absent. | Poll all seven runs to terminal, retain exact artifacts and source bindings, repair any actionable implementation failure on a new immutable candidate, then run one post-reset bounded R2 packet and one security-approved AWS packet within their cost controls; close Ozone/native/package/support-scope/W20.6. | 0.5–1.5 h active dispatch/tracking; 4–16 h hosted/provider/native/publication wait | GitHub runner queues, R2 cap/reset/token rotation, AWS security provisioning, Ozone capacity, native privileges, registries/signing, support scope, and final audit remain external. |

### W05.40 exact candidate evidence (2026-09-22 17:09 AEST)

The immutable candidate `25e275ab` is the current release-control anchor. Exact
local checks passed: formatting, `git diff --check`, the full locked Rust
workspace, strict workspace Clippy, 40/40 S3 gateway tests, optimized N-API
build, and the complete pinned-oracle Node/N-API suite. The exact
`scripts/test-pglite.sh` packet passed PGlite reconnect/versioning/VFS/
lifecycle/split-store/FUSE coverage, Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI
`12/2`, upstream `1200 passed / 82 skipped`, and 40 trace combinations at
621 operations each. The oracle was verified at
`85361a8212ff9bff8e69f62fa8993ef2c2ec51e8`.

The candidate hosted packet now has W04 policy `35694326676`, W08 policy
`35694327019`, Native 9P `35694328753`, Fault injection `35694329117`, W07
`35694328112`, and attestation-enabled W08 release targets `35697156046`
terminal-successful. W08 attestation verification emitted
`W08_RELEASE_TARGET_ATTESTATION_PASS` for both `x86_64-unknown-linux-gnu` and
`aarch64-apple-darwin`; the corresponding repository attestation records are
`49141286` and `49141266`. Candidate push Fault injection `35694307087` is a
secondary same-SHA result and also passed all three operating systems. CI
`35694325175` remains non-terminal; its four provider failures and W26 Ozone
evidence failure are not promoted or classified until the workflow completes.
No hosted result is promoted to production acceptance until CI is terminal and
its artifacts are reviewed. The R2 admission remains fail-closed, and the AWS
audit remains blocked on security-provisioned protected inputs; neither
provider boundary is inferred from local or RustFS evidence.

The focused security diff scan
`7b383f24-5724-43a6-bbf6-8bbf22c1947c` completed with zero reportable findings
across the five changed workflow files, the TiDB autocommit change, the 9P
teardown change, and the tracker documentation. It confirmed manual workflow
isolation, read-only workflow permissions, fail-closed provider/error
handling, and absence of credential material. It did not and could not
validate live AWS IAM/OIDC trust or provider budget configuration; those
remain external gates.
The R2 admission remains fail-closed, and the AWS audit remains blocked on
security-provisioned protected inputs; neither provider boundary is inferred
from local or RustFS evidence.

### W05.41 terminal candidate evidence (2026-09-22 17:35 AEST)

The candidate CI workflow `35694325175` is terminal at
`25e275ab6d4f918be72dcd8f62a5baca6bbcd251` with overall `failure`. The
redacted terminal log classifications are:

| Hosted job | Result | Evidence classification | Required next action |
| --- | --- | --- | --- |
| `tidb` `106637769088` | Failed | `ambiguous_commit.rs:263` expected an unknown outcome but received success; the production autocommit publication was not intercepted by the old explicit-COMMIT proxy | Rerun on a candidate containing `f94b53d8`'s publication-ack proxy fix against real TiDB |
| `tidb-rustfs` `106637769305` | Failed | Same stale failure-injection assertion; RustFS block composition itself reached the assertion | Rerun the corrected harness; do not classify this old result as a provider durability failure |
| `ozone-tidb` `106637769210` | Failed | All 400 iterations/1,200 lifecycle operations and cleanup passed, but measured successful lifecycle IOPS was `389.4352211421599`, below the hard `1000` target | Rerun after the candidate repair and retain the hard threshold; investigate hosted capacity if still below target |
| `ozone-foundationdb` `106637769419` | Failed | All 400 iterations/1,200 lifecycle operations and cleanup passed, but measured successful lifecycle IOPS was `83.8176176750703`, below the hard `1000` target | Rerun after the candidate repair and retain the hard threshold; investigate hosted capacity if still below target |
| `w26-ozone-evidence` `106648759164` | Failed | `W26_OZONE_EVIDENCE_PACKET_FAIL reason=ozone-compositions-artifact-source-checkout-dirty`; artifact source metadata had `dirtyEntryCount=1` | Verify the staged out-of-tree addon repair on a fresh candidate; the verifier remains fail-closed |
| `native-fuse` `106637769402` | Cancelled | Actual rootless kernel file-operation step failed and GitHub's `Complete job` step hung; no final log was retained | Obtain a fresh terminal native-FUSE result or make an explicit supported-scope decision |

The same workflow passed the Rust matrix on all three operating systems,
Node jobs including macOS latest/Intel, Ubuntu, Ubuntu arm, and Windows,
native NFS/WebDAV/9P, HTTP observability, RustFS, FoundationDB/RustFS, Ozone
base/compositions, and aggregate-native. These successes do not override the
failed/cancelled release gates. The repair commit `f94b53d8` is already on
`origin/main`; the next production candidate must be created from that exact
repaired mainline and kept immutable while the hosted matrix runs.

### W05.43 exact local qualification evidence (2026-09-22 17:53 AEST)

The new immutable candidate at `87f3cdf0` is locally complete. The Rust
packet used the repository's shared Cargo target and passed the locked
workspace test suite and strict Clippy; formatting and `git diff --check`
were also green. The optimized N-API build completed without modifying the
source checkout. The full Node/N-API suite then passed its typecheck,
factory/lifecycle, WebDAV, S3, FUSE/9P/NFS, codec differential, CLI,
unstorage, host-restart, distribution, and artifact-aggregation phases.

The PGlite harness completed all local service and matrix phases. Its compact
acceptance packet was:

| Surface | Result | Boundary |
| --- | --- | --- |
| Rust SDK | `6 pass / 3 skip / 0 fail` | Three R2 rows skipped because protected credentials were absent |
| Node SDK | `5 pass / 3 skip / 0 fail` | R2 and TiDB/RustFS rows skipped because protected services/credentials were absent |
| Rust and Node CLI | `12 pass / 2 skip / 0 fail` | Live PGlite+R2 runtime remained an explicit provider gate |
| Upstream compatibility | `1200 passed / 82 skipped` | Skips are oracle-classified unsupported capabilities |
| Seeded trace parity | `40/40 pass`, `621` operations each | Five seeds × eight local backends, oracle `85361a8212ff9bff8e69f62fa8993ef2c2ec51e8` |

This closes the candidate's local implementation and package evidence only.
The hosted candidate rerun, AWS security/OIDC inputs, post-reset live R2,
Ozone hard IOPS, native FUSE, platform/package publication, support scope,
and W20.6 decision remain open and are tracked as separate hosted/native/
provider/release gates.

### W05.44 hosted dispatch boundary (2026-09-22 17:59 AEST)

The locally complete candidate `87f3cdf0` is now being held immutable while
the non-R2 release packet runs. The primary dispatch IDs are:

| Workflow | Run | Initial state | Scope |
| --- | ---: | --- | --- |
| CI | `35702348089` | queued | Rust/Node/provider-composition and W26 evidence packet |
| Native 9P | `35702349894` | terminal success | Linux native 9P probe, addon, Rust and mounted-I/O lifecycle |
| Fault injection | `35702350927` | terminal success | Cross-platform failure and cleanup matrix; Windows, Ubuntu, and macOS jobs green |
| W04 production policy | `35702351026` | terminal success | PGlite policy and release-control checks |
| W07 FoundationDB production qualification | `35702352395` | workflow queued; durable job success | FoundationDB/RustFS durable provider packet green; macOS native feature-compile companion queued |
| W08 release targets (`attest=true`) | `35702352896` | workflow queued; builds/macOS verification success | Linux/macOS CLI artifacts green; Linux downloaded-asset verification and attestation remain queued |
| W08 release policy | `35702353241` | terminal success | Release manifest, SBOM, rollout and policy checks |

The separate push-triggered CI run `35700818889` and successful push-triggered
Fault/W04 runs are retained as same-SHA secondary observations; the manual
dispatches above are the primary release-control records because their
concurrency groups are isolated from later mainline movement. Live R2 was not
dispatched, AWS was not dispatched while security issue #3 remains unprovisioned,
and the production-release publisher was deliberately held until the required
acceptance packet is green. No hosted result is promoted until its run is
terminal and its artifacts/logs are reviewed.

At 18:06 AEST the candidate's seven primary runs were still queued, with no
repository-wide Actions run in progress. GitHub's public status endpoint
reported Actions operational, so this is recorded as hosted runner/queue
capacity rather than a service outage. The older duplicate push-triggered CI
run `35700818889` was requested for cancellation at 18:07 AEST; the API still
reported it queued at the 18:08 capture, so the cancellation is not promoted as
terminal until GitHub confirms it.

At 18:10 AEST the repository-wide queue contained 21 queued runs and zero
in-progress runs; the oldest queued run was CI `35700399264` created at
07:35:51 UTC on `f94b53d8`. This confirms a shared hosted-capacity backlog,
not a candidate-specific missing workflow or input. A queued Live R2 run from
another workstream is not touched or treated as W05 provider evidence.

At 18:16 AEST the same-SHA W04 policy run `35702351026` completed
successfully. W08 release policy `35702353241` is in progress; CI, Fault,
W07 FoundationDB, Native 9P, and W08 release targets remain queued. This
partial result does not close the hosted release packet.

At 18:19 AEST W08 release policy `35702353241` also completed successfully
on `87f3cdf0`, including rollout-ledger/evidence policy, release identity,
manifest, SBOM, and provenance checks. The remaining hosted packet is still
open because CI, Fault, W07, Native 9P, and attestation-enabled release
targets are not yet terminal.

At 18:29 AEST Native 9P `35702349894` completed successfully on `87f3cdf0`.
The terminal packet verified the Linux `9p`/`9pnet_fd` kernel client, N-API
server and direct-session lifecycle, connection identity/member surfaces,
automatic mounted I/O and cleanup, direct `./9p` mounted I/O and cleanup, and
structural-driver mounted I/O with callback reachability. The companion Rust
job also passed its privileged native I/O/lifecycle tests. At 18:32 AEST Fault
injection `35702350927` completed successfully on all three operating systems.
W07's durable job `106663122874` is green but its workflow remains open for the
queued macOS compile `106663122748`. W08's Linux/macOS builds and macOS
download verification are green; Linux download verification `106671605507`
and the attestation stage remain queued. CI `35702348089` remains queued.

### W05.45 exact package evidence (2026-09-22 18:03 AEST)

The exact candidate's package surface was rechecked without running lifecycle
scripts. `npm pack --dry-run --ignore-scripts` passed for both public Node
packages. The `@mount-rs/core` dry-run listed the Darwin arm64 native addon,
generated declarations, JavaScript facades, `LICENSE`, and
`THIRD_PARTY_NOTICES.md`; the `@mount-rs/virtual-fs` dry-run listed its
declarations, sources, `LICENSE`, and notices. Locked Cargo metadata contained
25 workspace packages, all with `Apache-2.0` licenses and no non-Apache
workspace package. A preliminary pnpm command used an unsupported
`--ignore-scripts` option; it produced no release evidence and was replaced by
the successful npm commands above. Cross-platform artifact aggregation,
registry publication, signing/provenance, and clean-install consumer evidence
remain hosted/platform gates.


### W05.46 hosted terminal and partial boundary (2026-09-22 18:34 AEST)

The exact immutable candidate `87f3cdf0` now has a terminal Fault injection
PASS: run `35702350927` completed successfully on Windows, Ubuntu, and macOS.
The three jobs (`106663114945`, `106663115212`, and `106663115140`) each passed
format, the default and all-feature fault-injection tests, and strict Clippy.
This closes the cross-platform fault-injection row for this candidate; it does
not close the remaining CI/provider-composition or release-publication rows.

W07 run `35702352395` has a terminal-successful durable qualification job
`106663122874`, covering the rollout/evidence policy, optimized N-API build,
Linux FUSE prerequisite, durable FoundationDB metadata, RustFS chunks,
restart, bounded workload artifact, and qualification markers. Its macOS
FoundationDB native feature-compile companion `106663122748` is still queued,
so W07 remains non-terminal at the workflow level.

W08 release-target run `35702352896` has successful Linux and macOS release
build jobs (`106663124664` and `106663124371`) and successful macOS downloaded-
asset verification (`106671605531`). Linux downloaded-asset verification
`106671605507` remains queued; target attestation and the overall W08 run are
therefore not yet terminal. Candidate CI `35702348089` remains queued with no
job evidence. The exact run URLs and job IDs are retained in the W05.44/W05.46
register rows; no queued result is promoted to production acceptance.

The production decision remains **NO-GO**. Required next actions are to let
CI, W07, and W08 reach terminal state and inspect their redacted logs and
artifacts; repair any actionable implementation failure on a new immutable
candidate; then provision AWS through security issue [#3](https://github.com/andymac4182/mount-rs/issues/3), run one post-reset bounded R2
requalification after token rotation, complete cross-platform package/signing
and clean-install evidence, resolve support scope, and run W20.6.

### W05.47 terminal W08 provenance boundary (2026-09-22 18:40 AEST)

The exact immutable candidate `87f3cdf0` now has a terminal-successful W08
release-target workflow `35702352896`. Linux build `106663124664`, macOS
build `106663124371`, Linux downloaded-asset verification `106671605507`,
macOS downloaded-asset verification `106671605531`, Linux attestation
`106673506027`, and macOS attestation `106673505969` all passed.

The uploaded and downloaded artifact ZIP digests matched: Linux
`a5537b896b906c3cb4f57d7ffae36566b48f64728467b898fe0cc583638abaaa` and macOS
`66a4d74b09cb86d00e12ddc38033ca0e72466b9ada967852d4a17c1b9735ad65`. Both
targets emitted `W08_RELEASE_TARGET_ATTESTATION_PASS` for source
`87f3cdf0a8b3d29c89ff6c1e8d6cbd2409d0c01d`. Repository attestation IDs are
Linux provenance `49160460`, Linux CycloneDX SBOM `49160463`, macOS
provenance `49160584`, and macOS CycloneDX SBOM `49160588`; the corresponding
Rekor log indices are `2908782598`, `2908782719`, `2908785011`, and
`2908785055`.

CI `35702348089` has started but is not terminal. The current job snapshot has
failures in Windows Node, W26 Ozone evidence, TiDB, TiDB/RustFS, Ozone/TiDB,
Ozone/FoundationDB, Ubuntu Rust, and Ubuntu Node; macOS Node and native NFS
remain queued. These are diagnostic observations only: the parent run remains
non-terminal and its redacted logs are not yet available for final
classification. W07 `35702352395` is likewise still workflow-queued around
its macOS FoundationDB feature-compile companion. Production remains
**NO-GO**.

### W05.48 terminal W07 provider/platform boundary (2026-09-22 18:45 AEST)

W07 run `35702352395` is terminal-successful on exact source
`87f3cdf0a8b3d29c89ff6c1e8d6cbd2409d0c01d`. Durable qualification job
`106663122874`, macOS FoundationDB feature compilation
`106663122748`, and cross-platform evidence assembly `106676099279` all
passed. The durable job's packet covered rollout/evidence/config policy,
optimized N-API, the Linux FUSE prerequisite, FoundationDB metadata, RustFS
chunks, service/VFS restart and reopen, Rust/Node/CLI paths, and fail-closed
evidence fixtures. The bounded workload marker reported 400 iterations at 64
concurrency, `466.18` lifecycle IOPS, and `minimum_iops=1`; this is not the
separate W26 hard `1000` IOPS gate.

The assembled packet emitted
`W07_PLATFORM_QUALIFICATION_PASS linux=terminal macos=feature-compile-only
source_revision=87f3cdf0`. Downloaded Linux and macOS evidence digests were
`2229862b90ca978f9a9d205ab59bb8414a4396e2d381f35380ad9df22543d5fd` and
`84185d564272f3a360f9d20606b0c99af31162d685a9fb26b103843c4b13ceeb`; the
preserved aggregate artifact is `10683929336` with upload digest
`082db52c7760bf8cad5420b0539dbf7f6658c42a6b0f572fbf3181690c9ac8ab`.
This closes the W07 workflow for the candidate while retaining the explicit
macOS feature-compile-only boundary. It does not prove live macOS
service/cluster/mount, production capacity, identity/ACL, backup/restore,
failover, observability, signing, or owner evidence. CI remains the only
candidate hosted workflow still running, and production remains **NO-GO**.

### W05.50 prepared TiDB interception and Node 9P cleanup repair (2026-09-22 19:11 AEST)

The second repair slice is locally complete. The TiDB ambiguity proxy now
tracks the prepared-statement protocol used by `mysql_async::exec_iter`: it
recognizes the publication `COM_STMT_PREPARE`, records the statement ID from
the upstream `COM_STMT_PREPARE_OK` response, and drops the acknowledgement for
the matching `COM_STMT_EXECUTE` after forwarding it upstream. This makes the
live-service test exercise the intended lost-acknowledgement boundary rather
than returning a successful changed-row count without injection. A focused
packet-classification test covers the prepared path.

The Node 9P port-conflict test now closes the losing server only if it actually
bound. A failed `EADDRINUSE` listener has no owned listener to close, and
avoiding that close removes a possible hosted teardown wedge. Rust formatting,
the locked TiDB integration-test compile check, JavaScript syntax, and diff
checks pass. The focused `MOUNT_RS_SERVER_PHASE=p9 node test/servers.mjs` phase
passed four consecutive local loopback runs. These are local checks only: no
live TiDB service, Windows runner, or executable Rust test was claimed here.
A new immutable candidate and same-SHA hosted packet are still required.
Production remains **NO-GO**.

### W05.51 macOS N-API package gate and exact pushed qualification (2026-09-22 20:01 AEST)

The macOS package gate exposed a real release defect in the Rust 1.95 / LLVM22
toolchain used by this checkout: the generated Darwin dylib could be built by
`napi-rs`, but macOS 27's loader rejected it with `mis-aligned LINKEDIT string
pool`. Direct Cargo cdylib output reproduced the same failure, so this was not
an N-API CLI-only issue. The upstream Rust discussion is tracked in [Rust
#157750](https://github.com/rust-lang/rust/issues/157750); the durable fix is
expected in Rust 1.98, while the repository still supports the older toolchain
in this release lane.

The new `integrations/mount-rs-napi/scripts/build-native.mjs` wrapper preserves
the package build contract, detects Darwin and Rust versions below 1.98, and
adds `MACOSX_DEPLOYMENT_TARGET=11.0` plus
`-C link-arg=-Wl,-platform_version,macos,11.0,26.0` without overwriting caller
flags. With that helper, exact pushed `515bdc00792a62403b0ee7b94e904434d067f43b`
produced an addon with aligned `LC_SYMTAB.stroff=22058472`, `minos=11.0`, and
`sdk=26.0`; the real generated addon loaded through Node and reported the
expected native export surface. The package build and `postbuild` completed
successfully.

The exact pushed SHA then passed `cargo fmt --all -- --check`, the full locked
workspace test suite, strict workspace Clippy, the complete pinned-oracle
Node/N-API suite, and the full PGlite script after the standalone provider
matrix lock was refreshed for the current `md-5` dependency. The PGlite packet
passed the server-slot and fail-closed cleanup checks, Rust reconnect,
versioning, VFS, backup/restore rollback, split-store, FUSE and N-API checks,
Rust SDK `6 pass / 3 skip / 0 fail`, Node SDK `5/3/0`, CLI `12/2`, and the
provider matrix under `--locked`: Rust `6 pass / 3 skip / 0 fail`, Node `5/3/0`,
and CLI `12/2`. The three Rust and three Node R2 rows and two live R2 CLI rows
remained explicit skips because protected R2 credentials/services were absent;
those are not local acceptance. No AWS value, R2 secret, or Keychain item was
read or persisted.

This closes the macOS package and local Rust/Node/SDK/CLI/PGlite implementation
slice only. The concurrent mainline moved to `e86c9ccb` after the qualified
`515bdc00` boundary, so a new immutable candidate and fresh same-SHA hosted
packet are required. Production remains **NO-GO** pending hosted CI, live
provider, native-platform, package/provenance, support-scope, security/OIDC,
and W20.6 evidence.

### W05.52 current immutable candidate local qualification and hosted dispatch (2026-09-22 20:11 AEST)

After the W05.51 package fix, shared mainline continued to move through the
W01 NFS/WebDAV/AWS documentation and implementation successors. I created the
separate immutable candidate branch
`andymac4182/c/w05-production-candidate-20260922c` at exact
`7efded5424f8ad6c2335e7be1cde98c22b9315fe` and did not add later commits to
that branch. On that exact candidate, formatting, the full locked Rust
workspace, strict `-D warnings` Clippy, the optimized N-API build/postbuild,
the Darwin Mach-O alignment check and Node addon load, and the complete
Node/N-API suite passed. The current Rust packet includes the NFS successors:
all 18 9P unit tests, the S3 45-test gateway suite, the full CLI/storage
tests, and the added NFS transport lifecycle/wire/session tests pass.

The real PGlite script also passed on the exact candidate. It reports Rust SDK
`6 pass / 3 skip / 0 fail`, Node SDK `5/3/0`, CLI `12/2`, all PGlite reconnect,
versioning, VFS, backup/restore rollback, split-store, FUSE and N-API checks,
and the `--locked` provider matrix. The R2 rows, live TiDB/RustFS row, direct
native mounts, and FoundationDB feature row remain explicit skips because
credentials/services/privileges were not present. No credential or Keychain
value was read.

The non-R2 hosted packet was dispatched against the same SHA: CI
`35714144497`, Fault `35714146067`, W04 policy `35714141926`, W07 FoundationDB
`35714147247`, W08 policy `35714144646`, W08 targets with `attest=true`
`35714146811`, and Native 9P `35714145176`. Every run initially reported
`queued` with the candidate SHA, so none is promoted to acceptance yet. Live
R2 was not dispatched because the monthly usage envelope remains closed, AWS
was not dispatched because security-provisioned protected OIDC inputs remain
missing, and the production publisher was not dispatched because W20.6 has no
GO decision. Production remains **NO-GO**.

### W05.57 same-SHA Ozone/TiDB hard-IOPS failure classified (2026-09-22 20:50 AEST)

CI `35714144497`, Ozone/TiDB job `106701527631`, ran against the immutable
candidate SHA `7efded5424f8ad6c2335e7be1cde98c22b9315fe`. The durable
composition completed all `1200/1200` write/read/delete lifecycles with zero
timeouts and zero cleanup failures. Its required `minIops=1000` gate measured
`84.01050956100686`, emitted `IOPS_TARGET_NOT_MET`, and then correctly emitted
`RUSTFS_COMBO_FAIL`. Evidence artifact `10689997396` finalized successfully.

This is a provider-capacity failure, not a product assertion failure and not a
reason to lower the hard production floor. The next decision is whether a
repeatable provider/performance repair can meet `1000` or whether Ozone is
excluded from the advertised release surface. Ozone/FoundationDB, the
remaining CI matrix, W07/W08 assembly, AWS/OIDC, post-reset R2, native-FUSE,
package publication/provenance, support scope, and W20.6 remain open.
Production remains **NO-GO**.

### W05.58 same-SHA Ozone/FoundationDB hard-IOPS failure classified (2026-09-22 21:06 AEST)

CI `35714144497`, Ozone/FoundationDB job `106701528236`, ran against the
immutable candidate SHA `7efded5424f8ad6c2335e7be1cde98c22b9315fe`. The durable
composition completed all `1200/1200` operations over `400` lifecycle
iterations with zero timeouts and zero cleanup failures. Its required
`minIops=1000` gate measured `298.6107927173534`, emitted
`IOPS_TARGET_NOT_MET`, and then correctly emitted `RUSTFS_COMBO_FAIL`; the
cleanup marker was `OZONE_CLEANUP_PASS`.

This is a second same-SHA provider-capacity failure, not a product assertion
failure and not a reason to lower the hard production floor. Ozone remains a
production blocker until the provider meets the threshold or an explicit
advertised-support decision excludes it. The remaining CI matrix, W07/W08
assembly, AWS/OIDC, post-reset R2, native-FUSE, package/provenance, support
scope, and W20.6 decision remain open. Production remains **NO-GO**.

### W05.59 terminal W07/native/platform and W08 asset-verification sub-gates (2026-09-22 21:04 AEST)

The exact candidate `7efded5424f8ad6c2335e7be1cde98c22b9315fe` now has the
following additional terminal evidence:

- W07 run `35714147247` completed successfully. Assembly job
  `106709151497` emitted `W07_PLATFORM_QUALIFICATION_PASS` with
  `linux=terminal`, `macos=feature-compile-only`, `provenance=bound`, and
  `source_revision=7efded5424f8ad6c2335e7be1cde98c22b9315fe`. Durable evidence
  artifact `10690300620` and cross-platform artifact `10690995180` were
  finalized; the latter upload digest is
  `a342f32cbd7c8b6fc2336da202fa0c19a8032f4dd5fe0a631f5197d5b57d7953`.
- W08 run `35714146811` passed both release builds and both downloaded-asset
  verifications (`106701533638`, `106701533824`, `106708898505`, and
  `106708898619`). The Linux and macOS checks each reported source commit
  `7efded54`, package version `0.1.0`, 288 SBOM components, and the exact
  artifact digests recorded in W05.59. The two attestation jobs remain queued,
  so W08 is not yet terminal release evidence.
- CI jobs native FUSE `106701527830`, Rust macOS `106701527825`, Rust Windows
  `106701527935`, and native NFS macOS `106701527843` passed. The native-FUSE
  log includes injected read-panic recovery and successful exercised mount,
  SQLite, PGlite, and cleanup tests.

These results close only their named sub-gates. They do not close the overall
candidate while W08 attestation, Ubuntu/provider CI jobs, live R2, AWS/OIDC,
Ozone capacity, package publication/signing, advertised support scope, and
W20.6 remain open. Production remains **NO-GO**.

### W05.60 latest shared-code requalification and candidate preparation (2026-09-22 21:26 AEST)

Current `origin/main` is `2bb846247aca02e9ce47a80a808314ca4beeed29`.
The current code is unchanged from the test-qualified `65776be8` snapshot;
intervening mainline commits are documentation and lockfile updates only.
The following exact-tip-equivalent checks passed:

- Focused NFS and FoundationDB locked suites passed, including NFS v4 wire,
  process-restart, concurrency, lifecycle, and the new cross-version race
  coverage.
- Full `./scripts/cargo-shared test --workspace --all-targets --locked`, strict
  workspace Clippy, `cargo fmt --all -- --check`, and `git diff --check` passed.
- The optimized Darwin addon built and loaded through Node with 253 exports,
  `LC_SYMTAB.stroff=22007680` aligned to 8 bytes, minimum macOS 11.0, and SDK
  26.0.
- The complete Node/N-API/oracle suite passed, and the PGlite matrix passed
  Rust SDK `6/3/0`, Node SDK `5/3/0`, and CLI `12/2`, with live R2/TiDB/RustFS
  paths remaining explicit credential/service skips.

This closes local qualification for the current code but does not promote
old-SHA hosted evidence. The next release-control action is to freeze
`2bb84624` as a new immutable candidate and dispatch a fresh same-SHA packet.
Production remains **NO-GO** pending hosted/provider/security/native/package/
scope gates and W20.6.

### W05.61 current-tip candidate frozen and same-SHA packet dispatched (2026-09-22 21:31 AEST)

Candidate branch `andymac4182/c/w05-production-candidate-20260922d` resolves
to exact SHA `d1a81ad45158dc80647b723f175c01a98b9ed458`. The local packet is
the code-equivalent qualification recorded in W05.60; the candidate adds only
the ledger/mainline documentation successors needed to bind the release
record.

Fresh hosted workflows were dispatched against this exact branch and SHA:

| Gate | Run |
| --- | --- |
| CI | `35721887870` |
| Fault injection | `35721895670` |
| W04 production policy | `35721891003` |
| W07 FoundationDB | `35721892642` |
| W08 release policy | `35721891447` |
| W08 release targets, attestations enabled | `35721892593` |
| Native 9P | `35721891380` |

R2 was deliberately not dispatched because the September usage envelope is
closed. AWS was deliberately not dispatched because the protected
`aws-s3-ci` environment and OIDC role remain an open security request. No
credential value or Keychain item was read. Production remains **NO-GO** until
these workflows and the remaining provider, security, packaging, support-scope,
and W20.6 gates are terminally closed.

### W05.62 current-main requalification and stale-candidate supersession (2026-09-22 21:42 AEST)

Shared `origin/main` at `1f8dcd41` includes the FUSE destroy cancellation fix
and the expanded NFS v3/v4 lookup/remove race regression that were absent from
candidate `d1a81ad4`. The current-main local qualification passed:

- `./scripts/cargo-shared test --workspace --all-targets --locked`, including
  the focused NFS v4 wire suite (`24` passed, including the new race test) and
  the FUSE all-targets suite;
- `./scripts/cargo-shared clippy --workspace --all-targets --locked -- -D warnings`;
- the full Node/N-API SDK, CLI, pinned-oracle, distribution, and restart suite;
- `scripts/test-pglite.sh`: Rust SDK `6 pass / 3 explicit R2 skips / 0 fail`,
  Node SDK `5 pass / 3 explicit skips / 0 fail`, and CLI `12 pass / 2 explicit
  skips / 0 fail`, including real PGlite reconnect, backup/restore, FUSE, and
  N-API checks.

The seven queued hosted workflows for superseded candidate `d1a81ad4` were
cancel-requested: CI `35721887870`, Fault `35721895670`, W04 `35721891003`,
W07 `35721892642`, W08 policy `35721891447`, W08 targets `35721892593`, and
Native 9P `35721891380`. They are not release evidence. R2 remains held by
the closed monthly usage envelope, AWS remains held by the security/OIDC
request, and no credential value or Keychain item was read.

Next action is to publish this ledger update, freeze a new immutable candidate
from the resulting shared tip, dispatch the same non-R2/AWS hosted packet, and
retain terminal exact-SHA evidence before production review. Production
remains **NO-GO**.

### W05.63 current settled candidate frozen and same-SHA packet dispatched (2026-09-22 21:50 AEST)

After the current-main local packet completed, three documentation-only
successors landed on shared main. The exact current tip
`849c76a2c642141579f4ad75cf504e7e49e818df` was therefore frozen without a
code delta from the fully requalified `9e756db3` implementation tip. The
immutable candidate branch is
`andymac4182/c/w05-production-candidate-20260922e`; it must not be mutated.

Fresh hosted workflows were dispatched against this exact branch and SHA:

| Gate | Run |
| --- | --- |
| CI | `35723736254` |
| Fault injection | `35723733151` |
| W04 production policy | `35723736618` |
| W07 FoundationDB | `35723740111` |
| W08 release policy | `35723734639` |
| W08 release targets, attestations enabled | `35723735941` |
| Native 9P | `35723734187` |

R2 was not dispatched because the September usage envelope remains closed;
the existing budget policy remains capped at `20` accepted attempts × `$4`
assumed cost (`$80`) with a `$100` monthly ceiling, but a post-reset token
rotation and live requalification are still required. AWS was not dispatched
because the protected `aws-s3-ci` environment and OIDC role remain absent;
Security issue [#3](https://github.com/andymacclenaghan/mount-rs/issues/3)
remains the approved provisioning path, with no Keychain access. Production
remains **NO-GO** until this packet is terminal, live providers are closed,
package/provenance and support scope are approved, and W20.6 issues a written
GO.

### W05.56 terminal Native 9P lifecycle and root-conformance acceptance (2026-09-22 20:46 AEST)

Native 9P run `35714145176` is terminal-successful at exact candidate SHA
`7efded5424f8ad6c2335e7be1cde98c22b9315fe`. Native job `106701527815`, N-API
lifecycle job `106701528172`, pinned conformance job `106701528173`, and root
conformance job `106706609031` all passed. The root conformance log reports
one test file and 146 tests passed. A post-job Rust-cache save emitted a
permission warning while the job conclusion remained successful; the cache
warning is hosted hygiene and is not promoted as product evidence.

This closes the same-SHA Native 9P gate, but not the release packet. W07
cross-platform assembly, W08 asset verification, CI provider/native jobs, the
Ozone capacity decision, security-provisioned AWS OIDC, post-reset R2,
native-FUSE or approved support scope, package publication/provenance,
advertised product scope, and W20.6 remain open. Production remains
**NO-GO**.

### W05.55 macOS artifact upload timeout classified (2026-09-22 20:42 AEST)

The completed macOS CI log for candidate run `35714144497`, job
`106701528015`, now gives the precise failure boundary. Every functional step
passed: the complete Node/N-API suite, parity checks, PGlite integration and
benchmark, structural macOS mount lifecycle, and upstream conformance. The
storage benchmark artifact finalized successfully as artifact `10689034619`.

The native addon artifact upload sent `10,536,521` bytes and computed digest
`31c94fa8043eac34289b4ef8a4d310256195f963c195c1b994ae9c7b9d7c7f58`, then
failed only at finalization with
`Failed to FinalizeArtifact: Unable to make request: ETIMEDOUT`. The parent CI
run is still queued at exact candidate SHA
`7efded5424f8ad6c2335e7be1cde98c22b9315fe`. This is a hosted artifact-service
or network boundary, not a product-test failure, and it is not promoted to
release acceptance.

After the parent reaches terminal state, the supported failed-job rerun or a
retained already-uploaded artifact can close this infrastructure-only row; a
new code candidate is not justified by this log. W07 assembly, W08 asset
verification, Native 9P root conformance, the remaining CI provider jobs, and
the AWS/R2/Ozone/native/package/scope/W20.6 gates remain open. Production
remains **NO-GO**.

### W05.54 terminal Fault success and partial W07/W08/native evidence (2026-09-22 20:38 AEST)

Fault injection `35714146067` is now terminal-successful on the exact
candidate, with Windows job `106701532629`, Ubuntu job `106701532753`, and
macOS job `106701533083` all passing. This closes the fault-injection slice,
but not the release packet as a whole.

W07 `35714147247` has both its macOS FoundationDB feature compilation and
durable FoundationDB/RustFS/Node/CLI/restart jobs successful; its cross-platform
assembly job `106709151497` remains queued. W08 release targets `35714146811`
has both Linux job `106701533824` and macOS job `106701533638` successful, but
the two downloaded-asset verification jobs remain queued. Native 9P
`35714145176` has native, N-API, and pinned conformance success; root-gated job
`106706609031` remains queued. Candidate CI `35714144497` remains queued with
FoundationDB/RustFS, RustFS, TiDB TLS compile, and Ubuntu ARM Node success, the
macOS Intel artifact-upload failure after all functional steps passed, and the
remaining provider/native/Windows jobs queued or in progress.

These are partial same-SHA results only. The W05 release gate still requires
terminal parent runs, retained artifacts, provider/native/package boundaries,
security-provisioned AWS OIDC inputs, post-reset R2 requalification, the hard
Ozone capacity decision, advertised support scope, and W20.6. Production
remains **NO-GO**.

### W05.53 hosted packet checkpoint and macOS artifact boundary (2026-09-22 20:31 AEST)

The same-SHA hosted packet has moved beyond the initial queue, but it is not
terminal and is not release acceptance. Candidate `7efded5424f8ad6c2335e7be1cde98c22b9315fe`
remains immutable. W04 policy `35714141926` and W08 policy `35714144646` are
terminal-successful. CI `35714144497` is still overall queued: its TiDB TLS
compile and Ubuntu ARM Node jobs passed, while macOS Intel job `106701528015`
failed only at the final `actions/upload-artifact` step. All preceding macOS
Node SDK/CLI/N-API, parity, PGlite, benchmark, structural macOS mount, and
upstream conformance steps passed, so the observed failure is currently a
hosted artifact-service boundary rather than a product-test failure; the
terminal parent log is still required before that classification is final.

Fault injection `35714146067` remains queued with Windows and Ubuntu jobs
successful and macOS pending. W07 FoundationDB `35714147247` is in progress:
the macOS feature compilation job passed and the durable FoundationDB/RustFS/
Node/CLI/restart job is still executing. W08 release targets `35714146811` is
in progress with the Linux release target passed and macOS still running.
Native 9P `35714145176` remains queued: native 9P, N-API lifecycle, and pinned
conformance jobs passed, while the root-gated companion remains queued. No
queued, partial, failed-upload, or non-terminal workflow is promoted to
release acceptance.

The next active step is to let each parent reach terminal state and retrieve
the artifact-service log. If the macOS upload failure is infrastructure-only,
the candidate needs a clean artifact upload or a documented retained artifact;
if any product step fails, a new immutable candidate must carry the repair.
The production checklist remains blocked by Ozone's hard `1000` IOPS gate,
native FUSE or an approved support-scope exclusion, security-provisioned AWS
OIDC inputs, post-reset R2 requalification, package publication/provenance,
advertised support scope, and W20.6. Production remains **NO-GO**.

### W05.49 terminal candidate CI failure and repair boundary (2026-09-22 19:01 AEST)

Candidate CI run `35702348089` is terminal `failure` on exact source
`87f3cdf0a8b3d29c89ff6c1e8d6cbd2409d0c01d`. The redacted job evidence is
classified as follows:

- Ubuntu Rust job `106663105271` timed out the regression
  `destroy_overrides_late_driver_error_for_inflight_call` at
  `transports/mount-rs-9p/src/session.rs:2023`. The test driver called
  `entered.notify_one()` before registering `release.notified()`, so the
  test's `notify_waiters()` could legally be lost. The repair registers and
  enables the release waiter before publishing the entered signal. The
  shared-target compile check passes, but this macOS host cannot execute the
  test until its Xcode license is accepted; this is not presented as a local
  runtime pass.
- Ubuntu Node job `106663105160` timed out during `9P server boundary` after
  `+19842ms`; Windows Node job `106663105335` timed out during the pinned-oracle
  parity phase. These require focused reproduction on the repaired candidate;
  they are not waived as runner noise.
- TiDB job `106663105037` and TiDB/RustFS job `106663105174` both reached the
  ambiguous-publication assertion with provider result `1`, meaning the
  failure injector did not intercept the prepared autocommit publication
  acknowledgement. The proxy must track the MySQL prepared-statement path;
  the test must then prove a dropped acknowledgement produces an unknown
  outcome without replay.
- Ozone/TiDB measured `362.85` successful lifecycle IOPS and the Ozone
  composition packet measured `445.24` for the failing provider and `1720.01`
  for the passing provider against the hard `1000` target. The target remains
  unchanged. W26 evidence job `106671318709` therefore emitted
  `W26_OZONE_EVIDENCE_PACKET_FAIL` because the required `OZONE_IOPS_PASS`
  composition marker was absent. This is a provider/capacity gate, not a
  reason to weaken production acceptance.

The first repair chunk is limited to deterministic test synchronization and
is compile-checked with
`./scripts/cargo-shared check -p mount-rs-9p --tests --locked`. A new
immutable candidate and hosted rerun are still required; the prior candidate
branch is not mutated. Production remains **NO-GO**.

### W05.36 current-tip evidence (2026-09-22 15:43 AEST)

The current exact mainline tip is locally qualified. The clean optimized N-API
build and generated declarations pass typecheck, including the direct
`P9Session(FsDriver, options)` constructor. The direct filesystem-backed and
structural-driver-backed 9P lifecycle tests pass, including callback/error
translation and adapted-driver release on destroy. The complete Node/N-API
suite passes all SDK, CLI, WebDAV, S3, FUSE/9P/NFS, differential,
distribution, and artifact phases, with only the documented provider/native
opt-ins skipped. The full locked Rust workspace, strict Clippy, format/diff
checks, PGlite lifecycle/provider matrix, Rust/Node/CLI matrix, upstream
`1200/82` suite, and all 40×621 oracle traces are green.

This remains a local release qualification, not a production release pass.
Terminal hosted CI/fault/W04/W07/W08 and package/provenance results are not
available on one settled SHA; R2 remains fail-closed at its monthly cap, AWS
protected inputs/OIDC trust still require the approved security path, and
native Linux/Windows/macOS signing and provider/scope/final-audit gates remain
open.

### W05.35 current-tip evidence (2026-09-22 15:26 AEST)

The current pushed tip is locally qualified but is not a production release
candidate yet. On `9e187955`, the permitted host runs passed
`./scripts/cargo-shared fmt --all -- --check`, `git diff --check`,
`./scripts/cargo-shared test --workspace --all-targets --locked`, strict
workspace Clippy, the focused S3 framing regression, the optimized
`pnpm --dir integrations/mount-rs-napi build`, the complete Node/N-API suite,
and `scripts/test-pglite.sh`. The current S3 gateway has 33 passing tests,
including the short streamed-response connection-abort case. The Node suite
passed its SDK, CLI, N-API, WebDAV, S3 restart/barrel, FUSE/9P/NFS, differential,
distribution, and artifact phases; PGlite passed lifecycle, reconnect,
versioning, VFS, split-store, backup/restore/rollback, FUSE, SDK, Node, CLI,
upstream, and trace phases. The explicit skips are R2 credentials, TiDB/RustFS
services, FoundationDB opt-in, and privileged native mounts; they are not
failures, but they remain production gates when those capabilities are in the
advertised support matrix.

The five same-SHA hosted workflows were cancelled before terminal acceptance,
and no R2 run was admitted under the fail-closed monthly cap. This is current
hosted evidence, not a release pass. Remaining production actions are to
select a settled final SHA, obtain terminal hosted CI/fault/W04/W07/W08 and
package/provenance evidence, provision AWS inputs and immutable OIDC trust
through security, rotate the R2 token after the UTC-month reset, close the
advertised-provider and native-platform gates, resolve production scope, and
run W20.6 for the written GO/NO-GO decision.

### W05.12 current-boundary reconciliation

The W05.12 row above contains historical R2 identifiers from the prior
qualification snapshot. The authoritative current boundary is now
`origin/main` `ccd3f671`: exact `eaf59894` is the latest complete local
Rust/Node/SDK/CLI/PGlite qualification, while `ccd3f671` carries subsequent
chunked, 9P, Ozone, NFS, FUSE, direct 9P mount-signal, WebDAV concurrency, and
FoundationDB lockfile changes that still require requalification. The current
R2 admission `35663890558` is queued and the preceding `35663627575` failed at
the monthly envelope before the live job; neither provides provider evidence.
AWS remains blocked by the unchanged security-provisioned-input audit, and no
credential value was read, stored, or placed in Keychain.

Follow-up boundary at 08:50 AEST: exact `a0c73c54` is now the latest complete
local Rust/Node/SDK/CLI/PGlite qualification, while `origin/main` `e7888067`
adds hosted 9P native-lifecycle, WebDAV session-member, Ozone cleanup, and
fragmented HTTP early-rejection changes. Current R2 `35664438822` failed at
the monthly envelope before live admission; no current provider acceptance is
claimed.

Follow-up boundary at 08:56 AEST: exact `d391f9b7` is the current shared tip.
Its widened WebDAV probes and inherited NFS process-crash fence are
focused-green as recorded in W05.16; the latest complete full packet remains
`a0c73c54`. Actions are non-terminal on this tip: R2 `35665105299` pending,
Native 9P `35665105178` queued, W08 policy `35665105293` pending, fault
`35665105186` queued, W08 targets `35665105417` pending, CI `35665105286`
pending, and W04 policy `35665105441` pending.

Follow-up boundary at 09:13 AEST: exact pushed `b65d4e31` is the latest
complete local Rust/Node/SDK/CLI/PGlite qualification. `origin/main`
`9b2dabd7` is a newer successor carrying FUSE handoff, chunked batching, 9P
source parity, WebDAV NodeFs crash recovery, and session-parity metadata;
those changes are not included in the `b65d4e31` full packet. Current hosted
Actions on `9b2dabd7` remain queued or pending, and no current-head hosted,
provider, native, package-publication, or final-audit acceptance is claimed.

Follow-up boundary at 10:21 AEST: fetched `origin/main` is exact
`8ca6c2572782420ae65bd709e3e08b9670d78148`. The full current local packet is
green on that SHA: format, full locked Rust workspace with permitted loopback
binds, strict Clippy, optimized N-API, complete current Node/N-API suite,
real PGlite/provider matrix, Rust/Node/CLI matrices, upstream `1200/82`, and
40 five-seed/eight-backend traces at 621 operations. The packet also covers
the current WebDAV provider-concurrency and in-flight crash/restart tests.
The provider-workflow path helper passed both workflow files. The AWS audit
remains `AWS_S3_OIDC_AUDIT_BLOCKED` for invalid/unreadable role/repository/
environment shape and missing protected bucket/region/account/versioning/
role inputs; no secret was read. The monthly R2 cap still blocks live
admission, so no current-head provider acceptance is inferred. Hosted,
native, package-publication, scope, and W20.6 gates remain open.

Follow-up boundary at 10:46 AEST: fetched and pushed `origin/main` is exact
`db431f4c`. The current exact local packet is green: format, full locked Rust
workspace, strict Clippy, optimized N-API build, complete Node/N-API suite,
and full real PGlite matrix. The clean-build P9 optional-shape regression is
fixed by `730a3de4` in the pushed ancestry. Exact PGlite evidence is
`6/3/0` Rust SDK, `5/3/0` Node SDK, `12/2` CLI, `1200/82` upstream, and
40×621 traces. The current release decision remains NO-GO because no
revision-matched terminal hosted/provider/native/package/publication/final
audit packet exists; no credential value was read or placed in Keychain.

Follow-up boundary at 10:54 AEST: `origin/main` advanced to exact
`1bdd6adf` with the FUSE busy-unmount forced-teardown successor. Its focused
format, package test, and strict Clippy gates passed. The complete local
release packet is still anchored at `db431f4c`; the FUSE delta is focused
green but not yet full-packet or hosted accepted. Production remains NO-GO.

Follow-up boundary at 11:02 AEST: `origin/main` is exact `ff29dad7`, a
docs/site-only successor over `1bdd6adf`. Native 9P `35673957522` is a
terminal success on earlier exact `03cca165`; the other revision-matched runs
were cancelled by subsequent shared-main pushes. No hosted result is promoted
to `ff29dad7`, and production remains NO-GO.

Follow-up boundary at 11:30 AEST: exact pushed `9cb2eef1` is the latest
complete local qualification in this session. Its clean optimized N-API build
and generated typecheck passed after restoring the single canonical P9
`stats.messages: Map<string, number>` normalization; the complete Node/N-API
suite passed, including the S3 session/member differential and WebDAV
durability surfaces, and the Rust N-API package passed all 18 tests plus strict
Clippy. The exact real PGlite packet passed Rust SDK `6/3/0`, Node SDK `5/3/0`,
CLI `12/2`, upstream `1200/82`, and all 40 five-seed/eight-backend traces at
621 operations. `origin/main` has since advanced to `1670ceba`, which includes
the 9P dirent-packer successor, the S3 N-API session-concurrency test, and
concurrent W07/WebDAV documentation; that newer tip is not yet full-packet or
hosted-qualified. Production remains NO-GO.

Follow-up boundary at 11:46 AEST: the exact code tip `2d2b66ef` passed the
complete local packet after the standalone provider-matrix lock repair was
generated offline. Full locked Rust tests, strict workspace Clippy, optimized
N-API build, complete Node/N-API suite, and the real PGlite packet all passed;
the PGlite packet reports Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`,
upstream `1200/82`, and 40×621 traces. The first PGlite attempt correctly
failed closed because `tests/provider_matrix/Cargo.lock` lacked the new direct
`tokio` edge from the R2 in-flight-upload change; the repair is one lockfile
entry and the rerun passed. During this qualification window `origin/main`
advanced to `412c422e`, adding 9P frame-assembler and hosted WebDAV native
concurrency successors that remain unqualified by this packet. Production
remains NO-GO.

Follow-up boundary at 12:02 AEST: exact pushed `6797a2d8` is fully qualified
locally through the 9P/WebDAV successor and provider-matrix lock repair. The
exact full packet passed format, locked Rust workspace tests, strict Clippy,
optimized N-API build, complete Node/N-API suite, and real PGlite; it reports
Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all
40×621 traces. Same-SHA hosted W04 `35677127007`, fault `35677126925`, W08
policy `35677126941`, and W08 targets `35677126934` completed successfully;
Live R2 `35677126924` failed and CI `35677127048` was cancelled, so no hosted
R2 or full-CI acceptance is promoted. The latest fetched `origin/main` is now
`819c663e`, which adds further 9P bounded-reader, N-API provider-network,
Ozone/RustFS lock, and documentation/site changes after `6797a2d8`; that tip
is not yet full-packet qualified. Production remains NO-GO.

Follow-up boundary at 12:32 AEST: exact shared tip `42ecd21e` is now fully
qualified locally. Format, the full locked Rust workspace, strict workspace
Clippy, optimized N-API build, complete Node/N-API suite, and the complete
real PGlite packet all passed. The current packet reports Rust SDK `6/3/0`,
Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40
five-seed/eight-backend traces at 621 operations. It includes the new S3
conditional PUT/CAS gateway coverage (`29` gateway tests), typed 9P/N-API
surfaces, refreshed AWS/FoundationDB lockfiles, WebDAV detached-cleanup
handling, cleanup-failure fail-closed checks, and artifact aggregation. Exact
same-SHA hosted status is not release acceptance: CI `35679133377`, W08
policy `35679133114`, and W08 targets `35679133090` were cancelled during
subsequent shared-main movement; no current-head terminal full packet exists.
The latest fetched `origin/main` is now `3de49e33`, adding a chunked lease
release repair and other 9P/N-API successors that are outside this exact
packet. Its W04 policy `35679778395` is successful, while CI
`35679778373` is pending, fault `35679778367` is queued, W08 policy
`35679778356` is pending, W08 targets `35679778394` are pending, and live R2
`35679778383` is queued; these are not terminal release evidence. Production
remains NO-GO.

Follow-up boundary at 12:44 AEST: exact pushed `f1ee869a` is fully qualified
locally after including the chunked failed-publication lease release and 9P
synchronous live-mount inspection/type updates. Format, full locked Rust,
strict Clippy, optimized N-API, complete Node/N-API, and the full real PGlite
packet all passed with Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`,
upstream `1200/82`, and all 40×621 traces. Exact same-SHA W04
`35680042918` and W08 targets `35680043018` succeeded, while CI
`35680042985`, fault `35680043013`, and W08 policy `35680042906` were
cancelled during later movement; no hosted full-release acceptance is
claimed. The latest fetched `origin/main` is `7389be4d`, adding a 9P direct
probe absence-shape normalization and FoundationDB qualification helpers
outside this exact packet. Production remains NO-GO.

Follow-up boundary at 12:51–12:53 AEST: exact pushed `1bdf8846` is fully
qualified locally. The code-identical packet passed `cargo fmt --all -- --check`,
the full locked Rust workspace, strict workspace Clippy, the optimized N-API
build, the complete Node/N-API suite, and the real PGlite matrix. It reports
Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all
40 five-seed/eight-backend traces at 621 operations. The current Node suite
also passed WebDAV provider/network concurrency and crash/restart, S3
restart/barrel scope, P9/9P/FUSE/NFS/WebDAV differential surfaces, FoundationDB
shape checks, chunked integration, distribution, and artifact aggregation;
provider credentials and privileged native mounts remain explicit skips where
required. Exact-SHA hosted status is not release acceptance: W04
`35680709392` and fault injection `35680709400` succeeded, W08 policy
`35680709423` was still in progress, W08 targets `35680709394` were queued,
and CI `35680709436` was cancelled during shared-main movement. After the
packet, `origin/main` advanced to `6d65716f`; the diff from `1bdf8846` to that
tip is limited to `WORK_TRACKER.md` and W01/W26 documentation, so the tested
implementation remains code-identical but no hosted result is promoted to the
new tip. Production remains NO-GO.

Follow-up boundary at 12:57–13:00 AEST: the pushed ledger checkpoint
`75248969` triggered a fresh hosted snapshot. W04 policy `35681358469`
succeeded, while fault injection `35681358424`, CI `35681358476`, and W08
policy `35681358387` were cancelled during workflow concurrency; W08 targets
`35681358456` remained in progress at the final bounded poll. These statuses
are recorded as non-terminal/cancelled hosted evidence only. The checkpoint
changes documentation only over the tested `1bdf8846` implementation, and
the production decision remains NO-GO pending one selected SHA with terminal
green hosted/provider/native/package/scope gates.

## Production-readiness dependency register

Historical production-candidate addendum (superseded candidate, 2026-09-22 16:39 AEST): the selected
release-control branch is
`andymac4182/c/w05-production-candidate-20260922` at immutable SHA
`25e275ab6d4f918be72dcd8f62a5baca6bbcd251`. Its exact local Rust/Node/SDK/CLI
packet is green, but the release gate is not closed: W07 and W08 target jobs
are still in progress, CI and Fault injection are queued, AWS security inputs
are absent, R2 is cap-held, provider/native/package/scope gates remain open,
and W20.6 has not issued a GO. The candidate is intentionally isolated from
moving `origin/main`; any later ledger publication must preserve this exact
SHA-to-run mapping.

This register expands W05 from a closed provider slice into the complete
production path. Percentages and estimates are provisional planning values,
not a weighted release score. Every row must either reach 100% with evidence
or be explicitly removed from the release scope by a recorded decision before
the final audit can issue a GO decision.

### Historical immutable-candidate addendum (superseded by W05.47; 2026-09-22 18:34 AEST)

The active release-control candidate is the immutable branch
`andymac4182/c/w05-production-candidate-20260922` at exact SHA
`87f3cdf0a8b3d29c89ff6c1e8d6cbd2409d0c01d`. Its local Rust/Node SDK/CLI,
N-API, PGlite/oracle, packaging, and license packet is green. Same-SHA
hosted evidence currently has Native 9P `35702349894`, Fault injection
`35702350927`, W04 policy `35702351026`, and W08 release policy `35702353241`
terminal-successful. W07 durable qualification job `106663122874` is green,
but W07 macOS feature compilation `106663122748` remains queued. W08 Linux
and macOS builds plus macOS asset verification are green, while Linux asset
verification `106671605507` and attestation remain queued. CI `35702348089`
remains queued. These job-level results are not promoted to workflow or
production acceptance until their parent workflows are terminal and their
redacted artifacts are reviewed.

Live R2 remains fail-closed behind the monthly usage cap and short-lived-token
rotation. AWS remains blocked by security issue [#3](https://github.com/andymac4182/mount-rs/issues/3) and its missing protected OIDC inputs;
no credential value was read, stored, or accessed through Keychain. The
production decision is **NO-GO** until CI/W07/W08 terminal evidence, AWS and
post-reset R2 provider gates, platform/package/signing and clean-install
evidence, support scope, and W20.6 are complete.

Current release-candidate addendum: exact tested pushed `1bdf8846` is the
strongest current local implementation/SDK/CLI/PGlite packet, while fetched
`origin/main` `6d65716f` is a docs-only successor over that code. Neither is a
production release candidate yet. PR-00 remains blocked by the UTC-month R2 admission cap
and required short-lived-token rotation; PR-02, PR-04, PR-06, and PR-09 need
revision-matched hosted/provider/platform evidence; PR-03 needs clean
cross-platform artifact/publication evidence; PR-08 needs an explicit scope
decision; and PR-10 must issue the final W20.6 GO/NO-GO. The AWS read-only
audit at this boundary is:
`AWS_S3_OIDC_AUDIT_BLOCKED invalid_role_arn_shape github_repository_unreadable
github_environment_unreadable missing_environment_variable_MOUNT_RS_AWS_S3_TEST_BUCKET
missing_environment_variable_MOUNT_RS_AWS_S3_TEST_REGION
missing_environment_variable_MOUNT_RS_AWS_S3_ACCOUNT_ID
missing_environment_variable_MOUNT_RS_AWS_S3_TEST_VERSIONING_STATUS
missing_environment_secret_MOUNT_RS_AWS_S3_CI_ROLE_ARN`.

Latest release-candidate addendum: exact tested `1bdf8846` is green locally,
but its same-SHA CI was cancelled while W08 policy remained in progress and
W08 targets queued; fetched `origin/main` `6d65716f` is a docs-only successor
with no new hosted acceptance. PR-00 remains cap-held; PR-02, PR-04,
PR-06, and PR-09 still need revision-matched hosted/provider/platform
evidence; PR-03 needs clean cross-platform artifact/publication evidence;
PR-08 needs an explicit scope decision; and PR-10 must issue the final W20.6
GO/NO-GO. No credential value was read, stored, or placed in Keychain.

Latest exact release-candidate addendum at 13:35 AEST: pushed
`de78011a530b8aebd5422892c07138eed73834cb` is locally fully qualified after
the WebDAV peer-reset and structural bounded-listing test correction. The
exact packet passes formatting, the full locked Rust workspace, strict
Clippy, optimized N-API, the complete Node SDK/CLI suite, real PGlite,
Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all
40×621 traces. Hosted state is not release acceptance: R2
`35683264337` failed closed before secrets at `count=291 limit=20`; CI
`35683264288` is not terminal green, Native 9P `35683264325` is queued, W08
policy `35683264328` is in progress, and W04 `35683264285`, fault injection
`35683264298`, and W08 targets `35683264327` were cancelled. AWS remains
blocked by the read-only security/OIDC audit and its missing protected
inputs; no security destination is configured in this workspace, so no
request was fabricated and no Keychain access was attempted. PR-00, PR-02,
PR-04, PR-06, PR-09, and PR-10 remain open until the final same-SHA packet,
provider/native/package/scope evidence, and written W20.6 decision are
green.

The current successor `d43f5ea4e4334912de86ac0db818392531a7d4ec` adds S3
observability/session hooks, a 9P listener-policy lifecycle test, and W07
telemetry evidence after that packet; it is not yet locally requalified or
hosted-accepted. The de780 hosted snapshot remains revision-specific and the
R2 cap, AWS security/OIDC, native, package, scope, and final-audit gates stay
open.

Latest qualification addendum at 13:50 AEST: exact pushed
`175615c7265a49b4d07ed16c57e63e9ae771df26` (`175615c7`) is fully qualified
locally after the d43 S3 observability/session-hook successor. Formatting,
`git diff --check`, full locked Rust workspace tests, strict Clippy, optimized
N-API build, the complete Node SDK/CLI suite, and the real PGlite/oracle
matrix all pass. The packet reports Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI
`12/2`, upstream `1200/82`, and 40×621 traces; it includes the PGlite
backup/restore/rollback, split-store/VFS/FUSE, cleanup-failure, WebDAV, S3,
9P, NFS, differential, distribution, and artifact checks. Fetched/pushed
`origin/main` `9563d2db8d73b8583212eed00f5b909cbcadf27e` is documentation-only
over that tested runtime. Same-SHA hosted W04 `35684095331`, fault injection
`35684095300`, and W08 release policy `35684095289` succeeded; CI
`35684095286` and W08 release targets `35684095309` were still in progress at
capture. R2 remains closed by the monthly admission cap before secret use;
AWS security/OIDC inputs, native/platform, package/provenance, provider,
scope, and W20.6 final-audit gates remain open. No credential value was read,
stored, printed, or placed in Keychain.

Latest qualification addendum at 13:59 AEST: exact pushed
`b3cfbcb419a2a24bd89750358c96701f98ba3130` (`b3cfbcb4`) is fully qualified
locally after the S3 per-key driver-error hook change. The focused regression,
format/diff checks, full locked Rust workspace, strict Clippy, optimized
N-API, complete Node SDK/CLI suite, and real PGlite/oracle matrix all pass;
the packet reports Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream
`1200/82`, and 40×621 traces. Same-SHA W04 `35684755110` succeeded, but W08
targets `35684755036`, CI `35684755170`, W08 policy `35684755009`, and fault
injection `35684755061` were cancelled by mainline movement. Fetched/pushed
`origin/main` `1179d9e3fbdb95ea1cca9866fd249c949614a9e1` adds 9P EOF/backpressure
and WebDAV server changes and requires a fresh packet. R2 remains cap-held;
AWS security/OIDC inputs, native/platform, package/provenance, provider,
scope, and W20.6 final-audit gates remain open. No credential value was read,
stored, printed, or placed in Keychain.

Latest qualification addendum at 14:08 AEST: exact pushed
`8d7e9f3d577daa22735d1e31079c0971c2e7eddd` (`8d7e9f3d`) is fully qualified
locally after the 9P EOF/backpressure and teardown changes. Formatting,
`git diff --check`, the full locked Rust workspace, strict Clippy, optimized
N-API build, complete Node SDK/CLI suite, and real PGlite/oracle matrix all
pass; the packet reports Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`,
upstream `1200/82`, and 40×621 traces. Same-SHA W04 `35685285569`, fault
injection `35685285680`, W08 policy `35685285584`, and W08 targets
`35685285524` succeeded; CI `35685285624` was cancelled. Fetched
`origin/main` `4f6e10483c461e0d8f8e309d1ea8380b6313e1b9` adds recursive WebDAV
mutation, duplicate-header, listener-restart, and N-API changes and requires
a fresh packet. R2 remains cap-held; AWS security/OIDC inputs,
native/platform, package/provenance, provider, scope, and W20.6 final-audit
gates remain open. No credential value was read, stored, printed, or placed
in Keychain.

Latest exact-current qualification addendum at 15:10 AEST: exact pushed
`01f844c1145278765438e7b367cd560e811476a6` (`01f844c`) is fully qualified
locally after the SQLite publication and HTTP/Windows parity successors. The
exact packet passed `cargo fmt --all -- --check`, `git diff --check`, the full
locked Rust workspace, strict workspace Clippy, the optimized N-API build and
postbuild, the complete elevated Node SDK/CLI suite, and
`scripts/test-pglite.sh`. The packet reports 28 WebDAV Rust tests, Rust SDK
`6 pass / 3 skip / 0 fail`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200
passed / 82 skipped`, and all 40 five-seed/eight-backend traces at 621
operations. The current fetched `origin/main` is documentation-only successor
`7cc9c8c5f53b08b3b9255e831dc272e7209aca8a` (`7cc9c8c5`) over that tested
implementation. Its hosted snapshot is not terminal: W04
`35689926763` and W08 policy `35689926768` are queued, CI
`35689926753` and W08 targets `35689926756` are pending, fault injection
`35689926749` is queued, and no R2 run was admitted because the monthly cap
remains closed. No credential value was read, stored, printed, or placed
in Keychain; no security request was fabricated.

| ID / mapped workstreams | Work type | Status | Completion | Evidence now | Remaining actions / ship criterion | Provisional engineering time | External blockers / boundaries |
| --- | --- | --- | ---: | --- | --- | --- | --- |
| PR-00 / W05 | Implementation + provider + hosted CI | Complete W05 slice; current requalification held by cap | 100% | Final live Cloudflare R2 run `35579757447` passed the Rust/Node SDK and CLI packet, live trace, N-API, service evidence, budget guard, and artifact upload. Current exact local packet `01f844c` is green across the complete Rust/Node/PGlite/CLI packet, but no current-head live R2 run is admitted; current `origin/main` `7cc9c8c5` is a documentation-only successor and its R2 admission remains closed at the monthly cap. | Rotate the four short-lived secrets before 2026-09-28; after the UTC-month reset, run one bounded current-release requalification only on the selected final SHA and retain terminal evidence. | 0.5–1 h maintenance/requalification setup | Provider alert is notification-only; the CI envelope is a fail-closed cost control, not a billing hard stop. |
| PR-01 / W01–W04 | Implementation + parity + storage/provider acceptance | Open dependency closure | 55% | Core, metadata/block split, memory/SQLite, and PGlite packets have substantial local evidence; the tracker still leaves W01 parity items, W02 mixed-provider/durability items, W03 migration/concurrency items, and W04 hosted reconnect work open. | Close all applicable parity and storage rows, including seeded cross-engine traces, stale writers/CAS, partial uploads, migrations, concurrent open/reopen, rollback, and hosted macOS/Linux reconnect. Ship criterion: no required W01–W04 row remains open or unscoped. | 8–16 h active engineering, plus hosted wait | Full API scope and provider semantics must be confirmed against `REQUIREMENTS.md`; local passes do not close hosted gates. |
| PR-02 / W06–W08 | Real-service/provider composition | W07 qualification green on prior revision; W08/current release closure open | 68% | RustFS service composition and bounded provider packets pass. W07 `35623069365` completed successfully on `1acc15f`, with production policy, real PGlite/Node prerequisites, default N-API build, Linux FUSE prerequisite, durable FoundationDB metadata/RustFS chunks/restart, evidence markers, and artifact `foundationdb-production-qualification-35623069365-1`. W08 durable TiDB topology/restart/capacity and release-manifest work is present in shared main, but current W08 runs are repeatedly superseded and service credentials/hosted acceptance remain open. | Obtain W07 evidence on the final release revision if the shared head advances; run real FoundationDB/TiDB/RustFS compositions with restart, fencing, ambiguous-commit, capacity, Rust/Node/CLI, and hosted evidence. Ship criterion: every advertised production provider has a revision-matched hosted or explicitly supported deployment gate. | 6–18 h active engineering, plus service provisioning | W07 success is revision-specific; FoundationDB/TiDB/RustFS services, durable topology, credentials, hosted capacity, and long-running non-canceling workflow execution remain external/provider gates. |
| PR-03 / W09–W11 | SDK/API/CLI implementation + package/native artifacts | Exact local package qualification green; hosted/package gates open | 96% | Exact tested pushed `01f844c` passes public Rust SDK, Node/N-API package, Rust CLI, Node CLI, declaration/build packets, real PGlite matrix, and the complete pinned-oracle package suite. The packet includes the 9P remote-admission test, SQLite publication fast-path coverage, bounded HTTP/Windows parity, WebDAV provider/crash/concurrency coverage, S3 restart/barrel scope, FUSE/9P/NFS differentials, and distribution/artifact aggregation. Current `origin/main` `7cc9c8c5` is documentation-only over this tested implementation; publication and platform gates remain open. | Close platform artifact aggregation/publication, Rust/Node CLI provider matrix on every advertised platform, native loading, and transport auto-selection. Ship criterion: a clean checkout can build, install, typecheck, and run supported SDK/CLI flows for every advertised platform. | 2–8 h | Node native artifacts, package registries, platform toolchains, supported-version policy, Windows runners, and native mount privileges are external boundaries. |
| PR-04 / W10, W12, W13, W15, W27 | Native and cross-platform acceptance | Open platform gate | 48% | Exact tested `01f844c` passes the mount-free Rust/N-API FUSE/NFS/9P/WebDAV/S3 surfaces and elevated NFS server integration, but reports direct native mount acceptance unavailable/opt-in. Current hosted W04 `35689926763` is queued, while no terminal current-head Linux FUSE/9P, macOS FSKit, Windows runtime/mount, or signed artifact acceptance exists. FSKit is unsigned and SQLite-hosting/VFS hosted gates remain open. | Execute revision-matched Linux FUSE/NFS/9P/WebDAV, macOS native/FSKit signing and activation, Windows Rust/Node runtime/mount, SQLite VFS multi-process/recovery, and crash/unmount gates. Ship criterion: each supported platform has a passing native gate, or the platform is explicitly excluded from the release support matrix. | 10–24 h active engineering, plus platform queue | `/dev/fuse`, macOS entitlements/signing, FSKit activation, Windows runners, kernel behavior, and physical host access cannot be replaced by local structural tests. |
| PR-05 / W14, W16, W17 | Versioning, adapters, HTTP server, multi-drive and deployment | Partial | 40% | Versioned-filesystem foundation, just-bash/Mastra local adapters, server/CLI local edge cases, and RustFS remote evidence exist; integration, cloud/TLS/deployment, per-drive authorization/isolation, and Cloudflare acceptance remain open. | Close version IDs/publication/pins/replay/CAS, adapter shared-state behavior, HTTP discovery/routing/ranges/TLS, multi-drive authorization and recovery, and real HTTP-client/provider acceptance. Ship criterion: documented deployment topology and security isolation pass from clean clients. | 6–14 h | TLS/domain/deployment configuration, remote services, multi-client concurrency, and per-drive tenancy need hosted validation. |
| PR-06 / W18–W20 | Performance, compression, dependency budget, CI, packaging and release controls | Current local code-quality gate passed; hosted/release gate open | 72% | Exact tested pushed `01f844c` passes format, full locked workspace, strict Clippy, release N-API build/suite, real PGlite matrix, upstream `1200/82`, and five-seed trace parity. On current docs-only tip `7cc9c8c5`, W04 `35689926763` and W08 policy `35689926768` are queued, CI `35689926753` and W08 targets `35689926756` are pending, fault injection `35689926749` is queued, and no R2 run was admitted. No terminal current-release hosted packet exists; AWS remains security-preflight-blocked. | Requalify the final release SHA after any implementation changes, run one unsuperseded hosted workflow to completion, then complete benchmark/dependency/license audits, package provenance checks, required hosted macOS/Linux/Windows jobs, and the final acceptance audit. Ship criterion: no required workflow is queued, cancelled, skipped, or failed on the release SHA. | 6–16 h active engineering, plus hosted/provider queue | Hosted runners, package registries, signing/provenance, AWS security provisioning, the R2 monthly cap, and release automation are external gates; pending/queued/denied runs are not release evidence. |
| PR-07 / W21, W28, W30 | Reference decisions, deterministic faults, observability and operations | Partial | 35% | Several reference reviews and local collector/failure/benchmark/macOS observability packets exist; remaining reviews, deterministic fault workloads, external collector/Linux/Windows evidence, and operational runbooks remain open. | Finish required reference/license review, inject and classify deterministic failures across providers/transports, validate external telemetry and alerting, and publish rollback/recovery/runbook evidence. Ship criterion: known failure modes are reproducible, observable, bounded, and recoverable. | 6–14 h | External collectors, failure-injection environments, and operational ownership are required for production evidence. |
| PR-08 / W22, W23, W29, W31 | Deferred/future scope decision | Scope decision required | 0% | Distributed cache, physical copy-on-write, lifecycle hooks, and per-drive mounts are marked deferred/future in the tracker; no production inclusion/exclusion decision is recorded in this W05 ledger. | Record whether each item is required for the first production release. Required items become implementation gates; deferred items must have a documented non-blocking rationale and follow-up. Ship criterion: no ambiguous requirement remains. | 1–3 h decision work, then variable implementation time | Product requirements and user approval are external blockers; the goal must not silently declare future scope irrelevant. |
| PR-09 / W24–W26 | Product surface and additional providers | Scope-dependent | 50% | The site/domain is live; AWS private test-resource and local Rust SDK/CLI evidence exists; Apache Ozone local/hosted qualification exists within its documented scope; the shared tip adds a secret-safe synthetic production-rollout contract checker with valid and negative fixtures, which validates admission shape only. The protected AWS workflow validates shape and secret-safe preconditions before OIDC; the read-only audit identifies the missing `aws-s3-ci` protection policy, bucket/region/account/versioning variables, and role-ARN secret; hosted run `35629600687` passed provenance and synthetic contract suites but failed closed at `missing_bucket` before AWS authentication. Current AWS `35641426061` is still in progress and is not evidence. Production resource/metadata/DR gates remain open. The shared AWS/Ozone/WebDAV documentation successors record evidence but do not constitute provider acceptance. | Decide advertised providers/surfaces, provision `MOUNT_RS_AWS_S3_TEST_BUCKET`, `MOUNT_RS_AWS_S3_TEST_REGION`, `MOUNT_RS_AWS_S3_TEST_VERSIONING_STATUS`, and `MOUNT_RS_AWS_S3_CI_ROLE_ARN` through the approved security/OIDC path, then close AWS/Ozone Rust/Node/CLI/restart/hosted tests or remove them from the production support matrix. Ship criterion: every advertised provider and public surface has a green acceptance packet and support statement. | 4–12 h per included provider | Cloud provider accounts, service deployment, package/domain ownership, security-provisioned workload identity, and hosted capacity are external gates. |
| PR-10 / W20.6 | Final audit and release decision | Not started | 0% | The tracker currently reports overall status `in progress; not release-ready`; the previous W05 close explicitly did not claim whole-product production readiness. | Audit `REQUIREMENTS.md`, `PORTING_STATUS.md`, API parity, tracker, CI artifacts, support matrix, security/license/dependency records, rollback plan, and every required test result. Issue GO only when all required rows are green on one revision; otherwise record NO-GO and exact blockers. | 2–4 h after dependencies close | Requires all required implementation, hosted, native, provider, packaging, and scope decisions to be complete. |
| PR-11 / W05.9 current exact-SHA packet | Release-candidate requalification and moving-main reconciliation | Local implementation packet complete; release gate open | 100% local / 0% hosted closure | Exact pushed `01f844c` passed format/diff, full locked Rust workspace including the HTTP/Windows parity successor, strict Clippy, optimized N-API build/postbuild, elevated Node SDK/CLI suite, and real PGlite/provider/CLI/oracle matrix. Counts: Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, `40 × 621` traces. `7cc9c8c5` is a docs-only successor, so the local implementation packet is reusable as ancestry evidence but not terminal current-tip release evidence. | Select one final SHA after the moving mainline settles; rerun or attach exact-SHA local packet, let CI/W04/W08/fault/native/package jobs finish without cancellation, run one admitted R2 packet after the UTC reset and credential rotation, close AWS/security/OIDC and advertised-provider gates, then issue W20.6 GO/NO-GO. | 1–3 h for each successor requalification, plus 8–24 h hosted/provider/platform wait | R2 monthly cap and token expiry/rotation, no approved security destination for AWS credentials, privileged Linux/macOS/Windows/native hosts, provider services, package registries/signing, and concurrent origin/main movement. |
| PR-12 / W05.51 | macOS N-API artifact, provider-matrix lock, and exact Rust/Node/SDK/CLI/PGlite qualification | Local slice complete; release gate open | 100% local slice / 0% hosted closure | Exact pushed `515bdc00` loaded the rebuilt Darwin addon on macOS 27 after the Rust `<1.98` linker workaround, and passed the full locked Rust workspace, strict Clippy, complete pinned-oracle Node/N-API suite, PGlite lifecycle/provider/CLI packet, Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, and the `--locked` provider matrix after the one-line `md-5` lock refresh. The current shared mainline moved to `e86c9ccb` during qualification and has no replacement same-SHA hosted packet. | Requalify a new immutable candidate from settled main; finish terminal CI/Fault/W04/W07/W08/attestation/Native 9P/FUSE/package evidence, live AWS/R2 provider gates, advertised support scope, and W20.6. | 1.5–3 h active; 4–16 h hosted/provider/native/publication wait | Concurrent mainline movement, hosted runners, provider services, AWS protected OIDC inputs, R2 reset/rotation, native privileges, package registries/signing, scope ownership, and final audit remain external. |
| PR-13 / W05.52 | Immutable candidate local qualification + same-SHA hosted release packet | Local packet complete; hosted packet queued | 100% local / 10% hosted provisional | Exact candidate `7efded54` passes format, full locked Rust workspace, strict Clippy, current NFS/S3/9P tests, dyld-safe macOS N-API build/load, complete Node/N-API suite, PGlite/Rust/Node/CLI matrix, and explicit provider/native skip accounting. Runs CI `35714144497`, Fault `35714146067`, W04 `35714141926`, W07 `35714147247`, W08 policy `35714144646`, W08 targets `35714146811`, and Native 9P `35714145176` were dispatched against the exact SHA and observed queued. | Wait for terminal same-SHA workflows; retain attestation/package artifacts; repair actionable failures on a new immutable candidate; close provider, native, package/provenance, support-scope, AWS/R2, and W20.6 gates. | 0.5–1.5 h active; 2–16 h hosted/provider/native/publication wait | GitHub queue, provider services, AWS security administration, R2 cap/reset, native privileges, signing/registries, scope and final audit remain external. |

| PR-14 / W05.53 | Hosted candidate packet checkpoint and artifact-service classification | Active; non-terminal hosted packet | 100% local / 24% hosted provisional | Exact candidate `7efded54` is locally green. W04 policy `35714141926` and W08 policy `35714144646` passed. CI `35714144497` remains queued overall with macOS Intel job `106701528015` failing only in final artifact upload after all functional steps passed; Fault `35714146067` is queued with Windows/Ubuntu success; W07 `35714147247` is in progress; W08 targets `35714146811` is in progress; Native 9P `35714145176` is queued with its substantive jobs green and root-gated companion queued. | Retrieve terminal logs, complete the same-SHA packet, rerun or retain the macOS artifact, repair any product failure on a new immutable candidate, and close R2/AWS/Ozone/native/package/scope/W20.6 gates. | 0.5–1.5 h active; 2–16 h hosted/provider/native/publication wait | GitHub queue/artifact service, root privileges, security administration, R2 reset, providers, registries/signing, and final scope/audit remain external. |
| PR-15 / W05.54 | Terminal Fault success and partial W07/W08/Native 9P evidence | Fault terminal-successful; hosted release packet remains open | 100% local / 39% hosted provisional | Exact candidate `7efded54`: Fault `35714146067` passed Windows `106701532629`, Ubuntu `106701532753`, and macOS `106701533083`; W07 macOS compile and durable qualification passed while assembly `106709151497` is queued; W08 Linux `106701533824` and macOS `106701533638` builds passed while download verification is queued; Native 9P substantive jobs passed while root job `106706609031` is queued; CI `35714144497` remains queued with the isolated macOS artifact-upload failure and incomplete remaining matrix. | Finish terminal assembly, asset verification, root conformance, CI classification, artifacts and package/provenance checks; repair product failures on a new immutable candidate; then close provider, security/OIDC, R2, Ozone, native, support-scope and W20.6 gates. | 0.5–1.5 h active; 2–16 h hosted/provider/native/publication wait | GitHub queue/artifact service, root privileges, security administration, provider services, R2 reset, registries/signing, scope and final audit remain external. |
| PR-16 / W05.55 | macOS native-artifact upload timeout classification | Infrastructure-only failure classified; parent CI remains non-terminal | 100% local / 45% hosted provisional | CI `35714144497` job `106701528015` completed all functional gates and successfully finalized storage benchmark artifact `10689034619`; native artifact upload of `10,536,521` bytes failed at finalization with `ETIMEDOUT` after digest `31c94fa8043eac34289b4ef8a4d310256195f963c195c1b994ae9c7b9d7c7f58`. | Wait for terminal parent, rerun the failed job or retain the functional artifact, then finish W07/W08/Native 9P/CI and all provider, security, package, scope and W20.6 gates. | 0.5–1 h active; 2–16 h hosted artifact/runner wait | GitHub artifact-service/network availability is external; no code candidate change is required for this timeout. |
| PR-17 / W05.56 | Terminal Native 9P lifecycle and root-conformance acceptance | Terminal-successful native 9P gate; broader release packet open | 100% local / 55% hosted provisional | Exact candidate `7efded54`; Native 9P run `35714145176` and jobs `106701527815`, `106701528172`, `106701528173`, `106706609031` all passed. Root conformance reports 146/146 tests passed; cache-save permission warnings are retained as hosted hygiene only. | Complete W07/W08/CI terminal gates and all provider, AWS/OIDC, R2, Ozone, native-FUSE, package, scope and W20.6 requirements. | 0.25–0.75 h active; 2–16 h hosted/provider/native/publication wait | Remaining gates are hosted, provider, security, platform, publication and product-scope boundaries. |
| PR-18 / W05.57 | Same-SHA Ozone/TiDB hard-IOPS failure classification | Terminal provider-capacity failure; no implementation regression inferred | 100% local / 42% hosted provisional | CI job `106701527631` completed 1200/1200 lifecycles with zero timeout/cleanup failures but measured `84.01050956100686` IOPS against the hard `1000` floor and emitted `IOPS_TARGET_NOT_MET`; artifact `10689997396` finalized. | Retain the hard floor, classify Ozone/FoundationDB/remaining CI, and decide performance repair versus advertised-scope exclusion before W20.6. | 0.5–2 h active; 2–16 h provider wait | Ozone capacity and hosted runner conditions are external; remaining AWS/R2/native/package/scope gates are open. |
| PR-19 / W05.58 | Same-SHA Ozone/FoundationDB hard-IOPS failure classification | Terminal provider-capacity failure; no implementation regression inferred | 100% local / 44% hosted provisional | CI job `106701528236` completed 1200/1200 operations over 400 iterations with zero timeout/cleanup failures but measured `298.6107927173534` IOPS against the hard `1000` floor, emitted `IOPS_TARGET_NOT_MET` and `RUSTFS_COMBO_FAIL`, and recorded `OZONE_CLEANUP_PASS`. | Retain the hard floor, classify the remaining CI/provider/native gates, and decide performance repair versus advertised-scope exclusion before W20.6. | 0.5–2 h active; 2–16 h provider wait | Ozone/FoundationDB capacity and hosted runner conditions are external; remaining AWS/R2/native/package/scope gates are open. |
| PR-20 / W05.59 | Terminal W07/native/platform and W08 asset-verification sub-gates | Named hosted sub-gates passed; release packet remains open | 100% local / 68% hosted provisional | W07 `35714147247` passed with bound provenance and artifacts `10690300620`/`10690995180`; W08 build/download verification passed for Linux and macOS with source `7efded54`, 288 SBOM components, and recorded package digests, but attestations remain queued. CI native FUSE `106701527830`, Rust macOS `106701527825`, Rust Windows `106701527935`, and native NFS macOS `106701527843` passed. | Wait for W08 attestations and remaining CI jobs; retain artifacts, classify failures, and close Ozone/AWS/R2/package/scope/W20.6 gates. | 0.75–1.5 h active; 2–16 h hosted/provider/publication wait | Attestation/runner queues, Ozone capacity, AWS security administration, R2 reset/rotation, registries/signing, support scope, and final audit remain external. |
| PR-21 / W05.60 | Latest shared-code local requalification and immutable-candidate preparation | Local current-code packet complete; new hosted packet not yet dispatched | 100% local / 0% current-tip hosted provisional | Current `origin/main` `2bb84624` is code-equivalent to tested `65776be8` after documentation/lockfile-only successors. Focused NFS/FoundationDB, full locked Rust, strict Clippy, formatting/diff, optimized Darwin addon load, full Node/N-API, and PGlite/Rust/Node/CLI matrix passed; live R2/TiDB/RustFS remained explicit skips. | Freeze `2bb84624`, dispatch fresh same-SHA hosted workflows, retain artifacts, and close R2/AWS/Ozone/native/package/scope/W20.6 gates. | 1.5–3 h active; 4–16 h hosted/provider/native/publication wait | Mainline movement, runner queues, R2 cap/rotation, AWS security/OIDC, Ozone capacity, native privileges, signing/registries, support scope, and final audit remain external. |
| PR-22 / W05.61 | Immutable current-tip candidate and same-SHA hosted packet dispatch | Candidate published; seven non-R2/AWS workflows pending | 100% local / 5% hosted provisional | Candidate branch `andymac4182/c/w05-production-candidate-20260922d` resolves to `d1a81ad45158dc80647b723f175c01a98b9ed458`; CI `35721887870`, Fault `35721895670`, W04 `35721891003`, W07 `35721892642`, W08 policy `35721891447`, W08 targets/attestations `35721892593`, and Native 9P `35721891380` were dispatched on that branch. R2 and AWS were intentionally held behind the cap and security request. | Poll all seven workflows, retain terminal artifacts, repair any implementation failure on a new candidate, then close R2/AWS/Ozone/native/package/scope/W20.6. | 0.5–1.5 h active; 4–16 h hosted/provider/native/publication wait | GitHub queues, R2 cap/rotation, AWS security/OIDC, Ozone capacity, native privileges, signing/registries, support scope, and final audit remain external. |
| PR-23 / W05.62 | Requalify current mainline after FUSE/NFS successors and supersede stale hosted packet | Current-main local packet green; superseded candidate packet cancellation-requested; new candidate not yet dispatched | 100% local / 0% current-tip hosted provisional | Shared `origin/main` `1f8dcd41` includes the FUSE destroy cancellation fix and expanded NFS v3/v4 race coverage. Full locked Rust workspace, strict Clippy, focused NFS/FUSE suites, complete Node/N-API SDK/CLI/oracle/distribution/restart suite, and `scripts/test-pglite.sh` passed. The PGlite matrix reported Rust SDK `6/3/0`, Node SDK `5/3/0`, and CLI `12/2`; live R2/TiDB/RustFS remained explicit skips. The seven queued runs for superseded `d1a81ad4` (CI `35721887870`, Fault `35721895670`, W04 `35721891003`, W07 `35721892642`, W08 policy `35721891447`, W08 targets `35721892593`, Native 9P `35721891380`) were cancellation-requested and are not acceptance. | Publish this ledger update, freeze a new immutable candidate from the resulting shared tip, dispatch the exact-SHA hosted packet, retain terminal artifacts, and close R2/AWS/Ozone/native/package/scope/W20.6. | 1–3 h active local qualification/release coordination; 4–16 h hosted/provider/native/publication wait | GitHub runner backlog, R2 cap/rotation, AWS security/OIDC, Ozone capacity, native privileges, signing/registries, support scope, and final audit remain external. |
| PR-24 / W05.63 | Freeze settled current candidate and dispatch fresh same-SHA hosted packet | Current candidate published; seven non-R2/AWS workflows running or queued | 100% local / 5% hosted provisional | Immutable branch `andymac4182/c/w05-production-candidate-20260922e` resolves to exact `849c76a2c642141579f4ad75cf504e7e49e818df`. The code was requalified at its immediate implementation predecessor: focused NFS (`43` unit, `25` v4 wire), chunked (`22`), full locked Rust workspace, strict Clippy, optimized Darwin addon load (`253` exports, `stroff` aligned, minos 11.0, SDK 26.0), full Node/N-API suite, diagnostic WebDAV provider concurrency, and PGlite Rust/Node/CLI matrix (`6/3/0`, `5/3/0`, `12/2`). Exact-SHA runs: CI `35723736254`, Fault `35723733151`, W04 `35723736618`, W07 `35723740111`, W08 policy `35723734639`, W08 targets/attestations `35723735941`, and Native 9P `35723734187`. | Poll every run to terminal, retain artifacts/source bindings, repair any actionable product failure on a new immutable candidate, then close post-reset R2, Security-provisioned AWS/OIDC, Ozone/native/package/support-scope gates, and W20.6. | 1–2 h active dispatch/tracking; 4–16 h hosted/provider/native/publication wait | GitHub runner queues, R2 reset/token rotation, AWS security administration, Ozone capacity, native privileges, registries/signing, support scope, and final audit remain external. |

### Current immutable-candidate addendum (2026-09-22 17:09 AEST)

The selected candidate remains `25e275ab6d4f918be72dcd8f62a5baca6bbcd251`.
Hosted W04 policy, W07 FoundationDB/provider qualification, W08 policy,
Native 9P, fault injection, and the attestation-enabled W08 release-target
packet are terminal-successful on that SHA. W08 produced and verified both
the Linux `x86_64-unknown-linux-gnu` and macOS `aarch64-apple-darwin`
artifacts, with CycloneDX SBOM attestations uploaded to the repository and
the Rekor transparency log. Candidate CI `35694325175` is still running;
green jobs are recorded, but `tidb`, `ozone-tidb`, `tidb-rustfs`,
`ozone-foundationdb`, and W26 Ozone evidence are failed at the job level and
cannot be classified until GitHub exposes terminal logs. This is a hosted
evidence checkpoint, not release acceptance. AWS/OIDC security provisioning,
live R2 post-reset requalification, remaining provider/native/package/scope
decisions, and W20.6 remain open.

### Production exit criteria

The goal is complete only when all of the following are true on one release
revision: (1) every required register row is 100% or explicitly excluded by
decision; (2) required CI, fault-injection, provider, native, and packaging
jobs are green rather than queued, cancelled, skipped, or merely locally
passing; (3) Rust and Node SDK/CLI artifacts build and install from a clean
checkout; (4) supported-platform and unsupported-platform behavior is
documented; (5) credentials, provenance, license/dependency records,
rollback/recovery procedures, and operational telemetry are reviewed; and
(6) W20.6 records a written GO decision. W05's successful R2 packet is a
necessary provider gate, not a substitute for these release criteria.

## Cost and credential control

| Control | Current setting / evidence | What it proves | Limitation |
| --- | --- | --- | --- |
| GitHub environment | `r2-ci`, four encrypted secrets, no repository plaintext values | CI receives credentials only at the live job boundary | A maintainer with environment administration can still change the secrets or policy. |
| Token scope | Object Read & Write, only `mount-rs-integration-tests`, one-week TTL | Limits the test credential to the test bucket and a short rotation window | Cloudflare token scope is not a dollar quota. |
| CI admission guard | 20 workflow admissions per UTC month, `$4.00` assumed per run, `$80.00` maximum envelope, fail closed before secrets are used; final admitted run marker was `accepted_run=12/20`, `remaining_slots=8` | Prevents this workflow from admitting more than 20 live runs and leaves `$20` headroom to the requested `$100` ceiling; the final admitted run used the warning-free summary path | The guard conservatively counts workflow attempts, including attempts denied after the cap is exhausted. It is a cost envelope, not a provider billing hard stop; per-run workload changes must stay within the bounded packet. |
| Cloudflare alert | Account-wide R2 alert at `$80` | Provides early notification before the target ceiling | The alert does not automatically suspend R2 usage. |
| Workflow bounds | Live job timeout 120 minutes, unique run/prefix IDs, exact cleanup, benchmark artifact retained | Limits runaway job duration and isolates remote fixtures | Timeout is an execution bound, not a billing guarantee. |

## Evidence boundary matrix

| Surface | Result | Acceptance meaning |
| --- | --- | --- |
| Exact tested pushed revision `1bdf8846` | Full local Rust/Node/SDK/CLI/PGlite packet green; same-SHA hosted partial/non-terminal; provider/native/package/scope closure open | `cargo fmt --all -- --check`, the full locked Rust workspace, strict workspace Clippy, optimized N-API build, complete Node/N-API suite, and the real PGlite matrix passed. Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40 five-seed/eight-backend traces passed at 621 operations. W04 and fault injection succeeded, W08 policy was still in progress, W08 targets were queued, and CI was cancelled; no hosted release acceptance is claimed. |
| Exact current `origin/main` `6d65716f` | Documentation-only successor over tested `1bdf8846`; hosted/provider/native/package/scope closure open | `git diff --name-status 1bdf8846..6d65716f` contains only `WORK_TRACKER.md` and W01/W26 documentation. The implementation is code-identical to the tested packet, but the new tip has no terminal hosted release packet and is not promoted to production. |
| Exact tested current revision `6797a2d8` | Full local Rust/Node/SDK/CLI/PGlite packet green; same-SHA hosted partial; provider/native/package/scope closure open | Format, full locked Rust workspace, strict Clippy, optimized N-API build, complete Node/N-API suite, WebDAV package tests/Clippy, and the real PGlite matrix passed. Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40 five-seed/eight-backend traces passed at 621 operations. Same-SHA W04, fault, and W08 policy/targets succeeded; R2 failed and CI was cancelled, so no hosted release acceptance is claimed. |
| Exact current `origin/main` `819c663e` | Newer 9P/N-API/provider/site successor; full packet and hosted/provider/native/package/scope closure open | The current tip adds bounded 9P reader-limit preservation, N-API provider-network cleanup retry changes, Ozone/RustFS standalone lock refreshes, and documentation/site updates after the tested `6797a2d8`. It has not yet received the full exact-SHA local packet or terminal hosted acceptance. |
| Exact tested current revision `42ecd21e` | Full local Rust/Node/SDK/CLI/PGlite packet green; same-SHA hosted partial/cancelled; provider/native/package/scope closure open | Format, full locked Rust workspace, strict Clippy, optimized N-API build, complete Node/N-API suite, and the real PGlite matrix passed. Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40 five-seed/eight-backend traces passed at 621 operations. The packet includes S3 conditional PUT/CAS coverage (`29` gateway tests), typed 9P/N-API declarations/codecs, WebDAV detached-cleanup handling, and refreshed AWS/FoundationDB locks. Same-SHA CI `35679133377`, W08 policy `35679133114`, and W08 targets `35679133090` were cancelled during successor movement. |
| Exact current `origin/main` `3de49e33` | Newer chunked/9P/N-API successor; full packet and hosted/provider/native/package/scope closure open | The current tip adds a chunked lease-release repair plus the `56291e3` 9P live-mount inspection successor and related declarations/tests over the locally qualified `42ecd21e` ancestry. Its W04 policy `35679778395` is successful, while CI `35679778373` is pending, fault `35679778367` is queued, W08 policy `35679778356` and W08 targets `35679778394` are pending, and live R2 `35679778383` is queued. No terminal current-tip release packet is claimed. |
| Exact tested pushed revision `f1ee869a` | Full local Rust/Node/SDK/CLI/PGlite packet green; same-SHA hosted partial/cancelled; provider/native/package/scope closure open | Format, full locked Rust workspace, strict Clippy, optimized N-API build, complete Node/N-API suite, and the real PGlite matrix passed. Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40 five-seed/eight-backend traces passed at 621 operations. The packet includes chunked failed-shutdown lease release, 9P synchronous live-mount inspection/type surfaces, S3 conditional PUT/CAS, WebDAV detached cleanup, cleanup-failure fail-closed, and artifact aggregation. Same-SHA W04 `35680042918` and W08 targets `35680043018` succeeded; CI `35680042985`, fault `35680043013`, and W08 policy `35680042906` were cancelled. |
| Exact current `origin/main` `7389be4d` | Newer 9P/FoundationDB successor; full packet and hosted/provider/native/package/scope closure open | The current tip adds 9P direct-probe absence-field normalization and FoundationDB qualification helper/test changes after the locally qualified `f1ee869a` ancestry, plus shared W01/W04/W08 documentation. It has not received the exact full local packet or terminal hosted acceptance; no evidence is promoted from `f1ee869a`. |
| Exact tested code tip `2d2b66ef` plus provider-matrix lock repair | Full local Rust/Node/SDK/CLI/PGlite packet green; hosted/provider/native/package/scope closure open | Format, full locked Rust workspace, strict Clippy, optimized N-API build, complete Node/N-API suite, and the rerun full real PGlite packet passed. Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40 five-seed/eight-backend traces passed at 621 operations. The standalone lock now includes the direct R2 `tokio` edge. R2/TiDB/RustFS/native rows were explicit credential/service/platform skips; this does not close same-SHA hosted or provider acceptance. |
| Exact current `origin/main` `412c422e` | Newer 9P/WebDAV successor; full packet and hosted/provider/native/package/scope closure open | The current tip adds the shared 9P frame assembler, expanded 9P parity declarations/tests, and hosted WebDAV native concurrent-I/O coverage over the tested `2d2b66ef` ancestry. The full exact-SHA packet has not yet been rerun on this tip, so no evidence is promoted from `2d2b66ef` to `412c422e`. |
| Exact tested revision `9cb2eef1` | Full local Rust/Node/SDK/CLI/PGlite packet green; hosted/provider/native/package/scope closure open | Clean optimized N-API build/typecheck, complete Node/N-API suite, Rust N-API package `18/18` plus strict Clippy, and the exact real PGlite matrix passed. Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40 five-seed/eight-backend traces passed at 621 operations. R2/TiDB/RustFS rows were explicit credential/service-gated skips; AWS/OIDC audit remained fail-closed. This is the strongest current local evidence, not same-SHA hosted or provider acceptance. |
| Exact current `origin/main` `1670ceba` | Newer 9P/S3/W07 successor; full packet and hosted/provider/native/package/scope closure open | The current tip includes the 9P dirent-packer max-size change, S3 N-API session-concurrency coverage, and concurrent W07/WebDAV documentation over the tested `9cb2eef1` ancestry. It has not yet received the full exact-SHA Rust/Node/PGlite packet or terminal hosted acceptance, so no evidence is promoted from `9cb2eef1` to this tip. |
| Exact current `origin/main` `1bdd6adf` | Focused FUSE delta green; full packet and hosted/provider/native/package/scope closure open | The exact current tip passed formatting, 61 non-native `mount-rs-fuse` package tests, and strict package Clippy. The delta adds bounded forced teardown for Linux busy-unmount helper outcomes. The complete Rust/Node/PGlite packet remains evidenced at `db431f4c`; native mount behavior, hosted workflows, live providers, package publication, and final audit are not closed by this focused result. |
| Exact current `origin/main` `ff29dad7` | Docs/site-only successor; implementation evidence inherited, hosted final packet open | No Rust or Node runtime source changed after the `1bdd6adf` focused FUSE gate. Native 9P `35673957522` passed on exact `03cca165`; CI/fault/W04/W08 runs on that moving predecessor were cancelled and do not qualify this tip. Treat the full packet at `db431f4c` and FUSE focus at `1bdd6adf` as ancestry evidence only until one selected release SHA reaches terminal hosted acceptance. |
| Exact current `origin/main` `db431f4c` | Full local packet green; hosted/provider/native/package/scope closure open | `cargo fmt --all -- --check`, full locked Rust workspace with permitted loopback binds, strict workspace Clippy, optimized N-API build, complete current Node/N-API suite, and `scripts/test-pglite.sh` all passed on the exact pushed SHA. Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and 40 five-seed/eight-backend traces at 621 operations passed. P9 optional declarations, WebDAV provider/network concurrency, crash/restart, CLI, distribution, and artifact aggregation passed. R2 credentials were absent by policy and provider rows skipped; AWS/OIDC audit remained fail-closed. This does not close hosted CI, live provider, privileged native, package publication, or final-audit gates. |
| Exact current `origin/main` `8ca6c257` | Full local packet green; hosted/provider/native/package/scope closure open | `cargo fmt --all -- --check`, full locked Rust workspace with permitted loopback binds, strict workspace Clippy, optimized N-API build, complete current Node/N-API suite, and `scripts/test-pglite.sh` all passed. Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and 40 five-seed/eight-backend traces at 621 operations passed. Current WebDAV provider-concurrency and in-flight crash/restart tests passed; R2 credentials were absent by policy, and AWS/OIDC audit remained fail-closed. This does not close hosted CI, live provider, privileged native, package publication, or final-audit gates. |
| Exact pushed revision `de78011a` | Full local packet green; hosted/provider/native/package/scope closure open | The exact pushed `origin/main` SHA passed format, full locked Rust workspace tests, strict workspace Clippy, optimized N-API build, complete Node SDK/CLI suite, real PGlite matrix, Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40 five-seed/eight-backend traces at 621 operations. The Node suite includes the deterministic WebDAV peer-reset check and structural-driver `501` bounded-listing boundary. Exact hosted snapshot: R2 `35683264337` failed closed at `count=291 limit=20` before secrets; CI `35683264288` was not terminal green; Native 9P `35683264325` was queued; W08 policy `35683264328` was in progress; W04 `35683264285`, fault `35683264298`, and W08 targets `35683264327` were cancelled. No production acceptance is promoted. |
| Exact current `origin/main` `d43f5ea4` | New S3/9P/W07 successor; full packet and hosted/provider/native/package/scope closure open | The current tip adds S3 observability/session hooks, a 9P Unix-listener policy lifecycle test, and W07 authority-telemetry evidence over the fully qualified `de78011a` ancestry. It has not yet received the exact full Rust/Node/PGlite packet or terminal hosted acceptance, so no evidence is promoted from `de78011a` to `d43f5ea4`. |
| Repository implementation | `d43f5ea4` is the latest observed origin/main; exact local packet evidence is anchored at `de78011a` | The tested ancestry includes the W05 ledger ancestry, CI workflow, budget guard, hosted orchestration, shared Cargo wrapper use, provider-matrix R2 CLI coverage, lease-refresh fix, strict-Clippy TiDB fix, Windows release-gate repairs, AWS protected-input/PGlite pairing and preflight hardening, release-package declaration fixes, S3 publication/staged-upload retention/ETag fixes, W08 release-manifest checks, Ozone provider matrix, W07 FoundationDB workflow, N-API S3 session lifecycle/concurrency and provider-network cleanup retry, the new S3 observability/session hooks, 9P property-shaped clients/mount helpers plus the shared frame assembler and bounded reader limits, FUSE lifecycle/syncfs and bounded native frames, NFS session hooks plus owner/lease callbacks, session-member views and shutdown cancellation, WebDAV lifecycle plus timed-out drain preservation/concurrent-client coverage and hosted native concurrency, chunked stale-read/concurrency plus atime coalescing and bounded mutation cancellation, W04 lease-TTL configuration, generated N-API declarations including the canonical P9 stats Map normalization, FoundationDB lease-policy validation, the standalone provider lock repair, FUSE teardown/read-worker/native-session fixes, Ozone/RustFS lock refreshes, and the WebDAV bounded-listing/peer-reset test boundary. |
| Isolated current-head Rust gates | Full workspace evidence remains green on exact tested `6797a2d8`; `819c663e` successor qualification pending; final hosted release evidence remains open | The exact `6797a2d8` packet passed formatting, the full locked Rust workspace with permitted loopback binds, strict workspace Clippy, optimized N-API build, R2 unit/gateway/HTTP interop tests, and WebDAV package tests/Clippy. The later `819c663e` 9P/N-API/provider successor has not yet been promoted from its focused changed-surface boundary. Live-provider and privileged-native opt-ins remain explicit skips. |
| Current-head local SDK/CLI/PGlite | Full local Rust/Node/PGlite qualification passed on exact `6797a2d8`; newer origin successor `819c663e` is unqualified | `scripts/test-pglite.sh` passed after the standalone lock was regenerated and the isolated oracle dependencies installed, including the repaired `--locked` provider matrix, real PGlite reconnect/versioned/VFS/lifecycle/split-store/FUSE checks, N-API PGlite/factory/chunked checks, Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40 five-seed trace/backend combinations at 621 operations. R2/TiDB/RustFS rows remained explicit service/credential-gated skips. |
| Local Node SDK/N-API | Passed with pinned `mountx` oracle and rebuilt release artifact on exact `6797a2d8` | The optimized release build and full `pnpm --dir integrations/mount-rs-napi test` passed typecheck, harness, smoke/contract, P9 metadata/locks/observability, 9P surfaces, S3 session/member differential, WebDAV durability/concurrency, factories, lifecycle, codecs including FUSE syncfs, JS driver, bounded-unstorage parity, differential, restart, distribution, and artifact aggregation. Native host mount is explicitly unavailable; R2, PGlite factory, and FoundationDB opt-ins remain separately classified. |
| Pushed WebDAV successor `0a2cf5df` / code `74d386dd` | Focused current-tip regression gate green | The WebDAV package format check, strict focused Clippy, all 18 WebDAV tests, optimized N-API rebuild, and complete Node suite passed, including the new timed-out drain-state test and WebDAV lifecycle/SQLite reopen coverage. This focused delta evidence extends the exact full-packet boundary without claiming a fresh full PGlite run for the docs-only wrapper commit. |
| Pushed NFS successor `816107f6` / code `3589a142` | Focused current-tip regression gate green | The NFS package format check, strict focused Clippy, 38 Rust tests plus rootless transport/v4 tests, optimized N-API rebuild, and complete Node suite passed, including owner/group callback and injected-clock coverage in the N-API server path. This focused delta evidence does not claim a fresh full PGlite packet for the changed NFS surface. |
| Local package artifacts and metadata | Clean-install/package gate passed locally | `npm pack --dry-run --ignore-scripts` passed for `@mount-rs/core` and `@mount-rs/virtual-fs`, including license/notices and the core native artifact; the locked Cargo normal dependency inventory has 719 lines and 25/25 workspace package licenses are Apache-2.0. After adding the explicit pnpm 11 `allowBuilds: false` policy, a clean frozen install of all 233 virtual-fs packages, TypeScript typecheck, and just-bash/Mastra integration tests passed. |
| Local Rust and Node CLI | Passed on exact `9354362a`; live R2 current-head gate blocked | The current matrix reports Rust SDK `6/3/0`, Node SDK `5/3/0`, and CLI `12/2`; the earlier live R2 CLI/N-API packet remains authoritative at `35579757447`. The current hosted/provider gate remains open because no live R2 admission is allowed before cap reset and token rotation. |
| Local native | macOS NFS structural read/write/teardown passed | Native local qualification only; Linux FUSE and other host prerequisites remain separate. |
| Hosted run `35570596593` | Budget passed; live job canceled | Partial hosted evidence only. It must not be called a release or W05 acceptance pass. |
| Hosted diagnostic run `35575940720` | Reached the Rust CLI and reported redacted `ESTALE: stale file handle, metadata lease` during graceful shutdown | Failure evidence that identified the lease-refresh defect; not an acceptance pass. |
| Hosted run `35577687152` | Passed the full hosted packet after `c71c8ee` refreshed an expired unfenced lease during shutdown | Hosted acceptance evidence before the final budget-summary formatting correction. |
| Final hosted run `35579757447` | Passed at `3db491e`; budget `12/20`, full SDK/CLI/N-API/service/trace packet, and artifact `10630468958` | Authoritative W05 hosted acceptance. |
| Hosted run `35607523296` | Fault injection passed on older head `88ecee2` | Useful hosted fault evidence, but not current-head release acceptance; current CI/package/native/provider results still need one unsuperseded revision. |
| Hosted W07 predecessor `35616476141` | Failed on `d07c711` after PGlite/RustFS setup | The redacted log records `mount-rs: mount(2) failed ... Invalid argument (os error 22)` in the privileged FUSE CLI test; no FoundationDB provider acceptance is claimed. Shared `0c4d5cb` is the targeted safety-flag repair. |
| Hosted W07 `35623069365` | Success on `1acc15f` | All W07 steps passed, including durable FoundationDB metadata, RustFS chunks, restart, evidence markers, and artifact `foundationdb-production-qualification-35623069365-1`; retain as revision-specific evidence only, then obtain a W07 run on the final release revision if shared main advances. Older W07 failures remain revision-specific evidence only. |
| Hosted current-main R2 `35628414156` | Failed at the monthly usage-envelope step with `count=139 limit=20`; live jobs skipped | Successful cost-safety refusal; no current-main live R2 evidence or provider operation was admitted, and the encrypted secrets were not passed to the skipped live jobs. Do not loosen the cap to force a run. |
| Hosted current-main AWS `35625592317` | Failed closed at `AWS_S3_CI_CONFIG_BLOCKED missing_bucket` before OIDC/provider tests | This records the missing security-provisioned AWS environment boundary; it is not an AWS provider failure or acceptance pass. No credential value was read or persisted. |
| Hosted exact-SHA packet `1bdf8846` / `35680709392`, `35680709400`, `35680709394`, `35680709423`, `35680709436` | W04 and fault injection succeeded; W08 policy in progress; W08 targets queued; CI cancelled | These statuses are revision-matched evidence only and do not form a production acceptance packet because required jobs are non-terminal or cancelled. R2/AWS were not admitted for this tip; the monthly R2 cap and security/OIDC preflight remain explicit blockers. |
| Hosted ledger checkpoint `75248969` / `35681358469`, `35681358424`, `35681358456`, `35681358387`, `35681358476` | W04 succeeded; fault/CI/W08 policy cancelled; W08 targets remained in progress at bounded poll | The checkpoint is documentation-only over the tested implementation. Hosted concurrency cancellation and the non-terminal W08 target are not release acceptance; no provider credential was admitted or read. |
| Hosted current-main CI/W08/fault | At the 2026-09-22 08:23 AEST live check, current head was `b27dd2bb`: CI `35662346488` queued with duplicate CI `35662257713` queued, W04 policy `35662257709` pending, W08 policy `35662257731` pending, fault `35662257717` queued, and W08 targets `35662257718` pending; no terminal workflow for exact `6f29d9ce` or `b27dd2bb` was listed | These runs are not terminal release evidence for the final release revision and may be superseded by concurrent shared-main pushes. The preceding R2 admission `35662206094` on superseded `5026a230` remained queued only at admission and was not provider evidence. The current local evidence does not promote queued, cancelled, stale, revision-mismatched, or cap-denied runs to release acceptance; a fresh terminal packet on one final SHA is required. |
| Hosted S3 publication regression `7ed1075` / fix `39a19fd` | Hosted Rust and Node HTTP paths reported nested streaming PUT `404`/`200` mismatch and test-driver `501`; local follow-up passed gateway `14/14`, R2 interop `2/2`, and earlier Node HTTP parity `40` paired cases | The implementation regression and staged-upload retention defect are fixed locally and included in the current ancestry; only a terminal unsuperseded hosted CI pass closes this release gate. |
| Hosted fault matrix `35598843164` | Storage-faults passed on preceding shared revision `82ffeb9` | Useful diagnostic evidence only; it does not close current-head full CI, live R2, AWS, signed/native, or package-publication gates. |
| Hosted W07 qualification `35598049389` | Durable FoundationDB/RustFS/Node/CLI/restart workflow completed successfully on older head `65c52b9`; `35606741719` was still in progress on `4aadbb1` | These are useful provider evidence for their respective older revisions, not a production pass for current head `d7ccdc7`; the current revision-mismatched run is `35617054948` and the post-`0c4d5cb` dispatch remains required. |
| Hosted R2 cap boundary `35598843157` | Budget job failed with `count=55 limit=20`; live integration was skipped before secrets | This is a successful safety refusal and proves no further current-month R2 job is admitted or receives the encrypted secrets; it is not provider acceptance evidence. |
| Hosted AWS security boundary `35598843178` / `35612723255` / `35621201833` | Protected-input preflight failed closed before OIDC or provider tests | This records an external security-provisioning blocker and proves the fail-closed guard; it is not an AWS provider failure and must not be called a hosted pass. The local read-only audit independently reports the missing protected environment policy, variables, and role-ARN secret without reading values. Security must provide the inputs through the approved security path, not by reading the local keychain. |

## Remaining action plan

1. The requested W05 acceptance is complete: final hosted run `35579757447`
   is successful and the evidence is recorded here and in `WORK_TRACKER.md`.
   Current-release requalification is a separate open action because the
   fail-closed R2 cap and AWS security preflight have blocked new provider
   evidence.
2. Select one final release SHA and let the required CI, fault, W07, AWS, and
   package workflows reach terminal results on it. Exact tested `1bdf8846`
   is the strongest current local Rust/Node/PGlite/CLI packet; current
   `origin/main` `6d65716f` is docs-only over that implementation, but no
   hosted result currently qualifies the final SHA. Requalify the full packet
   after any later code tip before promoting a hosted result.
   Record every failed, cancelled, skipped, or externally blocked job rather
   than treating a partial matrix as green.
   W07 `35623069365` is a terminal success on `1acc15f`, but it cannot qualify
   an unreleased final SHA. The captured intermediate runs were superseded;
   older W07 runs cannot qualify the current release revision.
3. Execute PR-01 through PR-07 in dependency order, updating this ledger
   after each implementation or hosted/native/provider chunk and pushing the
   corresponding commit to `origin/main`.
4. Resolve PR-08 scope decisions before the final audit; do not silently treat
   deferred work as either required or irrelevant.
5. Before the CI token expires on 2026-09-28, rotate the four `r2-ci` secrets
   through the approved security path. Do not request a live run until the
   UTC-month cap resets; then run one bounded acceptance packet on the final
   release revision. Do not copy secret values into issues, logs, this ledger,
   or chat.
6. Keep macOS native NFS evidence distinct from Linux FUSE, Windows, and
   signed/activated FSKit gates; those remain separate platform workstreams.
7. Keep the unrelated accidental read-only all-buckets Cloudflare token
   outside this workstream until its deletion is explicitly authorized.
8. Provision the `aws-s3-ci` protected environment through security/OIDC
   administration: its protection rule and protected-branch policy, the
   `MOUNT_RS_AWS_S3_TEST_BUCKET`, `MOUNT_RS_AWS_S3_TEST_REGION`,
   `MOUNT_RS_AWS_S3_ACCOUNT_ID`, and
   `MOUNT_RS_AWS_S3_TEST_VERSIONING_STATUS` variables, the encrypted
   `MOUNT_RS_AWS_S3_CI_ROLE_ARN` secret, immutable GitHub OIDC trust, and the
   metadata-disabled policy. The read-only audit currently reports each of
   these missing without reading a value. Then rerun the bounded AWS hosted
   gate without placing credentials in source or logs.
9. Keep the provider-matrix lock repair (`futures-util` in
   `tests/provider_matrix/Cargo.lock`) in the release branch; the current
   PGlite harness proves the Rust, Node, and CLI matrix executes under
   `--locked` and still skips absent live-provider credentials explicitly.

## Session time log

Times below are provisional wall-clock/activity estimates. Hosted waiting is
shown separately from active engineering time.

| UTC time | Activity | Classification | Result / next state |
| --- | --- | --- | --- |
| 2026-09-22 11:42–11:51 UTC (21:42–21:51 AEST) | Rechecked the settled implementation tip, froze immutable candidate `849c76a2`, and dispatched the fresh same-SHA non-R2/AWS packet | Release engineering / local qualification / hosted coordination | The exact candidate branch `andymac4182/c/w05-production-candidate-20260922e` was created from `origin/main`; CI `35723736254`, Fault `35723733151`, W04 `35723736618`, W07 `35723740111`, W08 policy `35723734639`, W08 targets `35723735941` with attestations, and Native 9P `35723734187` were dispatched. R2 remains closed by the monthly cap and AWS remains security/OIDC gated; terminal hosted evidence is the next checkpoint. |
| 2026-09-22 11:31–11:42 UTC (21:31–21:42 AEST) | Requalified current shared mainline after the FUSE/NFS successors, completed the full Rust/Clippy and Node/PGlite packet, and superseded the stale `d1a81ad4` hosted packet | Current-tip local release qualification / hosted queue hygiene | Full locked Rust workspace and strict Clippy passed; Node/N-API SDK/CLI/oracle/distribution/restart passed; PGlite passed Rust `6/3/0`, Node `5/3/0`, CLI `12/2`; focused NFS/FUSE regressions passed. Cancellation was requested for the seven queued `d1a81ad4` workflows; no hosted acceptance was inferred. Next state is a new immutable candidate from the resulting shared tip; R2/AWS remain cap/security gated. |
| 2026-09-22 11:26–11:31 UTC (21:26–21:31 AEST) | Published immutable candidate `d1a81ad4` and dispatched the fresh same-SHA non-R2/AWS hosted packet | Release engineering / hosted qualification coordination | Candidate branch `andymac4182/c/w05-production-candidate-20260922d` was verified at the exact SHA; CI `35721887870`, Fault `35721895670`, W04 `35721891003`, W07 `35721892642`, W08 policy `35721891447`, W08 targets `35721892593` with attestations, and Native 9P `35721891380` were dispatched. R2/AWS remain held by cap/security gates; W05.61/PR-22 records the next terminal-evidence checkpoint. |
| 2026-09-22 10:56–11:04 UTC (20:56–21:04 AEST) | Re-polled the exact candidate, retrieved terminal W07/W08/native/platform logs, and reconciled the new hosted evidence | Hosted provider/platform/native/package evidence | W07 passed with bound provenance and artifacts; W08 Linux/macOS builds and downloaded-asset verification passed with attestations queued; native FUSE, Rust macOS/Windows, and macOS NFS passed. W05.59/PR-20 records these green sub-gates while production remains NO-GO. |
| 2026-09-22 11:04–11:26 UTC (21:04–21:26 AEST) | Reconciled concurrent NFS/FoundationDB/W08 mainline successors and requalified the code-equivalent current shared snapshot | Current-tip local release qualification / candidate preparation | Focused NFS/FoundationDB, full locked Rust, strict Clippy, optimized N-API load, full Node/N-API suite, and PGlite/Rust/Node/CLI matrix passed; current `2bb84624` has no code delta from tested `65776be8` beyond docs/lockfile successors. W05.60/PR-21 records the next immutable-candidate action; production remains NO-GO. |
| 2026-09-22 11:02–11:06 UTC (21:02–21:06 AEST) | Retrieved the terminal Ozone/FoundationDB provider log and classified its hard capacity gate | Hosted provider-capacity evidence | Ozone/FoundationDB completed 1200/1200 operations over 400 iterations with zero timeout/cleanup failures but measured 298.61/1000 IOPS; `IOPS_TARGET_NOT_MET`, `RUSTFS_COMBO_FAIL`, and `OZONE_CLEANUP_PASS` were emitted. W05.58/PR-19 records the second same-SHA provider NO-GO boundary without weakening the floor. |
| 2026-09-22 10:46–10:50 UTC (20:46–20:50 AEST) | Retrieved the terminal Ozone/TiDB provider log and classified its hard capacity gate | Hosted provider-capacity evidence | Ozone/TiDB completed 1200/1200 lifecycles with zero timeout/cleanup failures but measured 84.01/1000 IOPS; artifact `10689997396` finalized. W05.57/PR-18 records a provider NO-GO boundary without weakening the floor. |
| 2026-09-22 10:42–10:46 UTC (20:42–20:46 AEST) | Retrieved terminal Native 9P logs and confirmed the root-gated conformance result | Hosted native/platform evidence | Native, N-API, pinned, and root jobs passed at exact `7efded54`; root conformance reports 146/146 tests. The Rust-cache post-save warning is recorded separately; W05.56/PR-17 closes only the Native 9P gate and production remains NO-GO. |
| 2026-09-22 10:38–10:42 UTC (20:38–20:42 AEST) | Retrieved the completed macOS job log directly and classified the final native-artifact upload timeout | Hosted artifact-service diagnosis / release control | All macOS functional gates and the storage benchmark artifact passed; native artifact finalization failed with `ETIMEDOUT` after upload. W05.55/PR-16 records an infrastructure-only boundary; parent CI remains queued and production remains NO-GO. |
| 2026-09-22 10:31–10:38 UTC (20:31–20:38 AEST) | Re-polled the exact candidate after the documentation push and recorded terminal Fault success plus partial W07/W08/Native 9P transitions | Hosted fault/platform/package evidence | Fault `35714146067` passed all three OS jobs; W07 durable and macOS compile passed with assembly queued; W08 Linux/macOS builds passed with asset verification queued; Native 9P substantive jobs passed with root conformance queued; CI remained queued with the isolated macOS artifact-upload failure. W05.54/PR-15 recorded; production remains NO-GO. |
| 2026-09-22 10:16–10:31 UTC (20:16–20:31 AEST) | Rebased the W05 ledger over moving `origin/main` `e984c232`, polled the exact candidate hosted packet, and classified the first macOS job failure | Hosted evidence / release-control documentation | W04 and W08 policy passed; completed macOS functional steps passed before the artifact upload failed; W07/W08 targets remained in progress and Native 9P remained root-queued. W05.53/PR-14 records the boundary; production remains NO-GO and no credentials were read. |
| 2026-09-22 10:01–10:11 UTC (20:01–20:11 AEST) | Created immutable candidate `7efded54`, dispatched the seven non-R2 same-SHA hosted gates, and requalified the current candidate through format, full Rust tests, strict Clippy, dyld-safe N-API build/load, complete Node/N-API suite, and PGlite/Rust/Node/CLI matrix | Release engineering / local production qualification / hosted coordination | All local gates passed with explicit R2/native/provider skips. CI `35714144497`, Fault `35714146067`, W04 `35714141926`, W07 `35714147247`, W08 policy `35714144646`, W08 targets `35714146811`, and Native 9P `35714145176` were queued on exact `7efded54`; production remains NO-GO. |
| 2026-09-22 09:11–10:01 UTC (19:11–20:01 AEST) | Requalified exact pushed `515bdc00` after the macOS Rust/N-API dyld repair, refreshed the standalone provider-matrix lock for `md-5`, reran the full PGlite/Rust/Node/CLI matrix, rebased the lock repair over concurrent `origin/main` `e86c9ccb`, and prepared the W05.51 production ledger update | Package/platform implementation / local release qualification / concurrent-main reconciliation | Darwin addon load, full Rust/Clippy/Node/N-API/PGlite/provider/CLI packet passed; R2/AWS/TiDB/RustFS/native mounts remained explicit gates. New local lock successor `505cbdcf` is ready to publish; a new immutable hosted candidate is still required and production remains NO-GO. |
| 2026-09-22 09:06–09:11 UTC (19:06–19:11 AEST) | Repaired prepared TiDB statement interception, hardened Node 9P failed-listener cleanup, ran locked Rust compile/format checks, JavaScript syntax/diff checks, and four focused N-API 9P loopback repetitions | Integration-test implementation / local qualification | TiDB classifier and Node cleanup slice are locally green; hosted TiDB/TiDB-RustFS, Linux/Windows parity, Ozone capacity, and new immutable-candidate rerun remain open. W05.50 recorded and production remains NO-GO. |
| 2026-09-22 09:00–09:01 UTC (19:00–19:01 AEST) | Retrieved terminal candidate CI logs, classified Rust/Node/TiDB/Ozone failures, patched the Rust 9P blocking-test synchronization race, and ran the shared-target compile check | Hosted failure analysis / implementation repair | Candidate CI `35702348089` is terminal-failed on exact `87f3cdf0`; the Rust test repair compile-checks successfully. Node 9P/Windows timeouts, TiDB prepared-statement interception, and hard Ozone IOPS remain open; W05.49 recorded and production remains NO-GO. |
| 2026-09-22 08:44–08:45 UTC (18:44–18:45 AEST) | Retrieved terminal W07 job/assembly logs and artifact digests on exact candidate `87f3cdf0` | Hosted provider/platform evidence | W07 `35702352395` passed durable FoundationDB/RustFS, macOS feature compilation, and cross-platform evidence assembly; the packet preserves the explicit macOS feature-compile-only boundary and records 466.18 lifecycle IOPS with a workflow-specific minimum of 1. CI remains the only candidate workflow in progress; production remains NO-GO. |
| 2026-09-22 08:38–08:40 UTC (18:38–18:40 AEST) | Re-polled the immutable candidate, captured terminal W08 target/provenance logs, and inspected the non-terminal CI job snapshot | Hosted release/package/provenance evidence | W08 `35702352896` passed Linux/macOS builds, downloaded assets, provenance, CycloneDX SBOM attestations, Rekor publication, repository uploads, and exact-source verification. CI `35702348089` has several failed jobs but is not terminal; W07 `35702352395` remains queued around macOS compile. W05.47 recorded; production remains NO-GO. |
| 2026-09-22 08:32–08:34 UTC (18:32–18:34 AEST) | Re-polled exact-candidate hosted runs and recorded terminal Fault plus W07/W08 partial results | Hosted release-control evidence / ledger maintenance | Fault `35702350927` passed Windows/Ubuntu/macOS. W07 durable job `106663122874` passed while macOS compile `106663122748` stayed queued; W08 builds and macOS asset verification passed while Linux verification `106671605507` stayed queued; CI `35702348089` stayed queued. Ledger updated; production remains NO-GO. |
| 2026-09-22 08:29 UTC (18:29 AEST) | Reviewed terminal Native 9P logs and job conclusions on immutable candidate `87f3cdf0` | Hosted native/platform evidence | Native 9P `35702349894` passed Linux kernel probes, Rust native I/O/lifecycle, N-API server/session/member/identity, automatic/direct/structural mounted I/O, and cleanup. W07 macOS compile, CI, Fault, and W08 target attestation remain open. |
| 2026-09-22 08:19 UTC (18:19 AEST) | Reviewed terminal same-SHA W08 release policy evidence after W04 policy passed | Hosted release-control evidence | W08 policy `35702353241` passed rollout/evidence policy, release identity, manifest, SBOM, and provenance checks. CI/Fault/W07/Native 9P/W08 targets remain open. |
| 2026-09-22 08:16 UTC (18:16 AEST) | Re-polled the immutable packet after hosted scheduling began | Hosted release-control evidence | W04 policy `35702351026` passed; W08 policy `35702353241` started; CI/Fault/W07/Native 9P/W08 targets remain queued. Full production packet remains open. |
| 2026-09-22 08:10–08:11 UTC (18:10–18:11 AEST) | Counted repository-wide queued/in-progress Actions runs and checked the public GitHub status | Hosted capacity evidence | 21 queued, 0 in progress; oldest queued run `35700399264` dates to 07:35:51 UTC. Candidate remains queued; this is a hosted-capacity gate. |
| 2026-09-22 08:06–08:08 UTC (18:06–18:08 AEST) | Audited the hosted queue and GitHub Actions status, then requested cancellation of the older duplicate CI run `35700818889` | Hosted capacity / release-control hygiene | Actions reported operational, but the seven primary candidate runs and the duplicate remained queued; no hosted pass or cancellation was inferred. |
| 2026-09-22 07:59–08:03 UTC (17:59–18:03 AEST) | Rechecked exact-candidate npm package contents and locked workspace licenses; corrected the package-command probe | Local packaging / dependency gate | Both intended npm dry-runs passed; core includes the Darwin arm64 addon and notices, virtual-fs includes declarations/notices, and all 25 workspace packages are Apache-2.0. Cross-platform publication/signing remains open. |
| 2026-09-22 07:53–07:59 UTC (17:53–17:59 AEST) | Verified the immutable candidate ref, inspected workflow dispatch contracts, dispatched the non-R2 same-SHA hosted packet, and captured run IDs | Hosted release-control coordination | Seven primary runs queued on `87f3cdf0`; Live R2, Live AWS, and production-release publication intentionally held behind their explicit gates. |
| 2026-09-22 07:35–07:53 UTC (17:35–17:53 AEST) | Built the optimized N-API artifact and ran the complete pinned-oracle Node/N-API suite plus `scripts/test-pglite.sh` on immutable candidate `87f3cdf0` | Local production qualification / SDK-CLI-package gate | Full local packet passed: Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40×621 traces. Provider credentials, live R2, TiDB/RustFS, FoundationDB opt-in, and privileged native mounts remained explicit skips or external gates. |
| 2026-09-22 11:46–11:59 | Completed the exact integrated `6797a2d8` current-tip packet after the 9P/WebDAV successor: full Rust workspace, strict Clippy, optimized N-API, complete Node suite, real PGlite/SDK/CLI/upstream/oracle matrix | Local production qualification | Full packet passed with Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and 40×621 traces. The isolated pinned oracle required a frozen no-lifecycle install before the clean rerun; no repository credentials or Keychain access were used. |
| 2026-09-22 11:59–12:02 | Refreshed same-SHA hosted Actions and reconciled moving shared mainline | Hosted evidence / concurrent-main reconciliation | On `6797a2d8`, W04 `35677127007`, fault `35677126925`, W08 policy `35677126941`, and W08 targets `35677126934` succeeded; R2 `35677126924` failed and CI `35677127048` was cancelled, so no hosted release packet was promoted. Origin advanced to `819c663e`, which is the next qualification target. |
| 2026-09-22 11:30–11:45 | Re-ran the exact `2d2b66ef` PGlite packet after regenerating the standalone provider-matrix lock for the R2 `tokio` dependency | Local release qualification / lock repair | Full packet passed: Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40×621 traces. The initial stale-lock refusal is retained as a reproducibility defect found and repaired; provider/native opt-ins remain explicit skips. |
| 2026-09-22 11:45–11:46 | Fetched concurrent shared main after the exact packet and reconciled the next current boundary | Release engineering / concurrent-main reconciliation | `origin/main` advanced to `412c422e` with 9P frame-assembler/type parity and hosted WebDAV native concurrent-I/O successors. The tested packet remains exact `2d2b66ef` plus the one-line lock repair; `412c422e` requires fresh full qualification. |
| 2026-09-22 11:05–11:28 | Ran the full real `scripts/test-pglite.sh` packet on exact `9cb2eef1` with an isolated Cargo target and permitted loopback binds | Local SDK/CLI/provider-matrix qualification | Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40 five-seed/eight-backend oracle traces at 621 operations passed. R2/TiDB/RustFS cases remained explicit credential/service-gated skips; no provider acceptance was inferred. |
| 2026-09-22 11:28–11:30 | Fetched and fast-forwarded concurrent shared main through `1670ceba` before the next ledger checkpoint | Release engineering / concurrent-main reconciliation | `1670ceba` includes the 9P dirent-packer and S3 N-API session-concurrency successors plus W07/WebDAV documentation. The exact local packet remains anchored at `9cb2eef1`; the newer tip requires fresh qualification, and production remains NO-GO. |
| 2026-09-22 10:49–10:54 | Reconciled concurrent main from `db431f4c` through `bed4aacd` to `1bdd6adf`; qualified the four-part S3 multipart-ordering successor, then the FUSE busy-unmount successor | Concurrent-main release qualification | `bed4aacd` gateway format and 28/28 S3 gateway tests passed. Exact `1bdd6adf` then passed format, 61/61 non-native FUSE package tests, and strict package Clippy; current full packet remains anchored at `db431f4c` until the FUSE delta is requalified end to end. |
| 2026-09-22 10:54–11:02 | Monitored exact `03cca165` hosted workflows and reconciled the moving docs/site-only mainline through `ff29dad7` | Hosted evidence / concurrent-main reconciliation | Native 9P `35673957522` passed both jobs. CI, fault, W04, and W08 runs were cancelled by subsequent pushes; no cancelled or revision-mismatched run was promoted. Current production decision remains NO-GO. |
| 2026-09-22 10:21–10:30 | Rebased the committed N-API P9 absence-type fix over concurrent `origin/main`; upstream already contained the equivalent implementation as `730a3de4`, so the resulting checkout resolved exactly to `db431f4c` and the push was safely a no-op | Release engineering / concurrent-main reconciliation | `HEAD == origin/main == db431f4c`; no duplicate code commit was created, and the equivalent clean-build declaration fix is present in the shared mainline. |
| 2026-09-22 10:39–10:44 | Ran the exact pushed `db431f4c` real PGlite qualification with an isolated Cargo target and loopback permission | Local SDK/CLI/provider-matrix qualification | Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40 five-seed/eight-backend traces at 621 operations passed; live R2/TiDB/RustFS rows were explicit skips because credentials/services were absent. |
| 2026-09-22 10:44–10:49 | Ran exact-current format, locked Rust workspace tests, strict Clippy, optimized N-API build, and complete Node/N-API suite | Local production qualification / package gate | All passed on `db431f4c`, including P9 optional declarations, WebDAV direct/network/provider concurrency, crash/restart and in-flight recovery, CLI, distribution, and artifact aggregation. Native mount and live provider rows remain explicit host/provider gates. |
| 2026-09-22 09:35–10:21 | Fetched the moving shared mainline, confirmed the previously qualified W05 implementation is contained in current `origin/main` `8ca6c257`, and ran the exact current full packet: format, locked Rust workspace with loopback permission, strict Clippy, optimized N-API build, complete Node SDK/CLI suite, real PGlite matrix, provider matrix, upstream oracle, and trace parity | Local production qualification / release engineering | Exact current local packet passed. Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and 40×621 traces passed; current WebDAV provider-concurrency and in-flight crash/restart tests also passed. Hosted/provider/native/package/scope gates remain open. |
| 2026-09-22 10:21 | Ran the provider-workflow path helper and read-only AWS/OIDC audit; refreshed the W05 ledger with the exact current SHA, production addendum, evidence boundary, remaining actions, estimates, and blockers | Hosted/provider/security boundary / documentation | Workflow path helper passed both workflow files. AWS remained fail-closed for the exact missing protected inputs; no secret value was read, stored, or accessed through Keychain. Ledger update is the next commit/push chunk; production decision remains NO-GO. |
| 2026-09-21 06:56–07:11 | Workflow `35570596593` queued and budget job admitted run `2/20` | Hosted wait / cost gate | Budget passed; live job scheduled. |
| 2026-09-21 07:15–07:25 | Hosted setup, locked installs, public Node addon build, Rust backend, PGlite SDK/CLI and upstream tests | Hosted gate | All reported passing before the later cancellation. |
| 2026-09-21 07:25–07:31 | Hosted trace parity and cancellation diagnosis | Hosted failure analysis | Five-seed local/PGlite trace passed; redundant remote trace was canceled mid-second R2 seed; no assertion failure was reported. |
| 2026-09-21 07:32–07:35 | Retrieved redacted logs and identified duplicated remote/local trace work | Active engineering | Chosen fix: R2-only bounded trace plus shared Cargo wrapper. |
| 2026-09-21 07:35–07:36 | Patched shell/Node helpers; ran shell syntax, Node syntax, and diff checks | Implementation | Checks passed. |
| 2026-09-21 07:36–07:38 | Committed, fetched, rebased over remote main, and pushed `d4f7281` | Release engineering | New workflow `35573697664` created. |
| 2026-09-21 07:39–07:48 | Created and pushed the initial ledger as `c773294`; reviewed the hosted queue and redacted logs | Documentation / hosted wait | Replacement run was still the open acceptance action. |
| 2026-09-21 08:00–08:23 | Inspected diagnostic run `35575940720`; retained redacted stderr and reproduced the exact hosted `ESTALE` shutdown failure | Hosted failure analysis | Classified the defect as expired-lease shutdown handling, not a credential or provider-auth failure. |
| 2026-09-21 08:23–08:33 | Patched `ChunkedFs::shutdown`, added the expired-unfenced-lease regression test, ran focused Rust tests, rebased, and pushed `c71c8ee` | Implementation / release engineering | Focused test passed; replacement hosted run queued. |
| 2026-09-21 08:32–08:44 | Hosted run `35577687152` executed the full packet | Hosted provider/native gate | Budget, SDK/CLI matrices, trace, live CLI, N-API, service evidence, and artifact all passed. |
| 2026-09-21 08:44–09:01 | Removed shell command-substitution warnings from the budget summary, ran syntax/diff checks, rebased, and pushed `3db491e` | Implementation / release engineering | Final authoritative run queued at the corrected head. |
| 2026-09-21 09:01–09:16 | Final hosted run `35579757447` completed and logs/artifact were reviewed | Hosted provider gate / evidence | Budget marker `12/20`, `$80` envelope, full W05 packet, and artifact `10630468958` passed. |
| 2026-09-21 09:16–09:23 | Answered the production-readiness question from current tracker and live GitHub Actions state | Release status review | W05 is green, but whole-repository status remains NO-GO; current main had queued CI/fault-injection work and open W20/platform gates. |
| 2026-09-21 09:23–09:31 | Created the continuation goal and designed the production-readiness dependency register | Goal setup / planning | PR-00 through PR-10 now enumerate implementation, hosted, native, provider, packaging, scope, and final-audit work. |
| 2026-09-21 09:31–09:35 | Ran isolated full locked workspace tests after clearing shared-target collisions | Local production gate | All workspace targets passed; live-provider/native opt-ins remained explicit skips. |
| 2026-09-21 09:35–09:38 | Ran strict locked workspace Clippy | Local production gate | Found one real `clippy::single_match` failure in TiDB ambiguous-commit test. |
| 2026-09-21 09:38–09:43 | Replaced the single-pattern match, ran format, targeted Clippy/test checks, committed/rebased/pushed `2e66f94` | Implementation / release engineering | Targeted checks passed; fresh CI, Live R2, and fault-injection workflows started. |
| 2026-09-21 09:43–09:51 | Watched hosted CI `35585066458` at `9c098e5`; retrieved redacted Windows diagnostics for Rust Clippy dead code and the Node `windows`/`win32` probe mismatch | Hosted failure analysis | Classified two implementation defects; Linux Rust, Linux native FUSE/NFS/9P/WebDAV, TiDB, Ozone, RustFS and several Node paths passed before the workflow finished. |
| 2026-09-21 09:51–09:55 | Patched the Unix-only HTTP test fields/helpers and normalized the Windows auto-probe platform name; ran format, targeted strict Clippy and 15 N-API unit tests; committed/pushed `bb27fe0` | Implementation / release engineering | Local repair gates passed; hosted run `35585966602` was cancelled before execution by a superseding push. |
| 2026-09-21 09:55–10:00 | Rebased onto concurrent `b7e2758` and `81ccc86` main updates; recorded current CI/live-R2/fault-injection run IDs | Release ledger / hosted wait | Current revision is `81ccc86`; goal remains active and release decision remains NO-GO pending unsuperseded hosted qualification and the remaining production register. |
| 2026-09-21 10:00–10:09 | Advanced through concurrent AWS/FoundationDB/W08 changes; ran the current locked workspace gates and diagnosed the first `http_subprocess` `EEXIST` failure as a concurrent temporary-root collision | Local production gate / failure analysis | Clippy passed; the full test gate failed only at test-directory creation, with no product assertion failure. |
| 2026-09-21 10:09–10:20 | Installed pinned Node/PGlite/oracle dependencies, rebuilt the N-API release artifact, and reran the PGlite/Rust/Node/CLI matrix plus the full pinned-oracle Node package suite | Local SDK/CLI/package gate | PGlite matrix passed Rust SDK `6/0/0`, Node SDK `5/0/0`, CLI `12/0/0`; full Node suite passed; R2/TiDB/RustFS/native opt-ins remained explicit skips. |
| 2026-09-21 10:20–10:27 | Fixed repeatable N-API declaration loss in `postbuild.mjs`, synced generated docs, verified a clean release-build diff, committed/pushed `d735814` | Implementation / packaging | Runtime-exported FUSE dirent declarations now survive clean release builds; local package build and tests passed. |
| 2026-09-21 10:27–10:37 | Added the atomic temporary-root sequence, reran focused/full Rust tests and strict Clippy, committed/rebased/pushed `f98cc7d`, and reviewed current hosted failures | Implementation / release engineering / hosted gate | Full Rust workspace and Clippy passed; R2 `35589628621` failed closed at `count=29/20`; AWS `35589628578` failed on missing security inputs; current CI remained pending and fault injection queued. |
| 2026-09-21 10:37–10:42 | Updated the production-readiness register and pushed the ledger chunk as `ad8c3ee` after rebasing onto concurrent `origin/main` work | Documentation / release engineering | The W05 ledger now records every mapped production dependency, the current revision, exact hosted blockers, provisional estimates, and the session log. |
| 2026-09-21 10:42–10:47 | Ran W20.5 local package and dependency checks | Local packaging / dependency gate | Both npm package dry-runs passed with license/notices/native-artifact contents; Cargo normal dependency inventory produced 719 lines and all 25 workspace packages reported Apache-2.0. Virtual-fs frozen reinstall passed lockfile policy but could not resolve `registry.npmjs.org` in the sandbox, so typecheck/install remains an external gate. |
| 2026-09-21 10:47–10:53 | Added the explicit pnpm 11 fail-closed native-build policy, clean-reinstalled all 233 virtual-fs packages, ran typecheck and just-bash/Mastra tests, committed `af59104`, rebased, and pushed `bc37ff7` | Implementation / packaging / release engineering | Clean install and integration gates passed; current-head R2 `35591121940` refused safely at `count=32/20`, while CI `35591121950` remained pending and fault injection `35591121942` remained in progress. |
| 2026-09-21 10:53–11:28 | Reconciled the shared `origin/main` advances, reviewed hosted Rust/Node HTTP failures, and isolated the nested streaming-publication defect from the provider-cap refusal | Hosted failure analysis / implementation | The 404 nested PUT, 501 test-driver rename, and Node HTTP parity mismatch were classified as one shared staging/publication path defect; no additional live R2 attempt was made. |
| 2026-09-21 11:28–11:39 | Added destination-parent creation for streaming and multipart publication, made root-parent creation a no-op, delegated the flaky-driver rename, ran S3 gateway, R2 interop, Node HTTP parity, format, and Clippy checks, committed and pushed `39a19fd` | Implementation / local release gate | S3 gateway `13/13`, R2 HTTP interop `2/2`, Node TypeScript/Rust HTTP differential `40` paired cases, and strict Clippy passed. |
| 2026-09-21 11:39–11:43 | Reconciled concurrent shared commits through `7130438`, verified the current R2 cap refusal and AWS security-input failure, and reviewed the current fault/CI/W07 runs | Release engineering / hosted evidence | Fault injection `35595466979` passed on Linux/macOS/Windows; full CI `35595466940` and stable W07 qualification `35595483259` remained in progress; R2 `35595383628` refused at `44/20`. |
| 2026-09-21 11:43–12:07 | Repaired Windows long-path error handling, RustFS endpoint admission, and staged-upload retention/mtime behavior; ran format, host/R2/S3 focused tests, and strict Clippy | Implementation / local release gate | Host tests `10/10`, R2 tests `15` total including HTTP interop `2/2`, S3 gateway `14/14`, and targeted Clippy passed; local Windows target was unavailable. |
| 2026-09-21 12:07–12:12 | Rebased the implementation chunk over shared main and pushed `56dfcb2`; reviewed the R2 cap and AWS protected-input failures | Release engineering / hosted evidence | R2 `35597712944` refused at the cap, AWS `35597712935` stopped at missing inputs, and full CI/fault runs were superseded by concurrent pushes. |
| 2026-09-21 12:12–12:20 | Reconciled shared fixes `9b396c2`, `65c52b9`, and `82ffeb9`; reviewed cancellation behavior, AWS preflight `35598843178`, R2 cap `35598843157`, Ozone qualification, and the older W07 run | Hosted failure analysis / production planning | AWS failed closed at `missing_bucket`; R2 remained safely denied at `55/20`; current-head CI/fault workflows were queued after the shared updates. |
| 2026-09-21 12:20–12:21 | Updated the W05 production-readiness ledger with current-head evidence, all remaining work, estimates, blockers, and the active completion goal | Documentation / release engineering | Ledger is ready for a documentation commit and push; production decision remains NO-GO. |
| 2026-09-21 12:21–12:30 | Fetched and fast-forwarded over concurrent shared pushes to `b26819e`, restored the ledger, and reviewed live Actions state plus completed W07 evidence | Release engineering / hosted evidence | Current CI `35599817215` is queued, fault injection `35599817297` is in progress, and W07 `35598049389` succeeded only on older `65c52b9`; ledger is corrected before commit. |
| 2026-09-21 12:30–12:43 | Repaired the provider-matrix `--locked` dependency edge by adding the declared `futures-util` lock entry, then reran the complete PGlite harness | Implementation / local provider gate | Cleanup fail-closed checks, real PGlite Rust tests, N-API PGlite/factory/chunked/FUSE, Rust SDK `6/0/0`, Node SDK `5/0/0`, CLI `12/2`, upstream `1200/82`, and trace parity passed. |
| 2026-09-21 12:43–13:10 | Ran current shared-head formatting, focused/full Rust tests, and strict Clippy; rebuilt the release N-API artifact | Local production gate / packaging | Focused packages, full workspace, and strict Clippy passed; release N-API build completed without generated-file drift. |
| 2026-09-21 13:10–13:28 | Ran the pinned-oracle N-API package suite and current-head PGlite matrix on `421fe9b`, then reconciled shared main to `9ee985e` and reran affected Rust gates | Local SDK/CLI/package gate / release engineering | Complete Node suite and PGlite/Rust/Node/CLI/oracle packet passed; missing oracle configuration was caught once and corrected without changing product code. |
| 2026-09-21 13:28–13:39 | Rebuilt and reran the N-API suite and full PGlite harness on `9ee985e`; fetched through concurrent W08/AWS updates to `98243d2` | Local provider/package gate / hosted evidence | `9ee985e` N-API and PGlite paths passed; hosted fault `35607523296` passed only on older `88ecee2`, CI `35607523533` was in progress there, and R2 `35607523280` failed closed at the cap. |
| 2026-09-21 13:39–13:58 | Revalidated the newest shared `98243d2` with format, full locked Rust workspace tests, and strict Clippy; updated this ledger with W05.13, current evidence, provisional estimates, and all remaining production gates | Local production gate / documentation | Current shared local Rust qualification passed. The ledger is ready to commit with the tested provider-matrix lock repair; production decision remains NO-GO because hosted/provider/native/security/package/scope gates remain open. |
| 2026-09-21 13:58–14:08 | Reconciled `origin/main` through `d619f93`; completed the five-seed PGlite trace (`1200/82`, 40 combinations, 621 operations), reran the complete d619f93 N-API suite with loopback permission, and updated the ledger with exact SHA boundaries | Release engineering / local SDK and package gate | d619f93 adds test/docs/lockfile changes only after the c719306 Rust qualification. N-API bounded-unstorage parity, all package/artifact/lifecycle paths, and artifact aggregation passed; the native mount remains explicitly unavailable. Ledger is ready to push as the next shared documentation chunk; production decision remains NO-GO. |
| 2026-09-21 14:08–14:12 | Reconciled the latest concurrent release/OIDC updates through `c7aa4f2` and refreshed current-head references | Release engineering / documentation | c7aa4f2 adds CLI-release smoke and secret-safe AWS OIDC audit workflow/docs only after the d619f93 N-API rerun; no product-code qualification claim is extended beyond the tested revisions. |
| 2026-09-21 14:12–14:30 | Rebuilt the release N-API artifact and ran the complete pinned-oracle Node package suite on `a7895a8` | Local SDK/CLI/package gate | Typecheck, harness, smoke/contract, factories, lifecycle, codecs, servers, JS driver, bounded-unstorage parity, differential, restart, distribution, and artifact aggregation passed; native mount and live-provider opt-ins remained explicitly classified. |
| 2026-09-21 14:30–14:33 | Ran `scripts/test-pglite.sh` on `a7895a8` with the pinned MountX oracle and PGlite source | Local Rust/Node/CLI/provider gate | Rust SDK `6/0/0`, Node SDK `5/0/0`, CLI `12/2`, upstream `1200 passed / 82 skipped`, and all 40 seed/backend combinations passed with 621 operations each; R2/TiDB/RustFS live rows were explicit skips. |
| 2026-09-21 14:33–14:34 | Fetched and fast-forwarded shared main from `a7895a8` to `4756256`; validated the successor FoundationDB scripts and ran the read-only AWS/OIDC audit | Release engineering / security-provider gate | `bash -n`, Node syntax, and the W07 qualification fixture passed. The audit safely reported the missing protected AWS environment policy, variables, and role secret; no credential value was read or persisted. Current R2 `35612779619` is cap-denied, AWS `35612723255` is preflight-denied, and W07 `35612812887` remains in progress. |
| 2026-09-21 14:34–15:09 | Added and locally qualified the N-API S3 drain-timeout surface; ran the release N-API suite and PGlite matrix on `9c9d0e4`; committed/rebased/pushed `de6514c` after preserving concurrent main changes | Implementation / local SDK-CLI-package gate / release engineering | The N-API build and suite passed, then the PGlite matrix passed Rust SDK `6/0/0`, Node SDK `5/0/0`, CLI `12/2`, upstream `1200/82`, and 40 trace combinations; `de6514c` is on `origin/main`. |
| 2026-09-21 15:09–15:23 | Reconciled concurrent shared pushes through `d7ccdc7`; reran the full locked Rust workspace with loopback permission, strict isolated Clippy, optimized N-API build/suite, and `scripts/test-pglite.sh` on tested ancestor `3b0d2fd` | Local production gate / hosted evidence | All current-ancestor local gates passed. Rust workspace and Clippy are green; N-API/package and PGlite Rust/Node/CLI/oracle matrices are green with Rust `6/0/0`, Node `5/0/0`, CLI `12/2`, upstream `1200/82`, and 40×621 trace passes. The current `d7ccdc7` successor adds `0c4d5cb` after this evidence boundary. |
| 2026-09-21 15:13–15:23 | Reviewed W07 predecessor `35616476141` and redacted failure output; retained the exact `mount(2) ... Invalid argument` native blocker and tracked W07 `35617054948` on `de6514c` | Hosted/provider failure analysis | PGlite and RustFS setup succeeded; the privileged FUSE CLI mount failed before provider acceptance. Shared `0c4d5cb` is the targeted safety-flag repair; a post-fix W07 run remains required. |
| 2026-09-21 15:23–15:35 | Inspected `aa3dae3`, ran the privileged FUSE unit suite `14/14`, strict FUSE Clippy, format, and polled W07 `35620006731` | Local native gate / hosted wait | The helper-metadata repair passes locally; W07 remains in progress on `aa3dae3` and is not yet acceptance evidence. |
| 2026-09-21 15:35–15:44 | Ran the full locked Rust workspace and strict workspace Clippy on `aa3dae3`; rebuilt the optimized N-API artifact, ran the complete Node suite, and ran the full PGlite matrix | Local production gate | All local gates passed; Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and 40×621 trace combinations passed. Native/live-provider skips remain explicit. |
| 2026-09-21 15:44–15:52 | Fast-forwarded over concurrent shared changes to tested revision `21666ef`; reran format, full Rust/clippy, optimized N-API build/suite, and PGlite matrix; refreshed hosted run state | Release engineering / local SDK-CLI-package gate | `21666ef` is locally green across Rust, Node, SDK, CLI, PGlite, N-API, package, and oracle paths. W07 `35620006731` remains in progress on `aa3dae3`; R2/AWS preflights and newer CI/W08/fault runs remain non-green or non-terminal. |
| 2026-09-21 15:52–16:15 | Diagnosed the current-head N-API typecheck failure, patched repeatable `P9AttachOptions` declaration insertion in `postbuild.mjs`, rebuilt the release artifact, ran the full Node/N-API suite, and reran `scripts/test-pglite.sh` | Implementation / local SDK-CLI-package gate | The fixed `1acc15f` working tree passed clean N-API generation, full Node suite, Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40×621 trace combinations; committed/rebased/pushed as `90df85b`. R2 `35624137705` then refused safely at `118/20`; concurrent CI/W08/fault runs were superseded while `origin/main` advanced to `2a711e9`. |
| 2026-09-21 16:15–16:29 | Requalified shared revision `f0129fd` after concurrent 9P/FUSE/WebDAV/AWS changes, rebuilt the N-API release artifact, ran the full Node suite and PGlite matrix, inspected W07 completion, and refreshed the generated P9 peer declaration | Local production gate / hosted provider evidence / packaging | Format, full locked Rust workspace, strict Clippy, N-API build/suite, PGlite Rust/Node/CLI/oracle matrix, and 40×621 traces passed on `f0129fd`; W07 `35623069365` completed successfully on `1acc15f` with artifact `foundationdb-production-qualification-35623069365-1`; the generated declaration refresh was pushed as `dca6990`. R2 `35625592337` refused at `123/20`; AWS `35625592317` stopped at `missing_bucket`; current dca CI/W08/fault runs were pending. |
| 2026-09-21 16:29–16:55 | Rebased onto concurrent shared main through `b381b89`, reran format/full locked Rust/strict Clippy, rebuilt the optimized N-API artifact, ran the full Node suite, and ran the PGlite matrix | Local production gate / implementation repair / release engineering | Rust, Clippy, N-API, Node, and oracle paths passed; the first PGlite matrix run exposed the missing standalone `mount-rs-fuse -> futures-util` lock edge, then the repaired `9f1d3dc` rerun passed Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40×621 traces. Committed/pushed `9f1d3dc`; latest observed shared main is `12ba117`. R2 `35628414156` failed safely at `139/20` before secret admission; current hosted workflows were revision-mismatched/non-terminal. |
| 2026-09-21 17:07–17:12 | Completed the exact `e9a6b25` qualification packet, fetched concurrent shared-main work, and fast-forwarded the clean checkout to `097ed00` | Local production gate / release engineering / ledger maintenance | Exact `e9a6b25` passed full locked Rust, strict Clippy, optimized N-API build, full Node/N-API suite, real PGlite lifecycle/rollback, Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40×621 trace combinations. `097ed00` is now the shared tip and contains post-boundary W01/W04/W08/W25/W26/FUSE changes; it is the next local qualification target. Ledger update is the next documentation chunk to commit and push. |
| 2026-09-21 17:12–17:24 | Qualified exact pushed candidate `caf88ba` after its ledger push, including the sandbox-permission reruns, optimized N-API package, full Node suite, and real PGlite matrix; then fast-forwarded to shared tip `1775895` | Local production gate / release engineering / ledger maintenance | `caf88ba` passed formatting, full locked Rust workspace with loopback permission, strict Clippy, optimized N-API build, full Node/N-API suite, PGlite lifecycle/rollback/reconnect/VFS, Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40×621 oracle traces. The first unprivileged Rust/Node attempts were correctly classified as sandbox `bind`/`spawn` restrictions. `1775895` adds post-boundary 9P N-API metadata, NFS v4 lock-cap, FUSE, and W26 changes and is the next qualification target. |
| 2026-09-21 17:24–17:38 | Qualified exact pushed candidate `3109a2e`, recovered the scratch volume after the bounded no-space linker failure, and fast-forwarded to shared tip `0398d94` | Local production gate / release engineering / ledger maintenance | `3109a2e` passed format, full locked Rust workspace with loopback permission, strict Clippy, optimized N-API build, full Node/N-API suite including P9 session metadata, real PGlite lifecycle/rollback/reconnect/VFS, Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40×621 oracle traces. The first Rust link attempt failed only at scratch-volume exhaustion; a fresh bounded target passed. `0398d94` adds post-boundary NFS session-refusal replay, FUSE interrupt-validation context, AWS rollout evidence, and W26 Ozone production-contract checks and is the next qualification target. |
| 2026-09-21 17:38–17:48 | Qualified exact pushed candidate `23a32a1` with format, full locked Rust workspace, strict Clippy, optimized N-API build, complete Node/N-API suite, and the full PGlite matrix; then fast-forwarded to shared tip `cf18d93` | Local production gate / release engineering / ledger maintenance | `23a32a1` passed the complete packet: PGlite reconnect/versioning/VFS/lifecycle/split-store/FUSE, Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40×621 oracle traces. The current `cf18d93` successor adds post-boundary 9P lock/session, FUSE native callback, WebDAV, AWS rollout, and W26 Ozone changes and is the next qualification target. |
| 2026-09-21 17:48–17:58 | Qualified exact pushed candidate `3e98a80` with format, full locked Rust workspace, strict Clippy, optimized N-API build, complete Node/N-API suite, and the full PGlite matrix; then observed shared main advance to `d2db74d` | Local production gate / release engineering / ledger maintenance | `3e98a80` passed the complete packet, including 9P lock/constants coverage: PGlite reconnect/versioning/VFS/lifecycle/split-store/FUSE, Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40×621 oracle traces. The current `d2db74d` successor adds post-boundary FUSE session framing, streamed S3 gateway-byte accounting, W25/W26/AWS/Ozone updates and is the next qualification target. |
| 2026-09-21 17:58–18:07 | Qualified exact pushed candidate `2101e55` with format, full locked Rust workspace, strict Clippy, optimized N-API build, complete Node/N-API suite, and the full PGlite matrix; then observed shared main advance to `ce7b365a` | Local production gate / release engineering / ledger maintenance | `2101e55` passed the complete packet, including 9P lock/constants coverage: PGlite reconnect/versioning/VFS/lifecycle/split-store/FUSE, Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40×621 oracle traces. The current `ce7b365a` successor adds post-boundary FUSE read-abort/session framing, streamed S3 gateway-byte accounting, AWS qualification, and W26 updates and is the next qualification target. |
| 2026-09-22 04:08–04:20 | Qualified exact code revision `0672b7c7` with format, full locked Rust workspace, strict Clippy, optimized N-API build, complete Node/N-API suite, and the full PGlite matrix; fetched shared-main documentation successors | Local production gate / release engineering / ledger maintenance | `0672b7c7` passed the full packet: Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, all 40×621 trace combinations, P9 lock/constants and FUSE blocked-read unmount coverage. The shared `e4d71fcb` successor contains only AWS documentation updates; hosted/provider/native/package/scope gates remain open. |
| 2026-09-22 04:20–04:33 | Requalified shared revision `4dd4f1c2` after W01 FUSE/NFS/AWS/W08 changes with format, full locked Rust workspace, strict Clippy, optimized N-API build/suite, and the complete PGlite matrix | Local production gate / release engineering | `4dd4f1c2` passed all local gates, including syncfs barrier/FUSE, NFS identity, AWS diagnostics, Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40×621 traces. Subsequent shared WebDAV code was not promoted into this evidence row. |
| 2026-09-22 04:33–04:39 | Fast-forwarded to and requalified shared revision `87034818`, including WebDAV request-error callbacks/stats and generated N-API updates | Local production gate / release engineering | `87034818` passed format, the full locked Rust workspace, strict Clippy, optimized N-API build, complete Node/N-API suite, PGlite lifecycle/VFS/FUSE, Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40×621 oracle traces. Current `8201ae35` is a documentation-only successor; hosted/provider/native/package/scope gates remain open. |
| 2026-09-22 04:39–04:54 | Qualified exact shared code revision `cd6dea28` with format, full locked Rust workspace, strict Clippy, optimized N-API build, complete Node/N-API suite, and the real PGlite matrix; then fetched concurrent main work | Local production gate / release engineering | `cd6dea28` passed the complete packet, including chunked immutable-write overlap and FUSE syncfs codec coverage, Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, all 40×621 traces, and fail-closed cleanup injection. The clean checkout was fetched/fast-forwarded to current shared `d4de2e75`, whose W04 split-store lease-TTL/configuration changes are outside this evidence boundary. |
| 2026-09-22 04:54–04:55 | Refreshed live Actions state and updated this ledger with the exact `cd6dea28` evidence, current `d4de2e75` successor, hosted run statuses, remaining production actions, and blockers | Release engineering / documentation | W04 policy `35641426233` was terminal-successful; current R2/AWS were in progress and CI/fault/W08 were queued/pending at observation time, so no current-head hosted release acceptance was promoted. Production decision remains NO-GO; this ledger update is the next commit/push chunk. |
| 2026-09-22 04:55–05:13 | Requalified pushed revision `57733d7c` with format, full locked Rust workspace, strict Clippy, optimized N-API build, complete Node/N-API suite, and the PGlite/Rust/Node/CLI/oracle matrix; recovered a bounded `/private/tmp` no-space failure and completed a fresh rerun | Local production gate / release engineering | `57733d7c` passed the complete packet: Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, all 40×621 traces, WebDAV SQLite reopen, W04 lease-TTL validation, chunked concurrency, FUSE syncfs/forced cancellation, and fail-closed cleanup injection. Only task-owned temporary Cargo targets were removed to recover space; no repository or credential data was touched. |
| 2026-09-22 05:13–05:14 | Fetched and fast-forwarded concurrent main through `b3a0a92d` and later shared successors, then refreshed the ledger boundary before the current-tip packet | Release engineering / documentation | Current R2 `35645426002` failed closed at the usage envelope, W04 policy `35645425988` succeeded, and W08/CI/fault/W07 runs were queued, pending, or in progress at observation time; none qualified the release. Production decision remained NO-GO; this was the prior documentation push chunk. |
| 2026-09-22 05:14–06:02 | Completed the exact `d8971ed8` Node/PGlite qualification, including the optimized N-API build, full Node suite, Rust/Node/CLI matrices, upstream/oracle packet, and trace parity; recovered a linker no-space failure by removing only explicitly completed W05 temporary targets and reran from the preserved isolated target | Local production gate / release engineering | The first PGlite attempt failed only at the linker with `No space left on device`; the fresh rerun passed Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40×621 traces. No repository, credential, or unrelated temporary data was touched. |
| 2026-09-22 06:02–06:14 | Fetched/fast-forwarded to current shared `20fb445a` and ran overlapping format, full locked Rust workspace, strict Clippy, optimized N-API build, complete Node suite, and full PGlite matrix | Local production gate / release engineering | Current shared `20fb445a` passed the complete local packet: Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40×621 traces. Native/live-provider rows remain explicit skips. |
| 2026-09-22 06:14 | Refreshed live Actions and updated this ledger for the current tip | Hosted evidence / documentation | Listed head `8ddf48f` had W04 success, fault in progress, W08 targets pending, W08 policy in progress, and CI pending; no terminal hosted result for exact `20fb445a` was listed. Production decision remains NO-GO; this is the next documentation commit/push chunk. |
| 2026-09-22 06:14–06:16 | Inspected the workflows triggered by pushed ledger revision `95437f67` and reran the read-only AWS/OIDC audit | Hosted evidence / security boundary | CI/W04/W08/fault runs for `95437f67` are queued or pending. The audit fail-closed with `invalid_role_arn_shape`, unreadable GitHub repository/environment metadata, and missing bucket/region/account/versioning/role inputs; no credential value was read. |
| 2026-09-22 06:21–06:25 | Qualified the pushed WebDAV successor `74d386dd` carried by `0a2cf5df` with format, focused strict Clippy/tests, optimized N-API rebuild, and the complete Node SDK/CLI suite | Local regression gate / release engineering | All 18 WebDAV tests passed, including timed-out drain-state preservation; the optimized N-API build and complete Node suite passed with WebDAV lifecycle, SQLite reopen, S3 restart recovery, 9P, FUSE, differential, distribution, and artifact aggregation coverage. |
| 2026-09-22 06:25–06:34 | Qualified the pushed NFS successor `3589a142` carried by `816107f6` with format, focused strict Clippy/tests, optimized N-API rebuild, and the complete Node SDK/CLI suite | Local regression gate / release engineering | All 38 NFS Rust tests passed, including owner/group callback and injected-clock coverage; the optimized N-API build and complete Node suite passed with the new NFS callback path plus existing WebDAV, S3, 9P, FUSE, differential, distribution, and artifact aggregation coverage. A fresh full PGlite packet is intentionally not claimed for this changed surface. |
| 2026-09-22 06:34–06:41 | Requalified exact origin/main `4fde3326` with format, full locked Rust workspace, strict Clippy, optimized N-API build, and complete Node/N-API suite; reran Rust with loopback permission after the sandbox denied the 9P TCP listener | Local production gate / release engineering | Full Rust and Node packets passed; the initial sandbox-only Rust attempt failed at the expected `Operation not permitted` bind boundary, then the permitted rerun passed all non-opt-in tests. |
| 2026-09-22 06:41–06:45 | Started the real PGlite matrix on `4fde3326` and recovered the host from completed W05 build-target pressure | Local production gate / environment recovery | PGlite provider/reconnect/versioning/VFS/lifecycle stages passed; the first run stopped in a later SDK/CLI compile at `No space left on device`. Only explicitly enumerated completed W05 temporary targets were removed; repository and credential data were untouched. |
| 2026-09-22 06:45–06:56 | Reran the complete `scripts/test-pglite.sh` packet on exact `4fde3326` from a fresh target with loopback permission | Local production gate / release engineering | Full packet passed: Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40 five-seed/eight-backend traces at 621 operations. R2/AWS/TiDB/RustFS/native rows remain explicit provider/platform skips. |
| 2026-09-22 06:56–06:57 | Refreshed live GitHub Actions and reconciled concurrent shared-main advancement | Hosted evidence / release engineering | Origin/main advanced to `91af2043`; its R2, W04, fault, W08, and CI workflows are queued or pending, while the preceding `595c5c85` fault run succeeded and CI/W08 targets were cancelled during supersession. No terminal hosted result qualifies the tested `4fde3326` or new `91af2043`. |
| 2026-09-22 06:57–07:10 | Qualified the actual shared tip `997fafa0` after concurrent WebDAV/chunked/W07/W08 changes with format, full locked Rust workspace, strict Clippy, optimized N-API build, and complete Node/N-API suite | Local production gate / release engineering | Full Rust and Node packets passed, including chunked atime coalescing, WebDAV close-timeout handling, NFS shutdown cancellation, and the expected live/native opt-in skips. |
| 2026-09-22 07:10–07:12 | Ran the complete real PGlite matrix on exact `997fafa0` and refreshed live Actions after shared-main advancement | Local production gate / hosted evidence | Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40 five-seed/eight-backend traces passed. Origin/main then advanced to `c45f3931`; R2 `35655588838` is queued, W04 `35655588659` succeeded, fault `35655588554` is queued, W08/CI are pending, and no terminal hosted result qualifies `997fafa0` or `c45f3931`. |
| 2026-09-22 07:12–07:30 | Qualified exact tested `dff99567` with format, full locked Rust workspace, strict Clippy, optimized N-API build, complete Node/N-API suite, and the full PGlite matrix; refreshed shared-main and hosted Actions state | Local production gate / hosted evidence | Rust workspace and Clippy passed; the optimized N-API artifact and full Node SDK/CLI suite passed; PGlite passed Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40 five-seed/eight-backend traces at 621 operations. The current remote advanced to `d11101c0`, which includes S3 cancelled-staging cleanup, streamed WebDAV requests, bounded FUSE frames, and W07/W01/W08 ledger changes; its CI/W08/fault runs are pending and no terminal hosted result qualifies either SHA. |
| 2026-09-22 07:30–08:04 | Requalified pushed candidate `15ce42a0`, diagnosed the current-tip PGlite `--locked` failure, regenerated and pushed the one-edge provider-matrix lock repair as `83fde1b4`, reran the full PGlite matrix, and refreshed live Actions | Local production gate / implementation repair / hosted evidence | Full Rust, strict Clippy, optimized N-API, and complete Node SDK/CLI packets passed on code-identical `15ce42a0`; the first current-tip PGlite run exposed the missing direct `sha2 0.10.9` lock edge, then `83fde1b4` passed the full matrix exactly: Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40 five-seed/eight-backend traces at 621 operations. Origin/main advanced to `6a19ff65` with WebDAV session-member differential coverage; its hosted R2 run failed closed, fault/W04 succeeded, and CI/W08 remain non-terminal. |

| 2026-09-22 08:04–08:23 | Requalified exact `6f29d9ce` across format, the full locked Rust workspace, strict Clippy, optimized N-API, complete Node SDK/CLI, and the full PGlite/oracle matrix; refreshed live Actions and reconciled shared main through `b27dd2bb` | Local production gate / hosted evidence / release engineering | The exact `6f29d9ce` packet passed: Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40 five-seed/eight-backend traces at 621 operations. Shared main then advanced through chunked/9P/Ozone/NFS changes into FUSE forced-teardown/read-worker/native-session fixes; current b27 hosted workflows are queued/pending and no terminal current-head acceptance exists. |

| 2026-09-22 08:23–08:41 | Ran the complete exact-SHA packet on pushed `eaf59894`, then fetched current shared main and live Actions | Local production qualification / hosted evidence | Format, full locked Rust, strict Clippy, optimized N-API, complete Node SDK/CLI, PGlite reconnect/versioning/VFS/lifecycle/split-store/FUSE, Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and 40×621 traces passed. Current `ccd3f671` adds 9P mount-signal, WebDAV concurrency, Ozone, and FoundationDB lockfile changes; current hosted workflows are queued/pending and R2 `35663627575` failed closed at the cap before live admission. |

| 2026-09-22 08:41–08:50 | Requalified exact pushed `a0c73c54` across format, full locked Rust, strict Clippy, optimized N-API, complete Node SDK/CLI, and full PGlite/oracle matrix; refreshed shared main and Actions | Local production qualification / hosted evidence | The exact `a0c73c54` packet passed with Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and 40×621 traces. Current `e7888067` adds 9P hosted lifecycle, WebDAV session-member, Ozone cleanup, and HTTP early-rejection changes; current hosted workflows are queued/pending and R2 `35664438822` failed closed at the cap before live admission. |
| 2026-09-22 08:50–08:57 | Fast-forwarded to `d391f9b7`, reran exact-tip NFS tests/strict Clippy, ran widened WebDAV direct-session and network concurrency probes, and refreshed Actions | Local changed-surface qualification / hosted evidence | NFS package tests (38 plus process-crash/restart, rootless, transport, and v4 suites) and strict Clippy passed; WebDAV direct-session and network probes passed 64 concurrent PUT/GET pairs with streaming/auth/error coverage. Local direct 9P correctly skipped at the macOS/kernel boundary. Current Actions are queued/pending across R2, Native 9P, W04, W08, fault, and CI; full current-tip packet and hosted release evidence remain open. |
| 2026-09-22 08:57–09:13 | Completed the pushed `b65d4e31` full local packet: format, locked Rust workspace, strict Clippy, optimized N-API, complete oracle-enabled Node/N-API suite, real PGlite lifecycle/matrix, upstream suite, and trace parity; refreshed shared main and Actions | Local production qualification / hosted evidence | Full packet passed: Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and 40×621 traces; 64-way WebDAV, NFS process-crash/restart, cleanup fail-closed, and artifact aggregation passed. Oracle dependencies were installed frozen without lifecycle scripts after the first environment-only `unstorage` module gate. Origin advanced to `9b2dabd7`; its hosted R2/Native 9P/CI/W04/W08/fault runs are queued or pending and do not qualify the current successor. |
| 2026-09-22 09:13–09:30 | Completed the exact pushed `9354362a` successor packet with format, full locked Rust workspace, strict workspace Clippy, optimized N-API build, complete oracle-enabled Node/N-API suite, and the full PGlite/provider matrix; refreshed shared main | Local production qualification / hosted evidence / ledger maintenance | `9354362a` passed Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, all 40×621 oracle traces, PGlite lifecycle/backup-restore/split-store/VFS/FUSE, cleanup-failure fail-closed, WebDAV NodeFs crash/restart and 64-way concurrency, NFS process-crash/restart, chunked publication, and artifact aggregation. After the checkpoint push, `origin/main` advanced to `61b904c3` with WebDAV/9P/NFS successors; GitHub API refresh was unreachable, so no hosted status was promoted. Production decision remains NO-GO. |
| 2026-09-22 09:30–09:35 | Reconciled concurrent main, rebased the W05 ledger onto `0ad4928e`, pushed `0aa25ad7` to `origin/main`, and verified remote/local equality | Release engineering / documentation | The published W05 ledger checkpoint is exactly `0aa25ad7`; `git fetch origin main` confirms `HEAD == FETCH_HEAD`, the worktree is clean, and no hosted or provider result was inferred from the unavailable GitHub API. Production decision remains NO-GO. |
| 2026-09-22 12:02–12:23 | Requalified exact shared successor `ed29016e` after the prior ledger boundary with format, full locked Rust workspace, strict Clippy, optimized N-API build, complete Node/N-API suite, and the real PGlite/oracle matrix | Local production qualification | Exact `ed29016e` passed the full packet: Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40×621 traces. WebDAV detached-native cleanup handling passed in the exact Rust workspace; provider/native opt-ins remained explicit skips. |
| 2026-09-22 12:23–12:32 | Requalified exact shared tip `42ecd21e` with format, full locked Rust workspace, strict Clippy, optimized N-API build, complete Node/N-API suite, and the real PGlite/oracle matrix; refreshed hosted status and fetched the next shared successor | Local production qualification / hosted evidence | Exact `42ecd21e` passed the full packet with Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, all 40×621 traces, and 29 S3 gateway tests including conditional PUT/CAS. Same-SHA CI/W08 were cancelled during mainline movement; latest `origin/main` is `3de49e33` with W04 success but CI/fault/W08/R2 non-terminal. Production remains NO-GO. |
| 2026-09-22 12:32–12:44 | Requalified exact pushed tip `f1ee869a` across format, full locked Rust workspace, strict Clippy, optimized N-API build, complete Node/N-API suite, and the full real PGlite/oracle matrix; refreshed exact-SHA hosted status and fetched current main | Local production qualification / hosted evidence | Exact `f1ee869a` passed Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40×621 traces. W04 and W08 targets succeeded on the same SHA; CI, fault, and W08 policy were cancelled during later movement. `origin/main` advanced to `7389be4d`, whose 9P/FoundationDB changes remain unqualified. Production remains NO-GO. |
| 2026-09-22 12:44–12:52 | Requalified exact pushed `1bdf8846` after the 9P/FoundationDB/WebDAV successors with format, full locked Rust workspace, strict Clippy, optimized N-API build, complete Node/N-API suite, and the full real PGlite/oracle matrix | Local production qualification | Exact `1bdf8846` passed Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, all 40×621 traces, WebDAV provider/network concurrency and crash/restart, S3 restart/barrel scope, P9/9P/FUSE/NFS/WebDAV differential surfaces, chunked integration, distribution, and artifact aggregation. Provider credentials and privileged native mounts remained explicit skips. Production remains NO-GO. |
| 2026-09-22 12:52–12:53 | Refreshed exact-SHA Actions, fast-forwarded the checkout through concurrent shared-main documentation, and updated the W05 ledger | Hosted evidence / release engineering | W04 `35680709392` and fault injection `35680709400` succeeded; W08 policy `35680709423` remained in progress, W08 targets `35680709394` were queued, and CI `35680709436` was cancelled. `origin/main` is now `6d65716f`, differing from `1bdf8846` only in `WORK_TRACKER.md` and W01/W26 docs. No hosted release acceptance is promoted; production remains NO-GO. |
| 2026-09-22 12:57–13:00 | Refreshed the hosted jobs triggered by pushed ledger checkpoint `75248969` with two bounded polls | Hosted evidence / release engineering | W04 `35681358469` succeeded; fault injection `35681358424`, CI `35681358476`, and W08 policy `35681358387` were cancelled; W08 targets `35681358456` remained in progress. These results are non-terminal/cancelled and do not close the release gate. |

| 2026-09-22 13:00–13:35 | Reproduced and fixed the full-suite WebDAV structural-driver failure, pushed `de78011a`, then ran the exact full local packet and refreshed same-SHA hosted Actions | Implementation / local production qualification / hosted evidence | The deterministic partial-request peer reset, cleanup-error preservation, and structural `501` bounded-listing assertion passed in native and structural WebDAV phases. Exact `de78011a` passed format, full locked Rust, strict Clippy, optimized N-API, complete Node SDK/CLI, real PGlite, Rust SDK `6/3/0`, Node SDK `5/3/0`, upstream `1200/82`, and 40×621 traces. R2 `35683264337` failed closed at the monthly cap before secrets; CI/Native 9P/W08/W04/fault status remains non-terminal or cancelled. Production remains NO-GO. |

| 2026-09-22 13:35–13:50 | Requalified exact pushed `175615c7` after the d43 S3 observability/session-hook and 9P/W07 successors; refreshed same-SHA hosted Actions, fetched concurrent `origin/main`, rebased safely, and prepared the next ledger checkpoint | Local production qualification / hosted evidence / release engineering | Exact `175615c7` passed format, diff check, full locked Rust workspace, strict Clippy, optimized N-API, complete Node SDK/CLI suite, real PGlite, Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40×621 traces. Same-SHA W04 `35684095331`, fault `35684095300`, and W08 policy `35684095289` succeeded; CI `35684095286` and W08 targets `35684095309` remained in progress. `origin/main` advanced to documentation-only `9563d2db`; no hosted/provider/native/package acceptance was inferred, and production remains NO-GO. |

| 2026-09-22 13:50–13:59 | Requalified exact pushed `b3cfbcb4` after the S3 per-key error-hook change, refreshed its hosted runs, fetched the next shared runtime successor, and prepared the next ledger checkpoint | Local production qualification / hosted evidence / release engineering | Exact `b3cfbcb4` passed the focused S3 regression, format/diff, full locked Rust workspace, strict Clippy, optimized N-API, complete Node SDK/CLI suite, real PGlite, Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40×621 traces. W04 `35684755110` succeeded; CI/W08 targets/W08 policy/fault were cancelled by mainline movement. `origin/main` advanced to `1179d9e3` with 9P EOF/backpressure and WebDAV server changes, so current-tip qualification remains open and production is NO-GO. |

| 2026-09-22 13:59–14:08 | Requalified exact pushed `8d7e9f3d` after the 9P EOF/backpressure and teardown changes, refreshed same-SHA hosted Actions, fetched the next WebDAV successor, and prepared the next ledger checkpoint | Local production qualification / hosted evidence / release engineering | Exact `8d7e9f3d` passed format/diff, full locked Rust workspace, strict Clippy, optimized N-API, complete Node SDK/CLI suite, real PGlite, Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40×621 traces. W04, fault, W08 policy, and W08 targets succeeded; CI was cancelled. `origin/main` advanced to `4f6e1048` with recursive WebDAV mutation, duplicate-header, listener-restart, and N-API changes, so current-tip qualification remains open and production is NO-GO. |
| 2026-09-22 14:08–14:58 | Requalified exact pushed `338026ae` after the 9P remote-admission successor, handled sandbox loopback/spawn boundaries with permitted host runs, ran the full Rust/Clippy/N-API/Node/PGlite packet, fetched/rebased concurrent main, queried exact-SHA hosted state, and prepared this ledger/tracker checkpoint | Local production qualification / hosted evidence / release engineering | Exact `338026ae` passed format/diff, full locked Rust workspace, strict Clippy, optimized N-API build/postbuild, complete elevated Node SDK/CLI suite, and real PGlite/provider/CLI/oracle matrix. Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40×621 traces passed. Current `origin/main` is docs-only `44cd558a`; W04 succeeded, fault/W08 policy are in progress, CI/W08 targets are cancelled, and no R2 run was admitted at the cap. Production remains NO-GO; security/native/provider/package/scope/final-audit gates remain open. |
| 2026-09-22 14:58–15:15 | Rebased over the S3, SQLite, HTTP/Windows, and shared documentation successors; requalified exact `01f844c` with the full Rust/Clippy/N-API/Node/PGlite packet, pushed the W05 ledger/tracker as `7cc9c8c5`, and refreshed exact-SHA hosted state | Local production qualification / hosted evidence / release engineering | Exact `01f844c` passed format/diff, full locked Rust workspace, strict Clippy, optimized N-API build/postbuild, complete elevated Node SDK/CLI suite, and real PGlite/provider/CLI/oracle matrix. Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40×621 traces passed. Pushed ledger tip `7cc9c8c5` has W04/W08 queued, CI/W08 targets pending, fault queued, and no R2 admission at the cap. Production remains NO-GO; security/native/provider/package/scope/final-audit gates remain open. |
| 2026-09-22 15:15–15:26 | Rebased and pushed the W05 ledger onto concurrent `origin/main` through `9e187955`, verified the integrated S3 framing regression, reran the full Rust/Clippy/N-API/Node/PGlite packet, refreshed current-SHA Actions, and prepared the next production checkpoint | Local production qualification / hosted evidence / release engineering | Runtime `4f9120a2` and pushed tip `9e187955` passed the focused S3 regression, format/diff, full locked Rust workspace, strict Clippy, optimized N-API build, complete Node SDK/CLI suite, and real PGlite/provider/CLI/oracle matrix. Counts are Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, 40×621 traces, and 33/33 S3 gateway tests. Same-SHA CI `35690379471`, fault `35690379495`, W04 `35690379497`, W08 targets `35690379501`, and W08 policy `35690379506` cancelled before terminal acceptance; no R2 run was admitted. Production remains NO-GO. |
| 2026-09-22 15:26–15:43 | Reconciled direct-9P, PGlite autocommit, S3 pipelining, and structural-driver successors; reran the exact current full Rust/Clippy/N-API/Node/PGlite/SDK/CLI packet through `496ed42b` | Local production qualification / release engineering | Exact current `496ed42b` passed format/diff, full locked Rust workspace, strict Clippy, optimized N-API build, direct and structural 9P session tests, complete Node/N-API suite, PGlite matrix, Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, all 40×621 traces, and 34/34 S3 gateway tests. R2/provider/native/package/scope/final-audit gates remain explicit; production remains NO-GO. |
| 2026-09-22 15:43–15:48 | Rebased/pushed the W05 structural-9P ledger over the S3 HTTP-framing successor and ran the full current gateway suite | Local release qualification / release engineering | Pushed tip `7440a68` is exactly synchronized with `origin/main`. The current S3 gateway suite passed `37/37`, including Expect/Continue, transfer-encoding refusal, HEAD framing, pipelined responses, short-response framing, multipart/CAS, interop, and lifecycle coverage. Production remains NO-GO; hosted/provider/native/package/scope/final-audit gates remain open. |
| 2026-09-22 15:48–15:52 | Reconciled the rejected-request-body drain fix, rebased over concurrent W07/9P documentation, rebuilt the N-API artifact, and reran the complete Node/N-API suite plus 37-test S3 gateway packet | Changed-surface integration qualification / release engineering | Runtime `d870f900` and current docs tip `2278f32b` passed the S3 gateway packet and rebuilt Node suite. The direct/structural 9P, WebDAV, S3, FUSE/NFS, differential, distribution, and artifact phases passed; provider/native opt-ins remained explicit skips. Production remains NO-GO. |
| 2026-09-22 15:52–15:57 | Committed and pushed the W05 ledger/tracker successor as `b72d02f4` after rebasing over concurrent `d4f43b28` and `cf0ed708`; refreshed exact-SHA hosted Actions and waited for the fault run to terminate | Release engineering / hosted evidence | `HEAD == origin/main == b72d02f4`. W04 policy `35692639780` succeeded; fault `35692639786`, W08 policy `35692639797`, CI `35692639806`, and W08 targets `35692639831` completed cancelled. No hosted release acceptance is promoted; production remains NO-GO. |
| 2026-09-22 15:57–15:59 | Rebased the hosted-status ledger over concurrent W01/W07/W26 changes and published the final documentation tip `13f17162` | Release engineering / documentation | `HEAD == origin/main == 13f17162`; the exact W05 hosted-status evidence remains tied to candidate `b72d02f4`, with no terminal release acceptance promoted. Production remains NO-GO. |
| 2026-09-22 15:59–16:12 | Stabilized immutable candidate branch `andymac4182/c/w05-production-candidate-20260922`, isolated manual workflow concurrency, and dispatched the same-SHA CI/fault/W04/W07/W08/Native 9P packet | Release engineering / hosted qualification | Candidate SHA `25e275ab6d4f918be72dcd8f62a5baca6bbcd251` is stable. W04, W08 policy, and Native 9P were later observed successful; W07/W08 targets were running and CI/Fault were queued. Candidate push Fault completed with Ubuntu/Windows green and macOS runner-queued. |
| 2026-09-22 16:12–16:24 | Ran exact-candidate formatting, full locked Rust workspace tests, strict Clippy, and read-only AWS/provider workflow audits | Local production gate / security-provider gate | Rust workspace and Clippy passed; AWS audit remained safely blocked on missing protected inputs/OIDC shape; provider workflow-path checks passed without reading credentials. |
| 2026-09-22 16:24–16:39 | Ran exact-candidate PGlite end-to-end harness, rebuilt optimized N-API artifact, reran complete pinned-oracle Node SDK/CLI/N-API suite, and updated this ledger/tracker | Local SDK/CLI/provider gate / documentation | PGlite packet passed Rust SDK `6/3/0`, Node SDK `5/3/0`, CLI `12/2`, upstream `1200/82`, and all 40×621 traces; exact N-API build and full Node suite passed; W05.40 records the remaining hosted/provider/native/package/scope gates. |
| 2026-09-22 16:39–16:47 | Ran credential-free W07/W08/AWS control fixtures, recovered exact npm package dry-runs with a task-scoped cache, verified 25/25 workspace licenses, and opened security request [#3](https://github.com/andymac4182/mount-rs/issues/3) | Local packaging / security coordination | W07 ledger `6/6`, W08 ledger `7/7`, AWS config `7/7`, AWS environment `3/3`, W08 evidence `11/11`, `@mount-rs/core` and `@mount-rs/virtual-fs` pack dry-runs, and Apache-2.0 inventory `25/25` passed. AWS credentials remain external and no secret value was read or stored. |
| 2026-09-22 16:47–16:53 | Completed focused security diff scan `7b383f24-5724-43a6-bbf6-8bbf22c1947c` over the W05 candidate range; refreshed exact-SHA hosted statuses | Security review / hosted evidence | Security scan completed with zero reportable findings and complete focused coverage; W07 and Fault are terminal-successful, W08 targets and CI remain queued, and production remains NO-GO pending those hosted gates plus AWS/provider/native/package/scope/W20.6 closure. |
| 2026-09-22 16:53–17:09 | Waited for and verified the attestation-enabled W08 release-target run; refreshed the candidate CI matrix and added the terminal provenance evidence to the W05 ledger | Hosted package/provenance / release control | W08 `35697156046` succeeded for Linux and macOS builds, downloaded assets, CycloneDX SBOM attestations, Rekor publication, repository attestation upload, and `gh attestation verify`. Candidate CI `35694325175` had not yet terminated at this observation. Production remains NO-GO. |
| 2026-09-22 17:09–17:35 | Retrieved the terminal candidate CI logs and W26 artifact, classified the TiDB/Ozone/FUSE/provenance failures, repaired the TiDB publication-ack failure injector and staged the W26 N-API build, ran focused local checks, and safely rebased/pushed the implementation chunk | Hosted failure analysis / implementation repair / release engineering | Candidate CI `35694325175` is terminal `failure`; the exact job evidence and remaining gates are recorded in W05.41. Shared `origin/main` now contains `f94b53d8`. Rust TiDB tests passed 8/8 unit tests with live-service cases explicitly ignored, storage benchmark tests passed, YAML parsed, and diff/format checks passed. A new immutable candidate and hosted rerun are still required. |

Estimated active engineering time for the completed W05 continuation before
this production program plus the current qualification checkpoints: **about
52–64 h total active work so far**. The
production-readiness register currently represents **about 63–145 h** of
provisional active engineering and review across the mapped rows, excluding
hosted queues, provider provisioning, signing, and other external wait time.
These estimates are planning ranges, not commitments; they will be revised
with evidence after each chunk.
