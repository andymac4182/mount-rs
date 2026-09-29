# Move validated TiDB directory buffers

The current scoped create benchmark at 1,000 files requests 8,111 Rust
allocations. Source inspection identifies a complete extra name copy in
`materialize_directory`: the SQL driver already owns each name buffer, but the
provider clones it into `DirectoryEntry`. Publication construction later clones
the changed parent again; that separate ownership boundary remains for this cut.

Consume the freshly decoded `Vec<StoredEntry>` when materializing a directory.
Validate the complete physical rows first, then move each `String` into its
logical entry. Structural publication retains physical ordinals separately and
passes borrowed logical names/inodes plus those ordinals to the existing physical
write planner. Selected loads and full snapshots also consume their row maps.

Keep one shared planner over borrowed physical entry views. Its existing adapter
accepts stored rows for controls; its production adapter accepts ordinals and
materialized entries. Reject unequal vector lengths. Preserve name uniqueness,
ordering, signed bounds, ordinal exhaustion, generic reorder rewrites, rename,
unlink and insertion between existing entries. FileCreate does not imply append.

The authority/affected guard locks, Read Committed fresh complete rows, logical
validator and same-identity captured-body checks remain unchanged. Preserve
physical error checks before logical validation, packet preflight before DML,
COMMIT before acknowledgement and refusal to replay uncertain outcomes.
No schema, wire, core ownership or storage configuration change is required.

Compare the alternatives explicitly: borrowed captured-body certification could
avoid materialization, but requires a new shared validator seam; aggregate SQL
framing could reduce driver row allocations, but adds packet/truncation and
execution-plan obligations. Moving existing buffers removes a known copy with
the current validator and physical representation.

Verification starts with a buffer-ownership control that fails against the
existing copying path while its original inputs stay alive. Then run the
existing physical planning controls, actual compact corruption/fault suite and
the unchanged paired publication benchmark. Measure latency as well as allocation
traffic; reduced copying alone does not establish faster publication.

Repair the existing selected-read allocation test's exact owned cleanup to cover
all seven metadata table families and verify zero remaining rows. Preserve its
fresh full/byte oracles, randomized key, actor shutdown, allocation ceiling and
failure retention. Actual checks use the existing pinned disposable TiDB fixture;
this remains distinct from RustFS and full production-capacity qualification.
