# Owned Layout Independent Verifier Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Independently check the consistency of the retained owned layout projection, private originals, four actual receipt byte inputs, selected native bytes, and 28 reviewed source seam byte inputs without executing the benchmark or claiming hosted qualification.

**Architecture:** A separate synchronous Buffer API reconstructs the joins and calls the existing pure outcome, phase metrics, interval, compact receipt, and namespace receipt validators. A read-only CLI supplies bounded stable bytes from eight explicit paths. The report distinguishes original floor outcomes, four-arm comparability, continuation safety, uncertainty, and missing terminal or capacity evidence.

**Tech Stack:** Node 24 ESM, `node:test`, built-in Buffer/crypto/fs/module/util/url/path APIs, and existing pure storage validators. No new dependency.

## Global Constraints

- Initial edit lease contains only this plan and `scripts/verify-owned-layout-comparison.test.mjs`. Freeze the semantic RED tests and hold for root review before creating production code.
- After approval, production changes belong only in new `scripts/verify-owned-layout-comparison.mjs`; preserve entry, coordinator, old pilot/verifier, scripts, workflows, protected eleven Rust files, both locks, and installed addon bytes.
- Keep the original 400 iterations, concurrency 64, 4,096-byte payload, 65,536-byte chunks, 1,000 IOPS floor, operation 30s/cleanup 10s deadlines, observer 60s lifetime, request 2s/257 requests/8MiB journal limits, and enclosing prepare245s/runner60s/persistence310s ceilings.
- Run pure controls using Node 24 and `--test-timeout=10000`. Denied-import CLI children have an owned 5s timeout and 1MiB captured output ceiling; signal or timeout is failure.
- Zero native, network, Engine, provider, runner, subprocess, Git, or controller dispatch. Existing pure helpers transitively import `node:http` inertly; import alone is accepted. Never import entry, runner, providers, or a public/native addon loader.
- Linux/macOS are supported CLI platforms. Reject unsupported Windows instead of treating meaningless POSIX permissions as proof; Buffer-only semantic controls remain portable.
- Always emit `hosted_qualified:false`. No current public producer establishes actual owner capacity or final teardown for this new lifecycle.
- Private record consistency is not authenticated live evidence, cold-cache or physical-device IOPS, exclusive CPU attribution, a causal compact improvement, or independent clean-checkout/build observation.
- Read raw receipt bytes and join parsed values separately. Never replace actual byte hashes with a hash of JSON reserialization.
- No raw config, endpoints, credentials, receipt paths, arbitrary errors, cohorts, container/device/interface names, or original sample paths in the returned report.
- Do not commit or wire CI as part of this lease. Root reviews and owns those actions.

---

## Current source basis

| Source | Existing contract to reuse |
| --- | --- |
| `benchmarks/storage/owned-layout-entry.mjs:176` | Private receipts retain `{value,sha256}` from actual file bytes. |
| `benchmarks/storage/owned-layout-entry.mjs:214` | Scope hash uses reconstructed field order `id,owner,metadataPrefix,blockPrefix`. |
| `benchmarks/storage/owned-layout-entry.mjs:420` | Single-use comparison evidence is joined to original outcomes and native metrics before sidecar retention. |
| `benchmarks/storage/owned-layout-entry.mjs:426` | `mount-rs.owned-layout-originals.v1` joins fixtures/controller/engine/build/native/scope and exact captured original benchmarks with cohorts. |
| `benchmarks/storage/owned-layout-entry.mjs:442` | Output-cap fallback preserves supported original outcomes and uncertainty in a comparison-incomplete envelope. It may coexist with complete retained private evidence. |
| `benchmarks/storage/owned-layout-comparison.mjs:121` | Fixed arm/persistence projection and explicit legacy/compact flags. |
| `benchmarks/storage/owned-layout-comparison.mjs:153` | Canary, original shutdown, two fresh configuration reopens, exact counts, and layout-generation continuity. |
| `benchmarks/storage/owned-layout-comparison.mjs:219` | Observer cap/cost accounting, eight identities, monotone paired counters, and original interval recomputation. |
| `benchmarks/storage/owned-layout-comparison.mjs:348` | Complete/comparable/safe and floor qualification are separate. Whole-call uncertainty is sticky. |
| `benchmarks/storage/owned-layout-outcome.mjs:238` | `assessOwnedLayoutRunnerOutcome(benchmark)` checks actual 400 samples, original full failure set, RustFS raw phase, config, cleanup, and terminal safety. |
| `benchmarks/storage/owned-layout-metrics.mjs:105` | `projectOwnedLayoutPhaseMetrics(originalPhase)` picks fixed measured counters, preserves unavailable optional sections, and does not qualify the runner. |
| `benchmarks/storage/backing-observer.mjs` | `projectInspect` and `summarizeInterval` validate resource projections and compute null totals plus separate partial totals. |
| `scripts/verify-owned-backing-pilot.mjs:180` | Precedent for bounded NOFOLLOW file reads and an inert direct CLI guard; the old pilot verifier remains unchanged. |

