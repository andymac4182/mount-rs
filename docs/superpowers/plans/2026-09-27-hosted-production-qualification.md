# Hosted Production Qualification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Execute the existing signed CLI, ten-process SQLite cache, full production target, and owned TiDB/RustFS backing pilot on clean hosted checkouts with retained success or failure evidence.

**Architecture:** Add optional dispatch jobs to the existing Remote Drives workflow. Every job uses a target outside the checkout, one Cargo build worker, pinned actions, fresh owned output, clean source checks, and retained binary identity. The jobs invoke existing public test supervisors and fixture owners; the workflow does not add a controller or change their contracts.

**Tech Stack:** GitHub Actions, Bash, Python 3, existing Rust integration tests and scripts/cargo-shared; the backing pilot additionally uses Node 24, pnpm 11, the release NAPI addon, and the owned local Docker Engine.

## Global Constraints

- Preserve push, pull request, and default dispatch behavior. A dispatch selecting `native_qualification`, `production_target`, or `owned_backing_pilot` skips all five original jobs, including the old soak and TiDB scale loads. Every new input defaults to false; `owned_engine_socket` defaults to an empty string.
- Signed CLI runs are separate debug builds of `local-oidc-fixture` and `io-profiling,local-oidc-fixture` on Ubuntu 24.04 and macOS. Each must execute the exact named test once; exit zero with zero tests cannot qualify.
- Cache execution uses only `ten_process_sqlite_cache_qualification` through the existing supervisor, with `MOUNT_RS_TEN_PROCESS_RUN=1` and a fresh absolute output path that the supervisor creates. Never invoke `native_worker` directly.
- Preserve the cache's 600-second setup, 1800-second work, 600-second phases, 30-second operations, 95-second cleanup with five-second force/reap reserve, 30-second audit, and 2525-second outer watchdog. Require 64 GiB free disk and strictly less than 24 GiB per PID and observed aggregate; retain the existing 100 ms sampling and output limits.
- Full target is explicitly SQLite, `full`, 10000 Drives/clients, 5000 Partitions, 1000 files per Drive, ten owned server PIDs, 100000 server/Drive routes, and 30 seconds per workload stage. Preserve 990 × 4096-byte, nine × 131072-byte, and one × 1048576-byte files per Drive: 10000000 files and 62832640000 initial payload bytes.
- Full target runs all eight existing patterns in both mostly-idle mode with 100 active clients and continuously active mode with all 10000 clients. Preserve online namespace/payload population, independent acknowledged-state byte/EOF/membership oracles, authentication denials, revocation, and never-replay semantics.
- Preserve the full target's 600-second setup/phase, 1800-second work, 30-second request, 95-second child cleanup, and all existing bounded observer/cleanup stages. Require at least 64 GiB free disk and at most 24 GiB RSS per owned process. Summed individual peaks are not a simultaneous aggregate peak.
- A 100-minute job budget includes hosted compilation and the existing roughly 42–44 minute native execution envelopes. It does not extend any test deadline. Host refusal, timeout, incomplete observation, or failed floor is a failed job; no smaller fallback is permitted.
- The pilot requires the operator to supply the literal `/var/run/docker.sock`. Set `DOCKER_HOST` to that socket and remove Docker context/TLS aliases before both creation and observation. A private 0700 directory contains a 0600 JSON capability with exactly `socket_path`; it is never exported.
- Pilot scope is the existing combo-only RustFS owner with literal `RUSTFS_COMBO_COMMAND='./scripts/test-tidb.sh'`, durable TiDB, NAPI and IOPS enabled, underprovisioned override disabled, 1 MiB workload, 4096-byte payloads, 400 iterations, 64 workers, and the original 1000 application lifecycle operations/s floor. Preserve the existing 900-second combo supervisor and observer limits. No extra NAPI observability feature is required; set `MOUNT_RS_PROFILE_IO=1` before runtime construction.
- Build the pilot addon using the existing staged release pattern and explicitly forward the Cargo lock flag: `napi build --platform --release --no-js --output-dir "$napi_stage" -- --locked`. Produce exactly one `.node` outside the checkout. The installed NAPI CLI does not consume `CARGOFLAGS`. Clean source before and after the build, checkout SHA, addon SHA, and the pilot's exact six source SHA values form the closed build receipt. Runtime native identity must join that receipt.
- Keep the protected eleven Rust files and all existing runtime/provider/controller sources unchanged in this workflow slice. Their local dirty state is not hidden or incorporated into a clean CI claim. No local Cargo, native, Docker, or Node execution occurs while authoring this wiring.
- Retain controlled logs and public receipts for 14 days using `if: always()`. Exclude cache JWT/key files and raw worker output; exclude full-target `private/` and backing databases. Export only sanitized pilot JSON and fixed build/capacity JSON; private capabilities, CID handoffs, and raw fixture logs remain private.

## Files and Interfaces

