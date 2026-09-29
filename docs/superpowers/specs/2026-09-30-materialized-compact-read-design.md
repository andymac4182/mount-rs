# Certify fresh materialized compact guards

The current TiDB complete selected read reconstructs a validated guard, encodes
the complete anchor and node to JSON, then parses those reconstructed bytes to
compare with a retained expectation. In the measured 1,000-file read cycle,
Stat accounts for 8,846 of 9,485 Rust allocation requests. Complete fresh SQL
membership and root-entry checks remain required by the existing fstat contract.

Add one core factory on `CompactInodeRead` that consumes a fresh materialized
guard and borrows its coherent complete anchor and expected body. It may return
the existing checked unchanged receipt when complete typed equality proves the
same fields as the canonical streamed comparator. It returns the existing
validated owned result for valid mismatches or unsupported expectation shapes.

The exact-match path validates the fresh anchor, actual selected membership,
physical identity, inode, generation, complete body and eligible expectation.
For a sealed audited root, exact body equality inherits kind/name/duplicate
validity from the complete audit. Every audited child must still be present in
the actual fresh sorted membership. This permits an allocation-free comparison
and receipt construction using the existing shared structure. For an ordinary
file expectation, validate its complete matched body before constructing a
receipt. Public expectation construction is never a freshness or validity proof.

Any provisional mismatch or invalidity runs `LoadedCompactInode::from_guard`
once to preserve the existing fallback result and error precedence. A mismatched
passed backing ID prevents certification; a valid fresh guard still returns
Loaded, as the existing comparator does. Invalid actual authority, identity,
membership or body retains its existing error. Do not add equality of the whole
fresh anchor with the retained one: existing controls explicitly accept valid
changed defaults, allocation bounds, unused members and current root fields.

TiDB replaces only reconstructed encoding/certification inside its existing
complete transaction. Preserve fresh authority decoding, complete membership,
physical header/hash/ordinal/name/count checks, backing verification, rollback,
caller generation recovery, local revision checks, write-only handle Stat and
open-unlinked/orphan behavior. The factory describes validation, never SQL
snapshot provenance. No statement, schema, protocol or acknowledgment changes.

Alternatives considered: returning Loaded unconditionally reuses the caller's
typed root check but loses the existing checked-read contract; selected-only
Stat reduces SQL work but drops the complete-root audit. The materialized factory
removes redundant work while preserving that contract. SQL row decoding and
physical validation collections remain separate allocation sources.

Verification requires a behavioral RED against the actual old encode/streamed
path, followed by zero-allocation exact root/file comparison controls and a
mutation matrix checked against the old reference. Then run core/provider and
chunked controls, actual TiDB compact faults/corruption, unchanged allocation
ceiling diagnostics and a source-bound comparison. Zero allocation for this core
factory excludes SQL, decoded-row construction, async futures and the caller.
It does not establish zero allocations for the complete metadata operation.