Inspect/stats response bodies are not retained. Their digest fields can only be checked for syntax and joins; this verifier must explicitly state that it did not independently rehash those bodies.

## Fixed API and inputs

```text
SOURCE_PATHS: frozen array of the exact28 strings below
verifyOwnedLayoutComparison(inputs): synchronous frozen consistency report
main(argv = process.argv.slice(2)): Promise<0 | 1>
```

`inputs` has exactly eight own data fields:

```js
{
  projection: Buffer, originals: Buffer,
  fixtures: Buffer, controller: Buffer, engine: Buffer, build: Buffer,
  native: Buffer,
  sources: { [exactSourcePath]: Buffer }
}
```

Use `util.types.isProxy` before inspecting untrusted objects or Buffers. Reject accessors, proxies, inherited/custom prototypes, symbols, missing/extra fields, sparse arrays, and non-Buffers. Copy Buffer bytes with captured built-in operations rather than untrusted methods, iterators, or `toJSON`. No injected validator, projector, factory, environment, or filesystem capability is accepted.

| Input | Nonempty actual byte ceiling |
| --- | --- |
| projection, originals | 33,554,432 each, independently |
| fixtures, controller, engine | 16,384 each |
| build | 65,536 |
| native | 134,217,728 |
| each source seam | 2,097,152 |

Decode all six JSON inputs as fatal UTF-8, reject duplicate decoded object keys (including escaped equivalents), then parse bounded dense JSON data. Object and array shapes are closed where the committed schema is closed. Original benchmark records retain the source snapshot fields; do not reject their private environment or original error text merely because those fields are excluded from the report. Bound parse depth/nodes/strings before replaying existing validators.

The exact source list is:

```text
Cargo.toml
Cargo.lock
bindings/mount-rs-napi/Cargo.toml
bindings/mount-rs-napi/index.js
bindings/mount-rs-napi/src/lib.rs
bindings/mount-rs-napi/src/namespace_presence.rs
src/diagnostics/storage.rs
src/diagnostics/profile.rs
scripts/test-tidb.sh
scripts/test-rustfs.sh
scripts/rustfs-combo-runner.py
scripts/rustfs-bounded-docker.py
.github/workflows/remote-drives.yml
benchmarks/storage/errors.mjs
benchmarks/storage/stats.mjs
benchmarks/storage/runner.mjs
benchmarks/storage/providers.mjs
benchmarks/storage/diagnostics.mjs
benchmarks/storage/backing-engine-transport.mjs
benchmarks/storage/backing-observer.mjs
benchmarks/storage/compact-layout.mjs
benchmarks/storage/namespace-presence.mjs
benchmarks/storage/owned-backing-pilot.mjs
benchmarks/storage/owned-layout-arm.mjs
benchmarks/storage/owned-layout-outcome.mjs
benchmarks/storage/owned-layout-metrics.mjs
benchmarks/storage/owned-layout-comparison.mjs
benchmarks/storage/owned-layout-entry.mjs
```

This list is a reviewed runtime/controller seam inventory. It is not a complete native build dependency closure. Do not add verifier/test/docs bytes or arbitrary checkout files to it.

## Fixed report

Return this exact frozen top-level shape; recursively freeze every retained child:

