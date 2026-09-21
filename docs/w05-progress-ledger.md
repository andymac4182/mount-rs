# W05 Cloudflare R2 progress ledger

Last updated: 2026-09-21 19:23 AEST (2026-09-21 09:23 UTC)

This is the working ledger for the W05 Cloudflare R2 workstream. Percentages
and time estimates are provisional. They separate implementation work from
provider, hosted-CI, and native-platform gates; a local pass does not close a
hosted or native gate.

## Overall position

**Provisional completion: 100% of the requested W05 scope.** The scoped CI
credentials, fail-closed cost admission guard, local live-provider coverage,
Rust/Node SDK and CLI matrices, bounded benchmark packet, and hosted
Cloudflare R2 acceptance all pass. The authoritative final run is
`35579757447` at commit `3db491e`; its budget gate accepted run `12/20` with
an estimated maximum monthly envelope of `$80.00`, and its live job passed
the full packet and uploaded the benchmark artifact.

The current implementation tip is `3db491e` on `origin/main`. No credential
value is stored in the repository or in this document. Native/platform gates
remain explicitly bounded: macOS native NFS qualification is local evidence;
Linux FUSE, Windows, and signed/activated FSKit acceptance are separate
platform workstreams and are not claimed by this W05 close.

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
| W05.7 Add usage-capped CI credentials, cost guard, and full hosted acceptance | Implementation + hosted/provider gate | Complete; rotation is scheduled maintenance | 100% | `.github/workflows/cloudflare-r2.yml` uses the encrypted `r2-ci` environment and a fail-closed budget job. Final run `35579757447` accepted `12/20` for September with an estimated maximum monthly envelope of `$80.00`, then passed Rust backend, Rust SDK `9/0/0`, Node SDK `7/1/0`, CLI `14/1/0`, upstream `1200 passed/82 skipped`, five-seed local/PGlite trace coverage, bounded live R2 trace `621/621`, live CLI, N-API, service evidence, and benchmark artifact `10630468958`. A Cloudflare account-wide `$80` budget alert is also configured. | Rotate the four `r2-ci` secrets before 2026-09-28 and rerun the bounded lane if the workstream remains active. No acceptance work remains. | 0 h acceptance work; 0.5–1 h provisional rotation maintenance | Cloudflare budget alerts notify but do not hard-pause account usage; the GitHub run-count/per-run envelope is the enforced cap for this CI lane, while the one-week bucket-scoped token limits credential lifetime and blast radius. A strict provider-side hard stop at exactly `$100` remains unavailable as an R2 alert feature. |

## Cost and credential control

| Control | Current setting / evidence | What it proves | Limitation |
| --- | --- | --- | --- |
| GitHub environment | `r2-ci`, four encrypted secrets, no repository plaintext values | CI receives credentials only at the live job boundary | A maintainer with environment administration can still change the secrets or policy. |
| Token scope | Object Read & Write, only `mount-rs-integration-tests`, one-week TTL | Limits the test credential to the test bucket and a short rotation window | Cloudflare token scope is not a dollar quota. |
| CI admission guard | 20 accepted runs per UTC month, `$4.00` assumed per run, `$80.00` maximum envelope, fail closed before secrets are used; final run marker was `accepted_run=12/20`, `remaining_slots=8` | Prevents this workflow from admitting more than 20 live runs and leaves `$20` headroom to the requested `$100` ceiling; the final accepted run used the warning-free summary path | It is a cost envelope, not a provider billing hard stop; per-run workload changes must stay within the bounded packet. |
| Cloudflare alert | Account-wide R2 alert at `$80` | Provides early notification before the target ceiling | The alert does not automatically suspend R2 usage. |
| Workflow bounds | Live job timeout 120 minutes, unique run/prefix IDs, exact cleanup, benchmark artifact retained | Limits runaway job duration and isolates remote fixtures | Timeout is an execution bound, not a billing guarantee. |

## Evidence boundary matrix

| Surface | Result | Acceptance meaning |
| --- | --- | --- |
| Repository implementation | `3db491e` pushed to `origin/main` | CI workflow, budget guard, hosted orchestration, shared Cargo wrapper use, provider-matrix R2 CLI coverage, and the lease-refresh fix are committed. |
| Local Rust/provider | Passed with authenticated Cloudflare R2 | Strong provider behavior evidence on the development host; not hosted CI evidence. |
| Local Node SDK/N-API | Passed with pinned `mountx` oracle and live R2/PGlite rows | Public addon and SDK behavior are qualified locally; not hosted runner evidence. |
| Local Rust and Node CLI | Passed with live R2/PGlite configuration and reopen/cleanup | Configuration-driven CLI behavior is qualified locally; not hosted runner evidence. |
| Local native | macOS NFS structural read/write/teardown passed | Native local qualification only; Linux FUSE and other host prerequisites remain separate. |
| Hosted run `35570596593` | Budget passed; live job canceled | Partial hosted evidence only. It must not be called a release or W05 acceptance pass. |
| Hosted diagnostic run `35575940720` | Reached the Rust CLI and reported redacted `ESTALE: stale file handle, metadata lease` during graceful shutdown | Failure evidence that identified the lease-refresh defect; not an acceptance pass. |
| Hosted run `35577687152` | Passed the full hosted packet after `c71c8ee` refreshed an expired unfenced lease during shutdown | Hosted acceptance evidence before the final budget-summary formatting correction. |
| Final hosted run `35579757447` | Passed at `3db491e`; budget `12/20`, full SDK/CLI/N-API/service/trace packet, and artifact `10630468958` | Authoritative W05 hosted acceptance. |

## Remaining action plan

1. No active W05 acceptance action remains: final hosted run
   `35579757447` is successful and the evidence is recorded here and in
   `WORK_TRACKER.md`.
2. Before the CI token expires on 2026-09-28, rotate the four `r2-ci` secrets
   and rerun the bounded acceptance packet if the workstream remains active.
   Do not copy secret values into issues, logs, this ledger, or chat.
3. Keep macOS native NFS evidence distinct from Linux FUSE, Windows, and
   signed/activated FSKit gates; those remain separate platform workstreams.
4. Keep the unrelated accidental read-only all-buckets Cloudflare token
   outside this workstream until its deletion is explicitly authorized.

## Session time log

Times below are provisional wall-clock/activity estimates. Hosted waiting is
shown separately from active engineering time.

| UTC time | Activity | Classification | Result / next state |
| --- | --- | --- | --- |
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
| 2026-09-21 09:16–current | Updating the final ledger and `WORK_TRACKER.md` closure, then committing and pushing the documentation chunk | Documentation / release engineering | Goal closure follows after clean pushed state is verified. |

Estimated active engineering time for this continuation: **about 2–2.5 h**;
hosted queue and test runtime are excluded from that estimate. Remaining
engineering time is provisional and limited to scheduled secret rotation or
future platform-specific gates.
