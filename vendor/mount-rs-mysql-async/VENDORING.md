# mount-rs mysql_async fork

This private, unpublished package is copied from `mysql_async` 0.37.1. Its
library remains named `mysql_async`; the package name is
`mount-rs-mysql-async`, version `0.37.1`, with `publish = false`.

## Source identity

- Upstream repository: https://github.com/blackbeam/mysql_async
- Registry VCS commit: `ce4b27698c50fb945d8c9ff8c40a2a646be50b12`
- Registry package archive SHA256: `40d11da0e2d9fad4640c9f9198ee431c6d68444568f83ef1f10f3367270071e4`
- Original `src/queryable/query_result/mod.rs` SHA256: `a746f4180e4e17eb394d09aa8a4c7a12757fe3567b78467f617d1dcc0e9eca02`
- Local copy source: the cached `packages.atlassian.com-abebf21e25a19c58/mysql_async-0.37.1` registry source.

`LICENSE-APACHE`, `LICENSE-MIT`, upstream copyright notices, `README.md`,
`README.tpl`, `.cargo_vcs_info.json`, and the reference `Cargo.toml.orig` are
preserved. The VCS JSON and original manifest describe the upstream source,
not a new upstream release. The generated registry `Cargo.toml` is the active
manifest. Registry download markers, the upstream lockfile and CI pipeline
are not required to build this dependency and are not copied.

## Scoped changes

1. The active package manifest changes the package name and publication
   policy and declares a standalone workspace. This prevents Cargo from
   enrolling upstream live-database tests into the parent workspace's CI.
   Dependency versions, features and library name retain upstream semantics.
   The library name means upstream test imports remain valid.
2. `QueryResult<BinaryProtocol>::next_binary_row_with` adds an HRTB callback
   that inspects packet and column borrows synchronously. Its output cannot
   borrow either. The boundary variant retains the current server more-results
   flag before advancing, so an extra empty OK result cannot hide behind
   `is_empty()`. The callback reader and ordinary owned decoding share the
   existing packet reader and result-set boundary handling. Ordinary
   `next`, `collect`, `stream`, Taken cleanup and drop cleanup continue to use
   `Protocol::read_result_set_row`.
3. The callback bridge and focused regressions exercise owned allocation
   controls, result-set lifecycle and public lifetime/Send contracts. Test
   allocation instrumentation is the only new unsafe code; production
   packet and field access introduces no unsafe code.

The allocation regression was first run through a temporary owned adapter.
The root agent observed its expected assertion failure: 32 calls and 1,216
requested bytes in the raw window, with the allocating owned control passing.
That adapter was then removed. The final callback bridge performs no owned
Row decoding.

## Delivery and removal policy

The provider uses an explicit dependency alias with
`package = "mount-rs-mysql-async"` and a relative `path` to this directory.
The parent workspace excludes this fork, which also declares its own standalone
workspace. Do not use a root
`[patch]` of the published `mysql_async` package: such a patch does not travel
with downstream published consumers and would silently create an API mismatch.
This fork's `publish = false` intentionally prevents claiming registry delivery.
A provider calling its added API requires an explicit package delivery solution
before it can be published.

The parent root package remains an implicit workspace member. Listing `.`
explicitly prevented exclusion of this nested dependency in the pinned Cargo
toolchain; metadata confirms that omitting that redundant entry retains all 37
original members and excludes this fork. Its standalone lockfile pins focused
driver tests independently; the parent lockfile pins production consumers.

Keep this fork limited to the borrowed-row hook and its regressions. Do not
refresh upstream dependencies, protocols, connection routines or formatting as
part of the hook. Remove the vendor dependency and this directory when an
upstream released driver provides the required lifetime and lifecycle contract,
after running the same callback, ordinary-path, lifecycle and allocation controls
against that release. If upstream rejects the hook, replace the packaging
arrangement explicitly rather than broadening this fork without review.

## Verification scope

The isolated allocation control warms columns and fixed packet/pool storage
before measurement. It establishes removal of owned Row/Value copies inside
the synchronous callback bridge. It does not measure network framing, TLS,
compression, query initiation, full provider allocations or deployment capacity.
Runtime tests and builds are performed by the root agent using the repository's
`scripts/cargo-shared` wrapper; this authoring agent has not run Rust commands.