```js
{
  schema: "mount-rs.owned-layout-independent-check.v1",
  status: "consistent" | "incomplete" | "rejected",
  runtime_scope: "production_entry_attempt" | "modeled_controls" | "unavailable",
  hosted_qualified: false,
  publication_complete: Boolean,
  originals_retained: Boolean,
  checked_arm_count: 0 | 1 | 2 | 3 | 4,
  floor_qualified: Boolean,
  comparable: Boolean,
  safe_to_continue: Boolean,
  native_uncertainty: Boolean | null,
  arms: [{ role, layout, status, outcome, native_metrics, container_metrics }],
  joins: {
    projection_sha256, originals_sha256, fixtures_sha256, controller_sha256,
    engine_sha256, build_sha256, native_sha256, source_count, seam_hashes_match
  },
  failure_codes: [],
  evidence_scope: {
    verification: "offline_retained_record_consistency; no_runtime_or_endpoint_dispatch",
    source: "exact_28_reviewed_runtime_controller_seam_hashes; not_full_build_dependency_closure",
    build: "declared_seal_flags_and_bytes_joined; clean_checkout_and_build_not_independently_observed",
    container: "recomputed_selected_counter_intervals; raw_inspect_stats_bodies_not_retained_or_rehashed",
    capacity: "unavailable_missing_actual_owner_capacity_producer",
    owner_terminal: "unavailable_missing_actual_owner_terminal_producer",
    interpretation: "descriptive_ABBA_accounting; no_hosted_qualification_or_causal_physical_IOPS_proof"
  }
}
```

`outcome` is the exact closed existing assessor receipt. `native_metrics` is the exact closed existing phase projector receipt from the captured original workload phase, or null when no benchmark was captured. `container_metrics` contains exactly `cpu_usage_ns,block_bytes,block_operations,network_rx_bytes,network_tx_bytes`; each contains `{complete,total,partial_total,missing_member_count}` from recomputation. Missing arm evidence remains null; never fabricate persistence, zero counters, or future arms. Digest fields are null if bytes were not accepted; `source_count` is bounded0..28. Supported status values are retained, unknown values map to fixed unavailable.

Rejection produces only fixed codes and no raw error text. Validate in this order and stop at the first failed stage: input shape; Buffer/caps; JSON/schema; byte joins; source/native joins; handoff/scope; ABBA/cohorts; original outcome equality; observer/local and global fixture accounting; persistence; native phase equality; continuation/publication. This order prevents inconsistent inputs from yielding a mixture of asserted proof and later failures.

Finite code suffixes, all prefixed `OWNED_LAYOUT_CHECK_`:

```text
INPUT_SHAPE_INVALID BUFFER_REQUIRED INPUT_CAP_EXCEEDED JSON_INVALID SCHEMA_INVALID
BYTE_JOIN_INVALID SOURCE_JOIN_INVALID NATIVE_JOIN_INVALID HANDOFF_JOIN_INVALID
COHORT_INVALID ORIGINAL_OUTCOME_MISMATCH OBSERVER_INVALID FIXTURE_IDENTITY_INVALID
PERSISTENCE_INVALID NATIVE_METRICS_MISMATCH CONTINUATION_INVALID PUBLICATION_INVALID
ARGUMENTS_INVALID FILE_INVALID RECORD_INCOMPLETE
```

An internally consistent stopped or cap-reduced record emits `status:"incomplete"` and `RECORD_INCOMPLETE`; it preserves original floor/status/uncertainty fields and keeps comparability/safety false. A complete four-arm record with only original `IOPS_TARGET_NOT_MET` failures emits `status:"consistent"`, failed original statuses, floor false, comparability/safety true. Neither case is hosted qualification. A malformed record emits rejected and safety/comparability false; retain only independently supported original fields already validated before the failing stage.

## Task 1: Actual byte joins and pure API boundary

**Files:**
- Create after approval: `scripts/verify-owned-layout-comparison.mjs`
- Test: `scripts/verify-owned-layout-comparison.test.mjs`