| Path | Responsibility |
|---|---|
| `.github/workflows/remote-drives.yml` | Optional jobs, original-job guards, exact entrypoint invocation, clean build identity, bounded artifact export |
| `docs/superpowers/plans/2026-09-27-hosted-production-qualification.md` | Execution commands, acceptance criteria, evidence scopes, outstanding gates |
| `scripts/verify-owned-backing-pilot.mjs` | Separately owned fixed artifact checker; consumes `pilot.json` and `build.json`, emits a fixed verdict and exits nonzero for unavailable, unverified, or unsuccessful evidence |

The pilot checker interface is `node scripts/verify-owned-backing-pilot.mjs <pilot artifact> <build receipt>`. It checks the closed schema, selected native SHA join, observed backing evidence, live qualification, successful complete benchmark, and unchanged 1000 floor. It runs when an artifact exists even if the fixture wrapper failed; a later checker result cannot clear the earlier failure.

## Task 1: Review and statically verify dispatch wiring

**Files:** Modify the workflow and create this plan. Existing Rust entrypoints and fixture owners are read-only.

- [x] Inspect the signed CLI feature/platform gate, cache supervisor ownership/resource/credential contracts, full target script/configuration, and staged NAPI workflow precedent.
- [x] Add independent `signed-default`, `signed-profiled`, and `cache` matrix lanes with `fail-fast: false`; each Ubuntu/macOS combination gets its own job and target. Add separate full target and backing pilot jobs.
- [x] Bind native build artifacts to the exact integration-test executable; require debug assertions for local OIDC tests. Record clean checkout SHA and actual test/CLI SHA before execution; cache supervisor runtime SHA must match the build receipt.
- [x] Retain full-target public JSON, expected-state receipts, metric receipts, and worker audit logs with explicit paths excluding `private/`. The script already validates terminal success, ten unique worker PIDs, clean reap, and empty cleanup errors; workflow also verifies the runtime source/binary join.
- [x] Preserve pilot addon build failures with the fixed build exit status and compiler log in the job output. Failed builds never write a successful build receipt; private fixture logs and Engine/CID capabilities remain excluded from artifact export.
- [x] Parse YAML, run `bash -n` on every embedded run block, compile embedded Python without executing it, and inspect the diff. Verify new jobs are dispatch-only, every action is pinned, original job steps are unchanged, and no protected Rust hash changed. Current static receipt: 21 shell blocks and eight Python blocks parsed; duplicate YAML keys, original-job preservation, dispatch/resource/action/export contracts, one-test oracle examples, and all eleven protected SHA values checked successfully.
- [ ] Obtain independent review of the immutable workflow/checker diff and preserve the static receipt separately from eventual hosted results. No stage, commit, push, or dispatch is part of this authoring checkpoint.

## Task 2: Execute signed CLI and cache qualification on hosted platforms

**Consumes:** Reviewed committed workflow available on the selected remote branch; existing debug test entrypoints.

Select the reviewed branch and require a nonempty value. The proposed native dispatch is:

```sh
qualification_ref="$(git branch --show-current)"
test -n "$qualification_ref"
gh workflow run remote-drives.yml --ref "$qualification_ref" \
  -f tidb_saturation=false -f native_qualification=true \
  -f production_target=false -f owned_backing_pilot=false -f owned_engine_socket=''
```

The signed lanes independently invoke:

```sh
./scripts/cargo-shared test --locked -p mount-rs-cli --features local-oidc-fixture \
  --test configured_remote_compact configured_binary_selects_mrc5_for_signed_quic_and_websocket_reopen \
  -- --exact --nocapture --test-threads=1
./scripts/cargo-shared test --locked -p mount-rs-cli --features io-profiling,local-oidc-fixture \
  --test configured_remote_compact configured_binary_selects_mrc5_for_signed_quic_and_websocket_reopen \
  -- --exact --nocapture --test-threads=1
```

The cache lane invokes:

```sh
MOUNT_RS_TEN_PROCESS_RUN=1 \
MOUNT_RS_TEN_PROCESS_OUTPUT="$RUNNER_TEMP/mount-rs-cache-private" \
./scripts/cargo-shared test --locked -p mount-rs-cli --features local-oidc-fixture \
  --test ten_process_cache ten_process_sqlite_cache_qualification \
  -- --ignored --exact --nocapture --test-threads=1
```

- [ ] Retain all six hosted job outcomes and their artifacts. Require the named test and `1 passed; 0 failed; 0 ignored` in each successful log; no aggregate green status substitutes for missing execution.
- [ ] For cache acceptance require `receipt.complete=true`, `owned_cleanup_closed=true`, nonforced successful worker reap, group disappearance, retained unrelated sentinel, observed initial resource frame, nonzero sample count, and joined test/CLI hashes.
- [ ] Review cache generations, byte/metadata oracles, scope denials, RSS coverage, socket/lock release, and receipt failures independently. A platform preflight refusal leaves that platform unqualified.

## Task 3: Attempt the full production target without shrinkage

**Consumes:** Reviewed committed workflow and existing exact full-mode script.

```sh
gh workflow run remote-drives.yml --ref "$qualification_ref" \
  -f tidb_saturation=false -f native_qualification=false \
  -f production_target=true -f owned_backing_pilot=false -f owned_engine_socket=''
```

