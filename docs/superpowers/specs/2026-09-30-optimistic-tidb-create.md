# Optimistic TiDB root-file create

## Goal

Remove the duplicate root proof read in the common `wx+` population path without
changing the locked structural publication validator. This advances the TiDB +
RustFS scaling work; complete membership/dentry scans and allocation costs remain.

## Design

TiDB explicitly advertises `CompactOptimisticCreateCapability::Supported`.
Other providers default to `Unsupported`. The SDK forwards the capability through
its erased metadata adapter without adding a diagnostic-bank slot.

An unguarded normalized root-child `wx+` whose name is absent from the runtime
cache may construct a `CompactRootFileCreate` directly from the opaque complete
audit witness. `capture_audited` constructs an expectation and grants no freshness.
The runtime root body and physical identity remain paired with that witness.

The existing publication transaction still locks the authority row, reads the
complete fresh membership and parent body/dentries, validates the proposal,
preflights all encodings, applies targeted DML and commits. Backing verification,
blob flush, unknown-COMMIT handling, receipt checks and cache installation retain
their existing order. Independent file updates do not acquire the authority lock.

## Refusal and retry

- Cached occupied names and all supplied path guards use the original fresh
  preflight before returning pathname errors.
- Preparation errors use the fresh path before being returned. A peer can repair
  a cached overflowing parent timestamp with a newer structural generation.
- After an optimistic proven `EAGAIN`, fresh root preflight runs before Full
  refresh. It refuses same-generation root identity/body drift rather than
  adopting it into a new audited witness.
- Unknown commits, canceled publication and forged receipts poison the owner;
  they are never replayed. A fresh observer is a separate outcome check.

## Acceptance

Require zero separate root reads and one structural publication for a clean
opt-in create, with a fresh byte/EOF/sibling oracle. Test peer same-name creation,
peer unrelated creation, cached-present remote unlink, guarded-create precedence,
timestamp recovery, same-generation drift, cancellation and lost acknowledgments.
Actual TiDB controls must retain fresh complete-parent/member corruption refusal
before DML and compare every persisted row with an independent post-close observer.

This changes neither the SQL representation nor the wire protocol. It does not
qualify bounded structural work, physical IOPS, the production fleet target or
crash durability. New throughput requires a source-bound matched benchmark.