**Interfaces:**
- Consumes the exact Buffer record, caps, schemas, report, fixed codes, and SOURCE_PATHS above.
- Produces `SOURCE_PATHS` and `verifyOwnedLayoutComparison(inputs)` with no side effects.

- [ ] **Step 1: Observe the retained first-tranche RED controls.** The fallback returns `{}`; required status and fixed-code assertions fail, while the existing producer-fixture sanity test passes.

```js
const model = fixture(), inputs = model.inputs()
assert.equal(api.verifyOwnedLayoutComparison(inputs).status, "consistent")
inputs.fixtures = Buffer.from(JSON.stringify(JSON.parse(inputs.fixtures)))
assert.deepEqual(api.verifyOwnedLayoutComparison(inputs).failure_codes,
  ["OWNED_LAYOUT_CHECK_BYTE_JOIN_INVALID"])
```

Run: `fnm exec --using v24.18.0 node --test --test-timeout=10000 scripts/verify-owned-layout-comparison.test.mjs`

- [ ] **Step 2: Implement the approved input boundary and closed report in the new production path.** Capture descriptor data only after rejecting proxies. Decode JSON fatally, scan decoded object keys for duplicates, and bound parsed depth/nodes/dense arrays. Compute SHA-256 from copied actual bytes. Compare each sidecar join's digest and parsed value to its corresponding actual receipt; compare public handoff/build/identity/originals receipt fields independently. Validate the exact build seal flags and native bytes plus all28 source hashes without calling Git or claiming those flags prove a clean build.

The actual byte hash primitive is:

```js
const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex")
const scopeBytes = (scope) => Buffer.from(JSON.stringify({
  id: scope.id, owner: scope.owner,
  metadataPrefix: scope.metadataPrefix, blockPrefix: scope.blockPrefix
}))
```

- [ ] **Step 3: Join the fixed controller handoff as retained data.** Require generation1, exact eight-role unique CID allowlist and owner labels, literal Engine socket, source-derived endpoint syntax, owners, and exact scoped prefixes. Use the entry's exact owner predicate `^[A-Za-z0-9_][A-Za-z0-9_.-]{0,119}$`, including leadingunderscore and120-character ceiling, and nonempty NUL-free text bounded4096bytes for TiDB URL, RustFS endpoint and bucket. Require selected native path syntax to be lexical normalized absolute POSIX with `.node` suffix and the same NUL/text cap; this is deterministic across Buffer API review hosts and does not claim realpath, file existence, checkout membership, or actual native load. Reconstruct the scope hash using `scopeBytes`. Compare all public/private handoff/ownership joins. Do not invoke entry's environment validator, fill credentials, probe endpoints, or represent syntactic endpoint equality as actual routing to those CIDs.

- [ ] **Step 4: Run the bounded pure suite.** The byte/input controls must be GREEN; replay controls remain RED until Task2. Retain full TAP logs and the production/test hashes rather than treating partial GREEN as completion.

## Task 2: Reconstruct original ABBA, persistence and resource evidence

**Files:**
- Modify after approval: `scripts/verify-owned-layout-comparison.mjs`
- Test: `scripts/verify-owned-layout-comparison.test.mjs`

**Interfaces:**
- Consumes parsed originals and fixed receipt joins from Task1.
- Imports only the existing pure `assessOwnedLayoutRunnerOutcome`, `projectOwnedLayoutPhaseMetrics`, `summarizeInterval`, `projectInspect`, `validateCompactLayoutReceipt`, and `validateSplitNamespacePresenceReceipt` seams.
- Produces independently recomputed closed arm receipts and honest complete/incomplete consistency flags.

- [ ] **Step 1: Observe semantic controls for original replay, below-floor continuation and partial metrics.** Model data is not runtime performance proof.

```js
const model = fixture({ floor: false }), report = api.verifyOwnedLayoutComparison(model.inputs())
assert.equal(report.status, "consistent")
assert.equal(report.floor_qualified, false)
assert.equal(report.comparable, true)
assert.equal(report.arms[0].outcome.status, "failed")
partialPhysical(model)
assert.deepEqual(api.verifyOwnedLayoutComparison(model.inputs()).arms[0].container_metrics.block_operations,
  { complete: false, total: null, partial_total: "0", missing_member_count: 8 })
```

