# Controlled TiDB/RustFS layout comparison

## Evidence boundary

The earlier owned pilot used the `mount-rs-split-tidb-r2` constructor and a
legacy layout. It failed the unchanged 1,000 logical operations/second floor.
That result does not establish a compact-layout speedup. The new comparison
must select `rustfs` explicitly in both arms; the R2 qualification contract
remains unchanged.

## Implementation order

1. Add a separate `mount-rs-split-tidb-rustfs` benchmark provider. Require a
   TiDB URL and explicit RustFS endpoint, bucket, region and credentials.
   Keep configuration values out of provider summaries. Use a distinct
   per-run metadata key and blob prefix, the existing layout flags, public
   NAPI constructor and normal shutdown. Retain failing constructor-selection
   and availability tests before implementation. These tests use the existing
   capture binding and prove configuration routing, not backend behavior.
2. Establish a closed, owner-scoped freshness prerequisite before constructing
   each arm. Unique names alone do not prove absence: opening TiDB metadata
   currently inserts or accepts an existing namespace row. Inspect exact
   metadata, inode, compact guard and block-authority rows and the normalized
   RustFS prefix. Do not add arbitrary SQL or expand the GET-only Engine
   observer transport. Record the preflight's consistency and race limits.
3. Create four distinct cohorts before execution. Fix the order to legacy,
   compact, compact, legacy. Each arm uses the same selected addon, settings,
   eight fixture identities and original lifecycle workload: 400 iterations,
   concurrency 64, 4 KiB payloads, 64 KiB chunks and the 1,000 IOPS floor.
   Start a separate observer per arm with unchanged per-arm caps.
4. Verify persisted MRC5 and matching block authority before compact timed
   work. Create one untimed persistence canary in each arm. After the original
   handle shuts down, reopen from configuration, verify full bytes and EOF,
   remove the canary, shut down and reopen an empty namespace. Null compact
   inspection only proves recognized non-MRC5; a legacy claim also requires
   a verified fresh cohort and all layout flags explicitly false.
5. Preserve every original runner and observer outcome. A sole original
   `IOPS_TARGET_NOT_MET` may permit the next arm only when its original
   `safe_to_continue_pair` receipt and the additional persistence/cleanup
   checks succeed. Setup, operation, timeout, pending native work, identity,
   capture, proof or cleanup failures stop the sequence. Keep floor
   qualification, comparability and continuation safety separate.
6. Retain a new bounded, redacted comparison artifact and verifier. Namespace
   deletion requires separate verified scope; filesystem shutdown is not
   namespace purge. Owned fixture teardown can be the explicit final cleanup
   boundary. Do not claim cold caches, physical IOPS or causal improvement
   from one pair. Wire an independent hosted gate; preserve the old pilot.

## Validation and delivery

Run Node 24 pure constructor controls, the existing storage harness and
affected observer/receipt regressions. Use actual named tests for any native
seam, strict touched-surface Clippy and formatting through `scripts/cargo-shared`.
Independently review the frozen patch before committing. Preserve the eleven
pre-existing Rust changes and `Cargo.lock`.

The local Docker allocation is below the existing owned-pilot memory floor.
No local paired backend run is authorized by this plan without first meeting
that floor. The installed local addon predates the new RustFS diagnostic
export and cannot provide that evidence. Source/control tests, clean hosted
CI and an actual backend comparison remain separate evidence levels.
