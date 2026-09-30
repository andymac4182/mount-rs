# SQLite WAL through the native JavaScript boundary

The normal Node package suite passed on macOS ARM64 using Node 24.18.0 and a
freshly built native addon from `00da6010`, with the three owned test changes
listed in [report.json](report.json). The final suite used the checked-in loader
and declarations, a locked Rust graph and the pinned mountx oracle.

The new native test checks six configurations: WAL and preserve on separate or
shared files, plus WAL selected independently for either storage role. It
observes both persisted SQLite header bytes and `PRAGMA journal_mode` through an
independent read-only Node SQLite connection. Four separate-file cells use MRC5;
shared-file cells use the ordinary chunked filesystem.

Twelve omitted/preserve reopens recover the complete 12,461-byte payload, stat
size and exact EOF. MRC5 backing identity, structural generation and verified
block authority remain unchanged. Forty invalid configuration cases require
`EINVAL` before either provider creates its backing file. TypeScript checks both
accepted values in both roles and rejects invalid string and boolean values.

The same WAL assertion first failed against the prior native addon: the
JavaScript WAL option was ignored and the main header remained `[1, 1]`. The new
addon passes with `[2, 2]`. This verifies option propagation through actual N-API
conversion rather than only Rust factory construction.

The full package chain also passed, including the installed offline consumer
and missing-transitive-dependency rejection. A preliminary run failed on the
sandbox's owned Unix-listener restriction; the bounded local-socket run passed.
Build-generated loader ordering/comments were retained separately, then the
checked-in artifacts were restored before the final package run. No production
source or generated binding changes are part of this test slice.

## Reproduce

Use Node 24, install the package and pinned oracle dependencies as in CI, then:

```sh
CARGOFLAGS=--locked pnpm --dir bindings/mount-rs-napi build
node --test --test-concurrency=1 --test-timeout=30000 bindings/mount-rs-napi/test/sqlite-journal-mode.mjs
MOUNTX_SOURCE=/absolute/path/to/pinned/mountx pnpm --dir bindings/mount-rs-napi test
```

Use an isolated `CARGO_TARGET_DIR` for each checkout. The native test is also
included in the normal package test command. It rejects a nonnative binding.

## Qualification limits

This is orderly native option-selection and reopen evidence on macOS ARM64.
It does not prove power-loss durability, other platform execution, external
provider behavior, native host mounting or the full 10,000-client production
capacity target. The active multi-server/cache/fallback goal remains open.
