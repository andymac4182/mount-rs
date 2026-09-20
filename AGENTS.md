# mount-rs agent instructions

## Shared Cargo target

Run normal Rust commands through `scripts/cargo-shared` from the repository
root. It sets one per-user Cargo target directory shared by all mount-rs
worktrees and preserves an explicitly supplied `CARGO_TARGET_DIR`.

```sh
./scripts/cargo-shared test --workspace --all-targets --locked
./scripts/cargo-shared clippy --workspace --all-targets --locked -- -D warnings
```

Do not create or commit a worktree-local `target/` directory. If a command
needs a built binary, use the active `$CARGO_TARGET_DIR` rather than assuming
`$PWD/target`. Existing scripts that deliberately set a dedicated target for
an isolated or bounded gate should keep doing so.
