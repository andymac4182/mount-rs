# mount-rs Formal Verification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use subagent-driven-development or executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a repeatable, scoped formal proof to mount-rs and build a path from core metadata checks toward protocol and unsafe boundary verification.

**Architecture:** Keep proof harnesses next to the production functions under `#[cfg(kani)]`, so the verifier checks the code that ships. Start with `mount-rs-core`'s file-layout validator, which runs before loaded metadata is trusted. Run a pinned Kani version in a separate Linux CI job; retain the existing Rust, provider, native mount, and end-to-end gates for properties outside the proof.

**Tech Stack:** Rust 1.95 / edition 2024, Kani 0.68.0, GitHub Actions, existing `scripts/cargo-shared` wrapper.

## Global constraints

- Use an isolated checkout for implementation. The current `codex/concurrent-storage-providers` checkout has unrelated modified and untracked files, including `Cargo.toml`, `Cargo.lock`, and `src/storage.rs`. The first proof target is unchanged by the current `src/storage.rs` diff.
- Keep the proof in the crate that owns the production function. Do not copy its implementation into a verification-only model.
- Do not lower `rust-version = "1.95"`, change edition 2024, or add a runtime Kani dependency to make a proof pass.
- Run normal local Rust commands through `./scripts/cargo-shared` and keep build output out of the checkout, as `AGENTS.md` requires.
- A successful bounded proof applies only to the harness input domain and properties recorded below. It does not prove block contents, I/O, concurrent publication, kernel mounts, or the soundness of all FFI callbacks.

---

## Why this first slice

Three viable starting approaches were considered:

| Approach | Benefit | Cost / limit |
| --- | --- | --- |
| **Kani on the metadata trust boundary (chosen)** | Checks all values within a stated input shape and can run in CI against the actual Rust function | Bounds on collection size; proof cost and compiler compatibility must be measured |
| Kani on `FixedSizeChunker::locate` | Very small arithmetic proof, likely quick to bootstrap | Lower risk reduction because current tests already cover the main offset cases |
| Deductive contracts across storage and unsafe bridges first | Could express broader or unbounded claims | Requires substantial specifications and tool integration before the first verified result |

The first proved claim concerns the production `src/storage.rs::validate_file_extents` helper, which `validate_file_layout` calls after chunker configuration validation. For one extent with arbitrary full-range `u64` file size, file offset, block offset, and length and a fixed nonempty block identity, an accepted extent has positive length, checked file and block ends, and a file end within the file size. Zero length and overflowing file or block ranges are rejected. The Kani harness does not verify chunker configuration or the complete `validate_file_layout` call. Ordering and overlap need a later bounded multi-extent harness.

## Task 1: Prove single-extent validation in the real core crate

**Files**

- Modify: `Cargo.toml` (`mount-rs-core` lint configuration)
- Modify: `src/storage.rs` (adjacent proof module after `validate_file_layout`)

**Interface**

- Consumes: private `validate_file_extents(&[BlockExtent], u64) -> Result<()>`, called by `validate_file_layout` after `from_config`, in `src/storage.rs`.
- Produces: Kani harness `single_extent_validation`; normal Rust builds compile without Kani.

- [ ] **Step 1: Establish the implementation baseline.** Create an isolated checkout from current `origin/main`, check `git status --short`, and confirm `validate_file_layout` still has the checked-add and ordering behavior at `src/storage.rs:224-255`. Record the starting SHA. The plan is currently untracked in the dirty source checkout; copy it into the isolated checkout before editing:

  ```sh
  mkdir -p docs/superpowers/plans
  cp /Users/amcclenaghan/github/andymac4182/mount-rs/docs/superpowers/plans/2026-09-23-formal-verification.md docs/superpowers/plans/2026-09-23-formal-verification.md
  ```

  Keep the existing dirty checkout untouched.

- [ ] **Step 2: Install and smoke-check the pinned tool.** On a supported Linux or macOS host, run:

  ```sh
  ./scripts/cargo-shared install --locked kani-verifier --version 0.68.0
  ./scripts/cargo-shared kani setup
  ./scripts/cargo-shared kani --version
  ```

  Require version `0.68.0`. Kani 0.68.0 bundles nightly Rust 2026-08-21; its predecessor 0.67.0 failed crates declaring Rust 1.95. The release evidence suggests compatibility, but this actual package has not been compiled with Kani yet.

