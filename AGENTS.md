# mount-rs agent instructions

## Cargo target cache

Run normal Rust commands through `scripts/cargo-shared` from the repository
root. Its default target directory is isolated by the canonical checkout
root under the per-user cache. It preserves an explicitly supplied
`CARGO_TARGET_DIR`, then `MOUNT_RS_CARGO_TARGET_DIR`. Keep explicit overrides
separate across worktrees too, so another branch cannot supply a stale binary.

```sh
./scripts/cargo-shared test --workspace --all-targets --locked
./scripts/cargo-shared clippy --workspace --all-targets --locked -- -D warnings
```

Do not create or commit a worktree-local `target/` directory. If a command
needs a built binary, use the active `$CARGO_TARGET_DIR` rather than assuming
`$PWD/target`. Existing scripts that deliberately set a dedicated target for
an isolated or bounded gate should keep doing so.
