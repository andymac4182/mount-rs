# Isolated integration lockfile repair — 2026-09-30

## Cause and change

The TiDB provider now depends on the local `mount-rs-mysql-async 0.37.1` package.
The isolated RustFS, provider-matrix and AWS workspaces still locked the registry
`mysql_async 0.37.1` package. Cargo rejected `--locked` before compilation.
Captured PR #34 logs confirm five RustFS and three Node failures at this boundary;
the AWS graph had the same latent mismatch.

Actual local locked builds reproduced the rejection in all three workspaces.
Cargo then regenerated each graph with:

```sh
./scripts/cargo-shared update --offline --workspace --manifest-path tests/rustfs/Cargo.toml
./scripts/cargo-shared update --offline --workspace --manifest-path tests/provider_matrix/Cargo.toml
./scripts/cargo-shared update --offline --workspace --manifest-path tests/aws/Cargo.toml
```

Each graph retains 381 packages and all 379 unrelated package records exactly.
The fork retains the old driver's 20 dependencies. RustFS retains `rand 0.10.2`;
matrix/AWS retain `0.10.3`. Only the driver package identity, its registry
source/checksum, and TiDB's incoming dependency edge change. Root registry driver
dev dependencies and the Rust alias `mysql_async` remain valid. No root lockfile
was copied and no version or `--locked` requirement was relaxed.

## Local qualification

All three complete `cargo metadata --locked --offline --format-version 1` runs
pass. Their resolved graphs select the exact local fork manifest with
`minimal-rust`, 20 resolved dependencies, and TiDB's `mysql_async` alias. No
registry `mysql_async` package remains in these three isolated graphs.

| Workspace | Locked build qualification | Strict Clippy |
| --- | --- | --- |
| Provider matrix | `build` passes | All targets pass |
| RustFS | `test --no-run` passes | All targets pass |
| AWS | `test --no-run` passes | All targets pass |

Commands use `scripts/cargo-shared`, Rust 1.95.0, an explicit temporary Cargo
target and `--locked --offline`. Strict Clippy uses `--all-targets -- -D warnings`.
Incremental compiler caching is disabled for these build/lint checks. Every
child retains the 300-second deadline, 64 MiB capture cap and 64 GiB host reserve;
all exit successfully, are reaped, and leave no process group.

| Witness | SHA-256 |
| --- | --- |
| Matrix failing-build receipt | `a5454db705216b8fa3be90abcef635ca031a5fc5defa1bf3d8b74339c2591377` |
| RustFS failing-build receipt | `af7ac38ee2b6f330a1b6a360622f0244ca89fa0068ec08de5248b8c2a200f5ab` |
| AWS failing-build receipt | `c237e3a0ba72bf19cab02daae698fdbf1a4f5bb8ec34e59f1c95f719386954ee` |
| Matrix passing-build receipt | `8f6a37f41c905bdf2c2c6e030879164c63b5b392b9565567e0a9629125512ef1` |
| RustFS passing test-build receipt | `f7cc77236529d141204cd9d2545ddd7a1f50774cf052ceec8af36e1f47b9e650` |
| AWS passing test-build receipt | `a0007b0a91c2f46aaaddc96b240efacec9e46d8373e9ae125180c1e5936d5d63` |

This qualifies graph resolution, compilation and linting. It does not execute
live RustFS/AWS tests, prove hosted CI success, improve throughput, or qualify
merging the broader work. Separate legacy performance floors and full formal
and production-scale verification remain outstanding.