- [ ] **Step 2: Reconstruct fixed A1legacy/B1compact/B2compact/A2legacy role order.** Require fresh role-derived cohort id/key/prefix/owner from the controller scope for every captured original. Complete evidence has four arms; stopped evidence has a captured prefix and at most its next failed public arm with null original. Match each original config/layout and exact reassessed outcome to public outcome. Preserve the original full failure set through the existing assessor; never use the old first-failure projection.

- [ ] **Step 3: Validate observer evidence independently of continuation booleans.** For any retained public backing projection or claimed completed arm, require original schema, complete journal/evidence, exact cap/cost and four journal entries, 33requests/33headers/16firstframes/16retired iterators, no pending stages/dropped entries/issues, valid digest syntax/windows, and two phase boundaries with all8 members. Reconstruct the fixed public backing projection from original selected fields and compare it to the retained projection. Call `summarizeInterval` on the original and projected boundaries; compare containers/metrics/endpoints and completeness. Check known paired counters monotonically and stable key sets even if another member is null. Only `metric_unavailable` may reduce physical metric coverage. Global fixed CID/role/owner-label/image/start/restart/resource-limit identities must remain identical across all available boundaries/arms; do not accept merely locally equal pairs. Limits describe configuration, not observed host capacity. A captured failed original with honest null public backing remains incomplete with its reassessed outcome; leave public container metrics null rather than reconstructing a missing proof.

- [ ] **Step 4: Validate every available persistence receipt and join compact continuity to the original timed receipt.** Use the namespace and compact validators; require source-fixed TiDB/RustFS providers and boolean durability values consistent across all retained arm receipts, explicit all3 layout flags, 4KiB canary full-byte/EOF/remove/empty checks, original confirmed shutdown, two configuration-only fresh reopens and confirmed reopen shutdowns, exact18 counters, empty cleanup codes, null failure, and phaseverified for a completed arm. Require one compact backingId; generation monotone before→timed→drained, exact drained→reopened, monotone reopened→removed, exact removed→empty. Legacy recognized-nonMRC5 never becomes exact legacy marker proof. Preserve honest partial or null arm state on stopped records without treating missing shutdown/reopen proof as complete; enforce full proofs only when completion is claimed. The retained controller does not contain durability configuration, so do not fabricate an environment to infer it.

- [ ] **Step 5: Recompute exactly one RustFS workload native phase and compare the closed receipt.** Required phase metrics status is observed for complete comparability. Keep local/process/core/forwarding optional unavailable rather than zero, fixed SQL/blob/local rows and inclusive semantics, and no physical/wire/allocator attribution. No additional native or observer calls.

- [ ] **Step 6: Reconstruct final status predicates.** Check fixed whole-call ceilings (caller may only lower), completion count/prefix/stop-code consistency, sticky uncertainty, pending count, each arm's native identity join, fixture stability and persistence gates. Independently latch required uncertainty as soon as an unsafe reassessed outcome has nonquiescence, unsettled owned work, pending operation count notzero, timeoutcountpositive, or original safe-pair flag nottrue. Validated persistence availablefalse or positive original create with unconfirmed shutdown, or reopen creates exceeding confirmed reopen shutdowns, also latchestrue. Known source `*_TIMEOUT` and positive enclosing pending count implytrue. Reject a suppliedfalse latch, retaining derivedtrue and already validated original outcomes even in a rejected report; later checks never erase it. Plain null persistence alone is the uncreated-arm default and does not invent uncertainty. Then validate public publication/originals/qualification flags. A stopped prefix with missed deadline or uncertainty cannot continue even if original samples are clean; no retry or future arm is inferred. A public cap envelope remains incomplete although privately captured originals can be complete.

- [ ] **Step 7: Run all Buffer semantics under deny guards.** Require exact codes for the actual-byte, ABBA, original sample, native counters, persistence, global identity, masked-reset and continuation falsifications. Require the full private model and below-floor/partial positives to pass; assert deep freezing and absence of secret paths/config in reports.

## Task 3: Bounded stable read-only CLI and independent acceptance

