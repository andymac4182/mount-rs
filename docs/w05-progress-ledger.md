# W05 Cloudflare R2 progress ledger

Last updated: 2026-09-21 17:39 AEST (2026-09-21 07:39 UTC)

This is the working ledger for the W05 Cloudflare R2 workstream. Percentages
and time estimates are provisional. They separate implementation work from
provider, hosted-CI, and native-platform gates; a local pass does not close a
hosted or native gate.

## Overall position

**Provisional completion: 92%.** The implementation, scoped CI credentials,
cost admission guard, local live-provider coverage, Rust/Node SDK and CLI
matrices, and bounded benchmark packet are in place. The remaining acceptance
boundary is a successful hosted workflow run after the first hosted attempt
was canceled during a redundant multi-backend live trace. The replacement run
is `35573697664` and was queued at the time of this update.

The current implementation tip is `d4f7281` on `origin/main`. It includes the
latest CI trace-lane fix rebased over remote main. No credential value is
stored in the repository or in this document.

## Work-item ledger

| Work item | Work type | Status | Completion | Evidence | Remaining actions | Provisional engineering time | External blockers / boundaries |
| --- | --- | --- | ---: | --- | --- | --- | --- |
| W05.0 Land the R2 object-store driver and configurable endpoint support | Implementation | Complete | 100% | `integrations/mount-rs-r2`, endpoint validation, signed HTTP adapter tests, focused Rust tests and Clippy passed. | None for the implementation item. | 0 h | Full acceptance is tracked separately below. |
| W05.1 Unblock D01 and authenticate against actual Cloudflare R2 | Provider / CI configuration | Complete locally; hosted close pending | 95% | Bucket-scoped Object Read & Write credentials are stored as encrypted GitHub `r2-ci` environment secrets: `MOUNT_RS_R2_ENDPOINT`, `MOUNT_RS_R2_BUCKET`, `MOUNT_RS_R2_ACCESS_KEY_ID`, and `MOUNT_RS_R2_SECRET_ACCESS_KEY`. The CI token is one-week TTL and bucket-scoped. Local authenticated Cloudflare R2 filesystem, block, and metadata tests passed. | Obtain one successful hosted run using the encrypted environment and then record the hosted result in `WORK_TRACKER.md`. Rotate before 2026-09-28. | 0.5–1 h | The macOS Keychain item was not readable through the requested security authorization path; CI was provisioned through the authenticated Cloudflare UI and GitHub encrypted secrets instead. |
| W05.2 Verify immutable writes, ranges, retries, reconnect, cleanup, and concurrent publication with independent metadata providers | Implementation + provider gate | Implementation complete; hosted provider gate partially observed | 95% | Local live R2 passed filesystem/CAS, SQLite-metadata/R2-block, PGlite-metadata/R2-block, fresh-client/reopen, concurrent publication, range, retry, prefix isolation, and exact cleanup cases. The canceled hosted run passed the live Rust backend and PGlite/R2 block gates before cancellation. | Re-run the hosted workflow to completion and retain the hosted success markers. | 0.5–1.5 h | Cloudflare HTTP latency is variable; hosted evidence must remain distinct from local signed-S3-compatible evidence. |
| W05.3 Run Node SDK, Rust/Node CLI, N-API/native, parity, and benchmark lanes on live R2 | Mixed implementation + hosted/native/provider gates | Local and implementation work complete; hosted close pending | 92% | Local evidence: Rust SDK `pass=9 skip=0 fail=0`; Node SDK `pass=7 skip=1 fail=0`; CLI `pass=14 skip=1 fail=0`; upstream `4` files, `1200 passed`, `82 skipped`; five-seed trace parity passed; live R2 benchmark and full public-NAPI benchmark passed. The canceled hosted run reached the same Rust/Node/CLI/upstream gates and uploaded the benchmark artifact, then was canceled during the redundant trace expansion. | Confirm replacement hosted success, including the final live R2 CLI, N-API, and service-evidence steps after the trace. | 1–3 h, mostly hosted wait | macOS native NFS qualification passed locally. Linux FUSE/privileged native acceptance is a separate platform boundary and is not claimed by this W05 ledger. |
| W05.4 Record redacted service identity and revision without credentials | Implementation / evidence hygiene | Complete | 100% | `scripts/r2-service-evidence.sh` reports service, endpoint authority, bucket, revision, and owned-prefix counts without secret values; embedded endpoint credentials are rejected. Local service-evidence gate passed. | Reconfirm the marker in the successful hosted log. | 0–0.5 h | Hosted log review must stay redacted; secret-bearing environment values are masked by GitHub. |
| W05.5 Fix the live Node factory expected-byte assertion and guarantee unique fixture keys with exact cleanup | Implementation + provider gate | Complete locally; hosted close pending | 98% | PGlite/R2 Node factory, DELETE/HEAD cleanup, chunked factories, restart/fencing, userspace FUSE, and full N-API test suite passed locally with the pinned oracle. The hosted provider matrix passed its Node SDK rows before cancellation. | Confirm the final hosted N-API suite and cleanup marker. | 0–1 h | Hosted runner queue and Cloudflare latency can affect elapsed time, not the implementation result. |
| W05.6 Run the configuration-driven CLI gate against the canonical Cloudflare endpoint | Provider / CLI hosted gate | Complete locally; hosted close pending | 96% | Standalone live CLI gate passed locally for PGlite-metadata/R2-block and SQLite-metadata/R2-block, ranged reads, auth isolation, graceful reopen, presence checks, and owned-prefix cleanup. Hosted provider-matrix CLI rows passed before cancellation. | Let the replacement run reach the dedicated live Rust CLI script after the trace. | 0.5–1 h | Requires the encrypted `r2-ci` secrets and a live GitHub runner; no credential is permitted in the CLI fixture or repository. |
| W05.7 Add usage-capped CI credentials, cost guard, and full hosted acceptance | Implementation + hosted/provider gate | In progress | 85% | `.github/workflows/cloudflare-r2.yml` uses the `r2-ci` environment and a fail-closed budget job. `scripts/r2-ci-budget.sh` accepted the first September run as `2/20` with an estimated monthly envelope of `$80.00` (`20 × $4.00`), below the `$100` target. A Cloudflare account-wide `$80` budget alert is also configured. The first hosted run budget job passed; the live job was canceled after substantial passing coverage. | Verify run `35573697664`. If it passes, update this row and W05 in `WORK_TRACKER.md`, then commit/push the tracker closure. If it fails or is canceled, retain the failure evidence, patch only the cause, commit/push, and rerun. | 0.5–2 h after the current run; each failure iteration may add 1–3 h | GitHub runner scheduling is currently a queue. Cloudflare budget alerts notify but do not hard-pause account usage; the GitHub run-count/per-run envelope is the enforced cap for this CI lane, while the one-week bucket-scoped token limits credential lifetime and blast radius. A strict provider-side hard stop at exactly `$100` remains unavailable as an R2 alert feature. |