- [ ] **Step 3: Teach ordinary Cargo about `cfg(kani)`.** Add to the root package manifest:

  ```toml
  [lints.rust]
  unexpected_cfgs = { level = "warn", check-cfg = ['cfg(kani)'] }
  ```

- [ ] **Step 4: Make the compiler and failure path observable.** Add this temporary module beside `validate_file_layout`:

  ```rust
  #[cfg(kani)]
  mod verification {
      #[kani::proof]
      fn single_extent_validation() {
          assert!(false);
      }
  }
  ```

  Run `./scripts/cargo-shared kani -p mount-rs-core --harness single_extent_validation`. Require that Cargo accepts the unchanged Rust 1.95 / edition 2024 manifest, Kani names the harness, and the command fails on the deliberate assertion. If compilation fails before the assertion, keep this task open and reduce the issue to a small upstream repro; do not weaken the manifest.

- [ ] **Step 5: Replace the temporary assertion with the bounded proof.** The initial harness called `validate_file_layout` with `FixedSizeChunker::new(4096).unwrap().config()` and a single extent. Kani expanded the fixed chunker configuration's `BTreeMap` implementation into a large unwinding tree, so the run was interrupted before verification. Extract the existing extent loop unchanged into private `validate_file_extents(&[BlockExtent], u64) -> Result<()>`; keep `from_config(&layout.chunker)?` in `validate_file_layout`, then call the helper. The final harness invokes this production helper with an array containing one extent, symbolic full-range `u64` values, and fixed nonempty block ID. It checks accepted bounds and required rejection paths and covers accepted, zero length, file overflow, block overflow, and file-size rejection. This deliberately narrows the claim to extent validation. No validator logic is copied into the harness.

- [ ] **Step 6: Run and audit the proof.** Run `./scripts/cargo-shared kani -p mount-rs-core --harness single_extent_validation`. Require `VERIFICATION:- SUCCESSFUL`, reached cover conditions for accepted, zero-length, file overflow, block overflow, and file-size rejection, and no unwinding assertion or unsupported-feature error. If unwind 16 is too low, try 32 and record the smallest passing bound; a timeout or solver error is not a proof. If an assertion fails, preserve the counterexample, write a focused ordinary Rust regression test, fix the production function, and rerun the proof. The final harness proves the extracted production helper directly; `validate_file_layout` invokes it after separate chunker configuration validation. The passing local run used Kani 0.68.0, CBMC 6.11.0, and unwind 16: `VERIFICATION:- SUCCESSFUL`, zero failed checks, and all five cover properties satisfied. Kani warned about unsupported constructs in the compiled crate, but their instrumented checks passed as unreachable in this harness.

- [ ] **Step 7: Check normal Rust gates and the diff.** Run:

  ```sh
  ./scripts/cargo-shared fmt --all -- --check
  ./scripts/cargo-shared clippy -p mount-rs-core --all-targets --locked -- -D warnings
  ./scripts/cargo-shared test -p mount-rs-core --all-targets --locked
  git diff --check
  ```

  Require all commands to pass. Confirm no change to `Cargo.lock` from a Kani dependency.

- [ ] **Step 8: Commit the proved core slice.** Inspect staged names, then run `git add Cargo.toml src/storage.rs docs/superpowers/plans/2026-09-23-formal-verification.md` and `git commit -m "verify: prove single-extent metadata bounds"`. The commit contains only the validated proof, lint configuration, and this plan.

## Task 2: Make the proof repeatable in CI and document its claim

**Files**

- Modify: `.github/workflows/ci.yml` (separate Kani job next to the Rust matrix)
- Create: `docs/formal-verification.md` (proof inventory and limitations)

**Interface**

- Consumes: `single_extent_validation` from Task 1.
- Produces: a required CI job that checks the same harness with Kani 0.68.0 and a source-level record of its input domain.

