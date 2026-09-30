# Full Formal Qualification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an explicit hosted run of every committed named proof in the three existing Kani runners, preserving the evidence needed to distinguish verified, failed, and unexecuted harnesses.

**Architecture:** Add `formal_qualification`, defaulting to false, to the existing Remote Drives workflow. One isolated Ubuntu job installs the pinned tools and FoundationDB compile prerequisites, executes the unchanged runner scripts, and derives its expected proof set from current source. It retains one log per harness and accepts the run only when every expected harness actually verifies with all of its declared covers reached.

**Tech Stack:** GitHub Actions, Ubuntu 24.04, Rust 1.95.0, Kani 0.68.0, CBMC 6.11.0, Bash, Python 3 with `tomllib`, FoundationDB 7.4 client and embedded API 740 headers.

## Global Constraints

- Own only `.github/workflows/remote-drives.yml` and this plan; leave the eleven unrelated dirty Rust files untouched.
- Existing push, pull request, and default dispatch behavior remains unchanged. Selecting only `formal_qualification` suppresses the five original workflow jobs; other explicit qualification inputs continue to select their own jobs.
- Execute `scripts/verify-formal`, `scripts/verify-remote-formal`, and `scripts/verify-cache-formal` without changing their commands or proof assumptions.
- Preserve the original 52 package/harness identities from commit `9904c87251cca2e9d32007b876ebf7a8d71b7bce` as an inventory baseline. Discover additions from the current checkout rather than imposing a maximum count. The coordinated client slice adds three remote-client harnesses, making 55 once committed.
- Use `CARGO_BUILD_JOBS=1`, a 100-minute job limit, and separate `CARGO_TARGET_DIR`, `KANI_HOME`, and evidence directories below `RUNNER_TEMP`.
- Use the repository's pinned checkout, Rust 1.95, and artifact actions. Install `kani-verifier` exactly at version 0.68.0 through `scripts/cargo-shared install --locked`; the Kani action's transitive stable toolchain is deliberately avoided.
- Kani 0.68 does not forward `--locked` to proof builds. Record both root and FSKit lock bytes/digests before and after; never describe that check as a locked proof build.
- FSKit is a separate workspace with no committed lock. Generate its ignored lock explicitly with the selected host toolchain and retain the resolved lock.
- Install `clang` and `libclang-dev` for bindgen. Use the existing official Linux amd64 FoundationDB 7.4.7 image pin `sha256:c13aed110fe17c6feb678f5dadc73dbcc2dbc42f2a3e952a877c116f63f052d3` to copy `/usr/lib/libfdb_c.so` from a run-labelled, stopped container. Never start that container or a database server.
- Remove only the captured client container ID after checking its ID, stopped state, and run label. Record cleanup failure; do not discover or remove other containers.
- This route runs current committed numeric and protocol decisions. It does not recreate or execute any historical pre-fix FoundationDB proof or rejected reproduction.
- Symbolic proof results remain separate from provider integration, network behavior, cache fault tests, performance, filesystem mounting, and the 10,000-client target.

---

### Task 1: Wire and review the explicit hosted proof route

**Files:**

- Modify: `.github/workflows/remote-drives.yml`
- Create: `docs/superpowers/plans/2026-09-27-full-formal-qualification.md`
- Read only: `scripts/verify-formal`, `scripts/verify-remote-formal`, `scripts/verify-cache-formal`, `scripts/cargo-shared`, and the committed harness sources.

**Interfaces:**

- Consumes: `workflow_dispatch` boolean input `formal_qualification`; current package/harness declarations and unchanged runner invocations.
- Produces: `mount-rs.formal-inventory.v1`, `mount-rs.formal-tools.v1`, and `mount-rs.formal-qualification.v1` JSON receipts; original lane logs and exit codes; `harnesses/<package>--<harness>.log`; root and FSKit locks; source, client, tool binary, and generated model SHA-256 manifests.

- [x] **Step 1: Reconcile current source inventory and prerequisite semantics.**

  The original runner inventory is 44 general + 7 remote + 1 cache. The workflow derives package and harness identities from named commands, maps them to current `#[kani::proof]` declarations, records source paths, line numbers, declared unwind attributes and lexical cover counts, and requires exact set equality. It also requires the baseline 52 identities to remain present. Unsupported or duplicate declarations fail before proof execution. Lexical cover counts are informational provenance and a minimum, not an exact expected Kani count: the two block-id grammar proofs call a production helper containing four covers, so both have an audited minimum of four despite having no direct cover macro in their proof bodies.

  Primary Kani source was checked at tag `kani-0.68.0`, commit `0d2328a93f0e0ff66132d6bfa1a7d884877cf862`: `src/setup.rs` defines `KANI_HOME/kani-<VERSION>`; `src/lib.rs` dispatches the release driver; `kani-driver/src/args/cargo.rs` and `call_cargo.rs` show the lack of locked forwarding; `harness_runner.rs` and `cbmc_property_renderer.rs` define the exact checking, completion, verification, and cover output used by the collector.

