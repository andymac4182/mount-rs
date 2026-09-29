# TiDB structural publication lock scope plan

1. Audit all compact membership and directory mutators for authority ownership.
2. Add bounded actual-TiDB diagnostics and capture the current write-key footprint.
3. Add correctness controls for fresh reads after transaction start and unrelated
   selected-file writes overlapping structural publication.
4. Demonstrate the sibling-independent write-key regression fails on current code.
5. Change only structural directory reads to nonlocking complete reads; retain
   authority and guard locks and all full validation.
6. Run actual controls, local tests, formatting, strict Clippy, and source review.
7. Build and pin a new release executable; repeat the same native benchmark.
8. Publish measured results, update PR 34, and retain unsuccessful gates explicitly.

The production-scale goal remains active until all required integration, capacity,
and merge gates are satisfied. This targeted diagnostic is not production-scale
qualification.
