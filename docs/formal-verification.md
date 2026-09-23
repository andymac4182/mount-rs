# Formal verification inventory

This inventory records the precise input domain and property of each proof. A passing bounded proof applies to its stated harness, not to the whole workspace.

| Harness | Production code and input domain | Proved property | Bound and limits |
| --- | --- | --- | --- |
| `single_extent_validation` | Calls `src/storage.rs::validate_file_extents` directly with exactly one `BlockExtent`, a fixed nonempty `BlockId` (`"block"`), and four independent, arbitrary full-range `u64` values: file offset, block offset, extent length, and file size. | Every accepted extent has positive length, a checked file end, a checked block end, and a file end no greater than file size. Zero length, overflowing file or block ranges, and a file end beyond file size are rejected. Five cover cases are reached: acceptance, zero length, file overflow, block overflow, and file-size rejection. | `#[kani::unwind(16)]`; Kani 0.68.0 and CBMC 6.11.0. The fixed block ID excludes empty-ID rejection. A single extent cannot exercise ordering or overlap. `from_config` and the complete `validate_file_layout` call are outside this harness. |

Run the local proof from the repository root with:

```sh
./scripts/cargo-shared kani -p mount-rs-core --harness single_extent_validation
```

The separate `kani-core` job in `.github/workflows/ci.yml` runs on Ubuntu 24.04 with Kani 0.68.0 and a 20-minute timeout. Its Kani action receives the exact arguments `-p mount-rs-core --harness single_extent_validation`. The workflow pins the checkout, outer Rust toolchain action, and Kani action revisions; the Kani action also installs a moving stable Rust toolchain internally. The local macOS proof returned `VERIFICATION:- SUCCESSFUL`, zero failed checks, and all five cover properties satisfied. Before making the job required, qualify an exact-revision hosted Ubuntu run for tool installation, harness discovery, successful verification, all five covers, and completion within the timeout.

Kani also emitted global unsupported-construct warnings for `caller_location (1)` and `foreign function (2)` in the compiled crate. The associated instrumented checks reported `SUCCESS` because those paths were unreachable in this harness. That result does not extend the proof to code paths using those constructs.

The ordinary Rust test `storage::tests::validates_chunkers_and_extent_ranges_without_fetching_blocks` exercises the full `validate_file_layout` path through `Namespace::validate`, including chunker configuration and extent validation. It checks the production wiring but is not a formal proof of that path. I/O, concurrency, kernel mounts, FFI, and their interactions are outside this proof.

## Planned proof slices

Each future harness needs its own property, input assumptions, bound, and result before it enters the proved inventory.

| Candidate | Proposed input assumptions | Limits to report |
| --- | --- | --- |
| Multi-extent layout and namespace | Bound file layouts to 0–3 extents and a small namespace graph; specify node and link bounds in the harness. | Ordering, overlap, checked ends, and root/link rejection only within those bounds; storage behavior remains outside the model. |
| Protocol lengths | Bound 9P and NFS XDR payload lengths and test configured maximum lengths as separate boundary cases. | Buffer parsing properties only; network transport and peer behavior require runtime tests. |
| Unsafe native boundaries | State caller contracts for SQLite VFS and FSKit pointers, lengths, capacities, and ownership transitions. | C caller validity, aliasing, platform APIs, and data-race freedom need separate evidence. |
| Publication ordering | Model block flush, metadata compare-and-swap, conflict, and ambiguous commit as pure bounded state transitions. | Real SQLite, PGlite, FoundationDB, and cross-host behavior require runtime gates. |
