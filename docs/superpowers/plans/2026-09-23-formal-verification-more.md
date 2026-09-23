# mount-rs Formal Verification: Next Production Decisions

**Goal:** Extend the local Kani inventory beyond metadata, wire framing, and lease acquisition into production decisions that can currently overflow, misclassify an allocation, renew a stale pin, or delete an object prematurely.

**Design:** Keep each harness beside the production code it calls. Extract small, allocation-free decisions only where an existing parser, transaction, or reconciliation loop already uses them. Compare each symbolic result with an independent arithmetic or path oracle. Record each harness's assumptions, reached covers, and boundaries in `docs/formal-verification.md`. A proof is included only after a successful local Kani run.

**Starting point:** `origin/main` at `8d438764` (merged PR #11, 30 executed harnesses). Work in the separate `mount-rs-formal-verification` checkout. Preserve the unrelated plan edit in the original checkout.

## Implementation slices

1. **NFS TCP record budget** — Add an ordinary regression for an empty push at `usize::MAX`, where the current `limit + 4` can overflow. Repair the production budget calculation with checked or widened arithmetic. Prove unrestricted `usize` counters and limit against a `u128` oracle, covering exact fit, over limit, and overflow. Scope: `transports/mount-rs-nfs/src/rpc.rs`.
2. **FSKit frame decode** — Extract a production-called header/length classifier before body allocation. Prove arbitrary `u32` declared body length and `usize` received size only admit a supported 20-byte header with exactly the declared body and no more than 1 MiB. Cover short, oversized, truncated, extra, and valid frames. Scope: `transports/mount-rs-fskit/src/lib.rs`.
3. **SQLite and PGlite read-pin renewal** — Extract exact token/liveness/checked-expiry decisions used inside the SQL transactions. Prove unrestricted admitted signed clocks and identities with an independent `i128` oracle. Prove signed TTL/token conversion if tractable. Retain finite provider tests of the SQL `WHERE`, commit, and returned token. Scope: both provider `storage.rs` files and package-local `cfg(kani)` lint configuration.
4. **FoundationDB authority time** — Prove the production sampler over unrestricted `u64` current, proposed, and forward-jump limit with optional values. Accepted samples never regress; zero and excessive forward jumps reject. Scope: `providers/mount-rs-foundationdb/src/lib.rs`.
5. **Object-store deletion eligibility** — Add a failing regression showing that a negative timestamp can currently be deleted; retain unknown timestamps. Extract the reconciliation classifier and prove only valid, in-scope, nonlive, definitely old objects are eligible under unrestricted admitted numeric inputs. Scope: `providers/mount-rs-object-store-blocks/src/lib.rs`.
6. **N-API host open flags** — Extract the host-bit classifier after JS numeric coercion. Prove arbitrary `u32` bits map access and create/exclusive/truncate/append rights exactly, unknown bits add no rights, and read-only truncate cannot pass the shared access guard. Keep executable JavaScript coercion tests. Scope: `bindings/mount-rs-napi/src/lib.rs`.
7. **Core directory graph and WebDAV lock scope** — Attempt bounded proofs only if their predicates can be connected to the real `Namespace::validate` or `DavLockTable` decisions and the proof completes. Keep finite graph and canonical-path matrices for behavior outside the bound. Scope: `src/storage.rs` and `transports/mount-rs-webdav/src/locks.rs`.

## Verification and delivery

- Run the touched package tests, strict Clippy, and formatting checks with `./scripts/cargo-shared`, using the separate FSKit workspace and feature-on FoundationDB tests where needed.
- Run each new harness separately, then `scripts/verify-formal` as the complete local proof gate. Audit reached covers and unwinding assertions. Keep unsupported or timed-out attempts out of the proved inventory.
- Run full workspace formatting, strict Clippy, and all-target tests. Review the complete diff, update the runner and inventory, and commit only the intended files.
- Push a reviewable PR, attach it to this task, merge it into main, and verify the main commit. Hosted CI is outside this delivery check at the user's request.

The scalar proofs do not establish SQL transaction atomicity, FoundationDB commit acknowledgement, remote object-store consistency, native caller memory validity, concurrent graph mutation, or network scheduling.
