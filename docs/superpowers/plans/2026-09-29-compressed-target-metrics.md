# Compressed Target Metrics Implementation Plan

> **For agentic workers:** Use subagent-driven development for the regression and
> independent review; the root owns Cargo, Git, fixture execution and receipts.

**Goal:** Retain replayable production-target metric evidence within the existing
bounded capture budget.

**Architecture:** Compress each immutable metric artifact when publishing it.
Keep path/digest references and exact decoded values. Decode only `.gz` artifacts
with explicit size and stream-completion checks.

**Tech stack:** Rust 2024, serde_json, flate2 Rust backend, existing Cargo wrapper.

## Constraints

- Keep all workload geometry, correctness gates and sampled owner limits.
- Preserve immutable publication and failed-run artifacts.
- Encoded metric limit: 16 MiB; decoded metric limit: 64 MiB.
- Ordinary JSON receipts retain their current encoding.
- No simultaneous Cargo or fixture owners.

## Task: Metric artifact codec

**Files:** service `Cargo.toml`, `Cargo.lock`, production-target `mod.rs`,
`metrics.rs`, `process.rs` and `quic_production_target.rs`.

**Interfaces:** Existing `publish_immutable(&Path, &Value) -> Result<(), String>`
publishes encoded frames; `read_json(&Path) -> Result<Value, String>` dispatches
`.gz` inputs through bounded decoding. All metric locators end in `.json.gz`.

- [x] Add a regression through the real publisher/reader requiring lossless
  recovery and at least a fourfold reduction for a representative counter frame.
- [x] Run the release `quic_production_target` regression selector and confirm
  failure is the missing gzip encoding.
- [x] Add flate2 to Unix dev dependencies using the Rust backend. Verify the lock
  change adds only the already-resolved dependency edge.
- [x] Replace the temporary JSON byte vector with streaming publication:

  ```rust
  let mut encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
  serde_json::to_writer(&mut encoder, value).map_err(|_| "metric encoding failed")?;
  let mut file = encoder.finish().map_err(|_| "metric compression finish failed")?;
  file.flush().map_err(|_| "metric flush failed")?;
  let encoded_bytes = file.metadata().map_err(|_| "metric metadata failed")?.len();
  ```

- [x] Use `flate2::bufread::GzDecoder<BufReader<File>>`. Limit encoded metadata
  to 16 MiB, read at most 64 MiB + 1 decoded bytes, reject excess, and require the
  remaining buffered input to be empty after successful stream completion.
- [x] Update worker/controller startup, boundary, closed and terminal metric
  filenames plus every acknowledgment and boundary locator to `.json.gz`.
- [x] Verify malformed/checksum/truncated/trailing/concatenated/oversized inputs
  fail, ordinary JSON still reads, and attempted republication preserves bytes.
- [x] Run affected release tests, strict release Clippy, workspace formatting and
  `git diff --check` through `scripts/cargo-shared` with the dedicated target.
- [ ] Independently review encoding completion, limits, filenames and hashes;
  commit and push to PR #33 only after checks pass.

## Runtime gate

- [ ] Freeze the clean committed Cargo-emitted binary and all compiled inputs.
- [ ] Pin a new bounded owner to that artifact and its exact test inventory.
- [ ] Run strict D10 with 10 servers/10 clients/10 drives/5 partitions/F100 and
  both activity modes × eight patterns. Require final oracle, scope/revocation,
  worker cleanup, strict RustFS export and preserved original fixture identity.
- [ ] Compare encoded capture bytes, observer wall time and whole-process CPU with the failed
  diagnostic. Publish throughput only after complete qualification.
- [ ] Continue D100/F1000, cache-enabled workload and full production geometry.

## Recorded local verification

- Actual regression RED: `.json.gz` frame lacked gzip magic (exit 101).
- Compression and reader controls: 34 passed in release mode.
- Affected targets: QUIC 145 passed / 6 ignored; remote block selection 12 passed.
- The independent review found and the root fixed the ignored native startup
  locator; formatting and strict release Clippy passed afterward.
- Real ten-process SQLite smoke reached both oracles, all 16 stages and clean
  worker cleanup, but failed qualification because the working tree was dirty.
  Preserve that run; repeat from a clean committed revision. TiDB/RustFS rerun
  remains pending.
- Compression CPU is not isolated; retained process CPU includes workload,
  observer and background work.