## Cost and credential control

| Control | Current setting / evidence | What it proves | Limitation |
| --- | --- | --- | --- |
| GitHub environment | `r2-ci`, four encrypted secrets, no repository plaintext values | CI receives credentials only at the live job boundary | A maintainer with environment administration can still change the secrets or policy. |
| Token scope | Object Read & Write, only `mount-rs-integration-tests`, one-week TTL | Limits the test credential to the test bucket and a short rotation window | Cloudflare token scope is not a dollar quota. |
| CI admission guard | 20 accepted runs per UTC month, `$4.00` assumed per run, `$80.00` maximum envelope, fail closed before secrets are used | Prevents this workflow from admitting more than 20 live runs and leaves `$20` headroom to the requested `$100` ceiling | It is a cost envelope, not a provider billing hard stop; per-run workload changes must stay within the bounded packet. |
| Cloudflare alert | Account-wide R2 alert at `$80` | Provides early notification before the target ceiling | The alert does not automatically suspend R2 usage. |
| Workflow bounds | Live job timeout 120 minutes, unique run/prefix IDs, exact cleanup, benchmark artifact retained | Limits runaway job duration and isolates remote fixtures | Timeout is an execution bound, not a billing guarantee. |

## Evidence boundary matrix

| Surface | Result | Acceptance meaning |
| --- | --- | --- |
| Repository implementation | `d4f7281` pushed to `origin/main` | CI workflow, budget guard, hosted orchestration, shared Cargo wrapper use, and provider-matrix R2 CLI coverage are committed. |
| Local Rust/provider | Passed with authenticated Cloudflare R2 | Strong provider behavior evidence on the development host; not hosted CI evidence. |
| Local Node SDK/N-API | Passed with pinned `mountx` oracle and live R2/PGlite rows | Public addon and SDK behavior are qualified locally; not hosted runner evidence. |
| Local Rust and Node CLI | Passed with live R2/PGlite configuration and reopen/cleanup | Configuration-driven CLI behavior is qualified locally; not hosted runner evidence. |
| Local native | macOS NFS structural read/write/teardown passed | Native local qualification only; Linux FUSE and other host prerequisites remain separate. |
| Hosted run `35570596593` | Budget passed; live job canceled | Partial hosted evidence only. It must not be called a release or W05 acceptance pass. |
| Hosted rerun `35573697664` | Queued at ledger update | This is the current acceptance decision point. |

## Remaining action plan

1. Monitor `35573697664` through conclusion; inspect only redacted markers and
   the benchmark artifact result.
2. On success, add the hosted run evidence to W05.7 and update the W05
   dashboard/D01 wording in `WORK_TRACKER.md`; commit and push that tracker
   chunk to `origin/main`.
3. On failure or cancellation, classify the exact step, make the smallest
   scoped fix, run local syntax/focused checks, commit and push, and repeat the
   hosted gate. Do not mark W05 complete on a canceled or partial run.
4. Before the CI token expires, rotate the four `r2-ci` secrets and rerun the
   budgeted acceptance packet. Do not copy secret values into issues, logs,
   this ledger, or chat.
5. Keep the unrelated accidental read-only all-buckets Cloudflare token
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
| 2026-09-21 07:39–current | Created this ledger and monitored the replacement queue | Documentation / hosted wait | Rerun remains the open acceptance action. |

Estimated active engineering time for this continuation: **about 1–1.5 h**;
hosted queue and test runtime are excluded from that estimate. Remaining
engineering time is provisional and depends on the replacement run's result.