- [ ] **Step 1: Add the pinned Linux job.** Add a separate job rather than expanding the existing macOS/Windows Rust matrix:

  ```yaml
  kani-core:
    runs-on: ubuntu-24.04
    timeout-minutes: 20
    permissions:
      contents: read
    steps:
      - uses: actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683 # v4.2.2
      - uses: dtolnay/rust-toolchain@46817827a5bfabe028bf34e1cce71fd40e2ff697 # 1.95.0
      - name: Verify core file-layout bounds
        uses: model-checking/kani-github-action@69d357bade1eb7b32bc3fc3d5c3d173c5bfa5237 # v1
        with:
          kani-version: '0.68.0'
          args: '-p mount-rs-core --harness single_extent_validation'
  ```

  The Kani CI guide still says Ubuntu 20.04 while the action README uses `ubuntu-latest`; verify that the chosen Ubuntu 24.04 runner actually installs and runs Kani before treating the job as a gate.

- [ ] **Step 2: Record the proof obligation.** In `docs/formal-verification.md`, state the exact claim from Task 1: direct proof of `validate_file_extents`, one nonempty block ID, arbitrary full-range `u64` numeric inputs, and one extent. State that chunker configuration and the complete `validate_file_layout` call remain outside this harness. Record the final unwind bound, tool version, harness name, reached cover cases, and the exact CI command. Give each future proof a separate row with its own input assumptions and limits.

- [ ] **Step 3: Commit the CI and inventory slice.** Review `git diff --check` and staged paths. Run `git add .github/workflows/ci.yml docs/formal-verification.md` and `git commit -m "ci: gate core metadata proof with Kani"`. This commit contains only the CI job and proof inventory.

- [ ] **Step 4: Validate the hosted job before making it required.** Push the isolated verification branch and inspect one exact-SHA PR or dispatch run. Require the action to install Kani 0.68.0, discover exactly the named harness, report successful verification and reached cover cases, and finish within 20 minutes on Ubuntu 24.04. Keep the existing `rust` matrix green on Ubuntu, macOS, and Windows. Only after that evidence is available, mark `kani-core` required in repository branch protection if branch protection is managed for this repository. Report local proof status, hosted CI status, and any remaining verification limits separately.

## Follow-on proof slices (separate reviewable plans)

1. **Multi-extent layout and namespace:** Extend `validate_file_layout` to 0-3 extents and prove accepted extents are ordered and nonoverlapping, with checked ends. Then bound a small `Namespace::validate` graph and prove malformed root/link references are rejected. State the node and extent count bounds in each harness.
2. **Untrusted protocol lengths:** Prove `transports/mount-rs-9p/src/wire.rs::P9Reader::need/raw` and the NFS XDR credential boundary reject truncated or oversized buffers without advancing past the input. Bound payload length to a tractable value, include exact configured limits as separate boundary cases, and retain codec/fuzz and live protocol tests.
3. **Unsafe native boundaries:** Inventory the safety contracts and pointer provenance for `bindings/mount-rs-sqlite-vfs/src/lib.rs` callbacks and `transports/mount-rs-fskit/src/lib.rs` C ABI. Prove extracted length/capacity and ownership state transitions under explicit caller contracts; use Miri on mock-backed executable tests where platform APIs permit. Kani does not establish C caller validity, reference aliasing, or data-race freedom by itself.
4. **Publication ordering:** Model the pure state transitions around block flush, metadata CAS, conflict, and ambiguous publication in `filesystems/mount-rs-chunked`. Prove that a known failed publication does not expose new references and that an uncertain commit stays ambiguous. Qualify real SQLite/PGlite/FoundationDB and cross-host behavior with the existing runtime gates.

Each slice starts with a written property and input model, then a harness with reachable success and rejection branches, a passing pinned-tool run, a CI gate, and a proof-inventory update. A green proof is reported with its exact bounds and source revision, not as verification of the whole workspace.

## Sources used for tool choice

- AWS, [Verify the Safety of the Rust Standard Library](https://aws.amazon.com/blogs/opensource/verify-the-safety-of-the-rust-standard-library/): scoped goals, tool-agnostic verification, CI, and Kani's bounded scope.
- Kani, [Using Kani](https://model-checking.github.io/kani/usage.html), [Where to start on real code](https://model-checking.github.io/kani/tutorial-real-code.html), [Loop unwinding](https://model-checking.github.io/kani/tutorial-loop-unwinding.html), and [Undefined behaviour](https://model-checking.github.io/kani/undefined-behaviour.html).
- Kani [0.68.0 release](https://github.com/model-checking/kani/releases/tag/kani-0.68.0), [Rust 1.95 compatibility issue](https://github.com/model-checking/kani/issues/4685), and [GitHub Action](https://github.com/model-checking/kani-github-action).