**Files:**
- Modify after approval: `scripts/verify-owned-layout-comparison.mjs`
- Test: `scripts/verify-owned-layout-comparison.test.mjs`

**Interfaces:**
- `main(argv)` accepts exactly the eight distinct flag/value pairs `--projection --originals --fixtures --controller --engine --build --native --checkout`.
- Exit0 means offline record consistency, including a complete safe below-floor record. Exit1 means rejected/incomplete. Both print only the closed report to stdout. Importing the module prints nothing and reads no files.

- [ ] **Step 1: Exercise the existing denied-import child controls once production exists.** Children forbid native loaders, entry/runner/providers/addon imports, actual network methods and subprocess dispatch. Use Node24's observed `import.meta.main` direct-entry indicator: a direct executable symlink must emit the closed consistency report, while a normal import remains inert. An exit zero without a report is a failed CLI control. Before production exists, these controls are explicitly skipped and cannot be called executed inert CLI proof.

- [ ] **Step 2: Read all seven explicit file paths and exactly28 source files under checkout.** On Linux/macOS pre-lstat each leaf and reject nonregular files. Open supported readonly `O_NONBLOCK|O_NOFOLLOW` handles to close the FIFO replacement/open race; check the opened handle's regular type and captured identity. Use nonempty fixed caps, bounded reads with an extra-byte overflow check, and bigint dev/ino/size/mtimeNs/ctimeNs stamps before/after reads and at the final path check. Close every owned handle in a finally block on success or failure. Six JSON files require regular0600, nlink1 and currentUID ownership, plus a currentUID-owned private0700 direct parent. Resolve canonical paths and reject any same canonical/devino identity among explicit inputs, including addon aliases; do not load addon bytes. Pin the canonical checkout root and its bigint identity, and require each literal source path's canonical containment under that root. Reject every source symlink component and escape; leaf NOFOLLOW alone does not protect ancestor components. After the complete read set, recheck all35 files (7 explicit inputs plus28 sources) against captured bigint dev/ino/size/mtimeNs/ctimeNs and recheck pinned checkout root and source containment before calling the Buffer API. Reject unsupported platforms explicitly. These are observed read-boundary consistency checks, with no uniform filesystem wall deadline claim.

- [ ] **Step 3: Run CLI falsifications for missing/duplicate args, nonprivate modes, symlink/hardlink/canonical ancestor aliases, native input aliases, and a source changed after its own completed read.** The source mutation uses the existing bounded child guard's owned filesystem interleaving, with no production injection seam. Use owned0700 temporary directories and inert model addon/source bytes; cleanup in test hooks. No controller or historical private tooling is executed.

- [ ] **Step 4: Freeze source and full focused/joined logs, then request independent review.** Use unchanged Node24/10s controls; no formal/backend suite, Cargo, Engine or live fixture is part of this acceptance. Root owns eventual CI integration after GREEN and review.

## Initial tranche review boundary

Freeze this plan and new tests with source hashes, Node version, exact full RED exit/counts/skips, fixture-sanity result, syntax result and protected-byte check. Send the receipt to root and the assigned read-only reviewer. Hold production creation until root authorizes GREEN implementation. No optional edge expansion is required before that review.

## Missing producers and remaining goal gates

The current new controller handoff is a retained entry input contract; public fixture scripts do not yet produce its actual endpoint/scope seal for this four-arm lifecycle. Actual owner capacity/floors and terminal settlement/resource removal receipts are also absent. A later authorized public producer must bind actual entry/originals/build/controller/CID/capacity byte digests, preserve nonzero errors and sticky uncertainty, and observe exact owned resources absent/data destroyed. Such a receipt may identify cleanup as `owned_backing_resources_destroyed` with `namespace_purge:not_performed`; shutdown or file removal is not logical SQL/blob namespace purge. A logical purge claim needs separate observed scoped deletion/absence and pool shutdown evidence.

This verifier accepts none of those missing claims as qualification inputs. Even consistent production-entry records retain hostedfalse until separately approved actual producers and a terminal verifier join exist. Build seal flags/source hashes and syntactic controller endpoints remain retained boundary observations, not independent build cleanliness or whole-interval routing proof.
