# Filesystem flush barrier plan

## Contract

`BlockStore::flush` covers prior successful PUTs before metadata publication.
Filesystem PUT already completes the file, shard directory, root directory and
final device barriers before returning success, including duplicate publication.
There is no buffered successful PUT for flush to drain. Constructor barriers
persist the backing marker and root's parent entry.

Remove only the additional root-directory sync in flush. Keep its owned blocking
task and both backing identity checks. Keep every PUT and constructor barrier,
including the macOS device barriers, for both durability option values. Flush
does not acquire a new guarantee to drain an outstanding or cancelled PUT.

## Execution

1. Add per-store test observations for the existing root barriers and a gate
   between flush's two identity checks, retaining production behavior.
2. Execute `unix::tests::flush_after_acknowledged_put_does_not_repeat_root_barrier`
   against that behavior. Require one executed assertion failure with
   `acknowledged PUT flush repeated the root barrier`.
3. Delete the single flush root barrier. Preserve the exact assertion/control
   behavior; formatting may change only its presentation.
4. Run all provider tests, filesystem-filtered SDK/CLI integration, strict
   provider/SDK/CLI Clippy and formatting through `scripts/cargo-shared`.
5. Review the final source independently. Test late root/marker substitution,
   duplicate barriers, a paused PUT and a cancelled PUT with fresh reopened
   bytes. Release every test gate before final assertions and on unwind.
6. Build a fresh clean instrumented native executable and benchmark TiDB with
   filesystem and RustFS using the preserved geometry, full byte/EOF/membership
   oracles, scopes, process settlement, host reserve and evidence bounds.

The tests observe a blocking PUT's scoped completion and fresh bytes; they do
not establish a global blocking-task drain or power-loss proof. The existing
outer process supervisor bounds execution. Native results must establish any
performance claim: the earlier flush span included queueing and verification,
and does not predict the gain from deleting a single sync.

## Local evidence

The named test compiled and failed at the required assertion (one failure,
exit 101). After the deletion, all 31 provider tests passed and 28
filesystem-filtered SDK/CLI checks passed. Strict all-target provider/SDK/CLI
Clippy, formatting and independent source review also passed. These are local correctness checks;
current-source native performance, Linux runtime, CI and merge remain separate
gates.