- [x] **Step 2: Implement the route while preserving existing jobs.**

  Add only the input, its inclusion in the five original job guards, and the new job. Build/install/setup logs are retained. Host and actual bundled Kani compiler, Cargo, solver and driver binaries are hashed; versions are recorded. No product binary is built by this symbolic route. Generated model identities are retained separately.

  Execute each original runner under Bash tracing:

  ```bash
  PS4='+ ' bash -x ./scripts/verify-formal
  PS4='+ ' bash -x ./scripts/verify-remote-formal
  PS4='+ ' bash -x ./scripts/verify-cache-formal
  ```

  The actual workflow redirects each runner to its own retained log and saves its exit code. The next runner runs after an earlier runner failure if tool setup succeeded and the job was not cancelled. Each runner's original `set -e` behavior is preserved, so remaining commands in that runner are explicitly marked `unexecuted`.

- [x] **Step 3: Validate the authored workflow and collector without invoking native tools.**

  Parse YAML with duplicate-key rejection. Compare all existing job bodies against `9904c872` after normalizing only the added original guards. Run `bash -n` on each embedded Bash block and Python AST parsing on each embedded Python block. Check that every action reference is a verified 40-character pin.

  Exercise the exact inline collector against tiny owned fixtures. Require a healthy one-harness trace and a zero-direct-cover/four-helper-cover trace to qualify; independently reject absent checking/completion, zero executed, mismatched harness, failed checks, duplicate invocation/verification, missing covers, unreachable covers, unexecuted later harnesses, changed source/locks, incomplete cleanup, and missing model/tool evidence. Require all actually reported covers to be reached and at least the audited source/helper minimum; a harness with neither direct nor helper covers has no invented cover obligation.

  These fixture checks validate the collector, not the actual proofs. Native Kani, Cargo, Docker and network-backed prerequisite execution remain unrun until the reviewed source is committed and the root dispatches it.

  The authored collector passed 37 controlled cases: four healthy cases (helper-generated covers, colored successful output, expanded actual covers, and no-cover proof) and 33 independent rejection cases. Raw logs and their hashes remain intact; parsing removes only ANSI SGR display codes. The candidate-source inventory reconciled all 55 declarations with 44 general, 10 remote and 1 cache named invocations while preserving the baseline 52 identities. Duplicate-key YAML parsing, all seven new Bash syntax blocks, all three Python AST blocks, pinned action references, and comparison of all eight existing job bodies also passed. These are static and synthetic checks; they do not establish a live proof result.

- [ ] **Step 4: Independently review, commit, and execute the selected current source.**

  Root stages exactly the two owned paths after independent workflow review and static validation. Once the coordinated three client proof declarations and runner names are committed, dispatch:

  ```bash
  gh workflow run remote-drives.yml --ref codex/remote-production-qualification -f formal_qualification=true
  ```

  Inspect the resulting specific job and downloaded artifact against its checkout SHA. Success requires every expected harness's matching checking line, exactly one successful verification, positive check count with zero failures, exactly one 1/0/1 completion, all declared covers reached, zero runner exits, unchanged source and lock digests, retained model/tool identities, and exact client cleanup. A setup failure, incomplete runner, timeout or absent artifact is not a passing formal qualification.

## Scope and remaining risks

- The source inventory uses the current bounded harness declaration style and lexical brace/cover inspection, not a general Rust parser. Unsupported source shapes fail visibly; future proof structure changes need collector review.
- FSKit's generated dependency lock is retained and checked for stability during this run, but its initial resolution can differ between runs. A committed FSKit lock would be a separate source policy change.
- Package repository and Kani release downloads may fail, and a hosted cold proof suite may exceed the job time limit. Those outcomes retain incomplete evidence and cannot qualify a reduced subset.
- FoundationDB client loading/build feasibility is established only when the hosted job actually reaches the three current feature-on proofs. Extracting a client is not FoundationDB service or durability qualification.
- Successful bounded proofs establish the current harness assumptions and unwind domains only. Async cancellation, backing authority, crash/power-loss durability, distributed cache behavior, allocations, IOPS and production scale need their separate runtime evidence.