The job invokes:

```sh
MOUNT_RS_TARGET_MODE=full MOUNT_RS_TARGET_PROVIDER=sqlite \
MOUNT_RS_TARGET_DRIVES=10000 MOUNT_RS_TARGET_FILES=1000 MOUNT_RS_TARGET_SECONDS=30 \
MOUNT_RS_TARGET_OUTPUT="$RUNNER_TEMP/mount-rs-full-output" MOUNT_RS_PROFILE_IO=1 \
./scripts/bench-remote-production-target.sh
```

- [ ] Retain the exact attempted dimensions, build/source identity, controller log, terminal or partial journals, worker/resource/metric receipts, and first failure. Hosted CPU, memory, and disk suitability is unverified until observed.
- [ ] Require the exact controller test to execute once, successful full terminal receipt, all ten unique workers and clean reaps, complete online population, both modes/all patterns, fresh complete oracle, observed resource limits, and successful source/binary join.
- [ ] If a hosted resource floor fails, record the refusal and the remaining full-target gate. Do not enable control mode, reduce dimensions, lengthen deadlines, suppress observations, or lower disk/RSS floors to obtain green CI.

The initial payload alone is about 58.52 GiB. A local SQLite run retaining the 64 GiB free floor therefore needs over 122.52 GiB initial free disk before metadata and logs; this is an inference from the immutable workload, not a measurement of the hosted runner.

## Task 4: Execute the owned backing pilot and audit its artifact

**Consumes:** Reviewed workflow plus the fixed checker, a clean release addon build, and explicitly selected local Engine.

```sh
gh workflow run remote-drives.yml --ref "$qualification_ref" \
  -f tidb_saturation=false -f native_qualification=false \
  -f production_target=false -f owned_backing_pilot=true \
  -f owned_engine_socket=/var/run/docker.sock
```

- [ ] Before fixture launch retain a numeric Engine capacity receipt for the exact selected socket, preserving the 10737418240-byte memory and four-CPU floors. No global Engine survey or raw inspection artifact is exported.
- [ ] Run the fixed pilot through the existing fixture owners once, retain raw fixture output privately, print its fixed exit status, and preserve its first failure. Existing correctness, concurrent, NAPI, restart and ambiguous-commit owner checks retain their existing behavior.
- [ ] Run `node scripts/verify-owned-backing-pilot.mjs "$PILOT_PRIVATE/pilot.json" "$PILOT_PRIVATE/build.json"` when the artifact exists, including after benchmark failure. Export sanitized pilot/build/capacity JSON only.
- [ ] Accept only a verified native build/runtime join, complete observed backing evidence, live qualification, successful cleanup, and the unchanged successful 1000 floor. Wrapper exit zero alone is insufficient; an observed failed floor remains useful retained evidence and a failed job.

## Evidence Scopes and Remaining Gates

| Evidence | What it establishes | What remains open |
|---|---|---|
| Static YAML/Bash/Python checks | Reviewable workflow structure and shell/parser syntax | Hosted build and runtime execution |
| Existing fixture-free/mock observer checks | Capture, sanitization, parser and failure contracts | Real native load and observed Engine behavior |
| Signed CLI default/profiled jobs | One configured SQLite compact Drive, local ES256 authentication, QUIC/WebSocket byte/EOF/sync/reopen behavior; diagnostic opt-in assertions | External issuer, mounting, multiple hosts, full capacity |
| Ten-process cache job | Ten local debug CLI PIDs, five Partitions, ten Drives, sixteen 4 KiB files per Drive; generation logical banks, peer/disk/RAM faults and fresh backing/cleanup oracles | Exact phase counters, HTTP attempts, physical IOPS, peer transport bytes, Redis, ENOSPC, suppressed-reply-after-commit composition |
| Owned native backing pilot | Selected release addon joined to clean source/build identity, one owned durable TiDB/RustFS benchmark generation, observed backing/resource scope and original floor | Capacity target, cross-host durability, physical IOPS, global allocation, broader provider qualification |
| Successful full SQLite target | Complete requested local workload, ten independent servers, full routes/security/byte/resource/cleanup receipts | Production TiDB topology, cross-host capacity, external issuer and final platform/transport qualification |

The preserved local Docker VM was measured at 14 CPUs and 8318976000 bytes, below the unchanged 10 GiB fixture floor. This motivates a hosted route; it does not prove hosted suitability. The private local wrapper repeatedly failed during Cargo churn, so this plan changes workflow wiring without modifying its controller or treating a synthetic/private wrapper as public runtime evidence.

The existing 1000 lifecycle operations/s CI floor and 400/64/4096/65536 controls remain unchanged. Production TiDB ramps, real reply loss, directory cache discovery, external issuer, cross-host and mounting gates, strict touched-surface formatting/Clippy/tests, immutable packet review, and final branch acceptance remain outstanding until their own current receipts are accepted. None of the authored jobs has been dispatched at this checkpoint.
