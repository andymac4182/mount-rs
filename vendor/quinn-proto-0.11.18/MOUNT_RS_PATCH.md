# Scoped Quinn first-packet CLOSE patch

This directory preserves all 58 files in the `quinn-proto 0.11.18` registry
archive. The original archive SHA-256 is
`a9746dbde176634f4f2f1faf2404e30a31b2bc1e9cafb5329c95d8177a18c9fc`,
matching the repository lockfile before the source override. Upstream commit:
`eaec0db4bcb698f76df736d89938743c88a2ad5f`.
`MOUNT_RS_UPSTREAM.json` records every original file hash. The normalized
`Cargo.toml`, package version, features, licenses, and upstream test sources are
unchanged. `Cargo.toml.orig` and `Cargo.lock` are upstream provenance, not the
workspace build manifest or resolved workspace lock.

## Local change

Only `src/connection/mod.rs` differs from the original package. Immediately after
successful first-Initial packet processing, it performs the existing closed-state
bookkeeping (`close_common`, then the existing close timer unless already drained)
before processing a coalesced remainder. Normal packets already do this. A first
transport CLOSE can enter Draining without a TLS handshake; previously that path
left a live endpoint record governed only by the idle timeout. The patch retains
the existing error propagation, Initial AEAD checks and natural 3-PTO grace.
It does not authenticate the connection or admit application data.

The repository-owned regression is
`crates/mount-rs-blob-cache/tests/quinn_close.rs`: deterministic first-CLOSE and
ordinary-close controls plus an ignored, owned real-UDP endpoint drain test.
The unpatched dependency failed both first-CLOSE regressions after their
packet/error/state assertions, while ordinary closure drained normally.

## Delivery and removal

The repository root overrides crates.io through `[patch.crates-io]`; this
package is excluded from workspace membership. Workspace-built CLI and server
artifacts use this source through existing Quinn dependencies. Cargo patches do
not propagate from published libraries to another consumer's root manifest.
Such consumers must supply the same root override until a fixed upstream
release is selected. Remove this directory and the root override only after
that release passes the deterministic and real-UDP regressions, peer cache
restart/security tests, and remote transport tests.

Primary sources:
- https://github.com/quinn-rs/quinn/releases/tag/quinn-proto-0.11.18
- https://doc.rust-lang.org/cargo/reference/overriding-dependencies.html
