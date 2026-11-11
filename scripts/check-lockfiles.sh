#!/bin/sh
# Fail fast when a manifest changed without its lock file. The test crates
# under tests/ have lock files of their own that `cargo --locked` rejects
# when a workspace crate gains a dependency.
set -eu
status=0
for lock in $(git ls-files 'Cargo.lock' '*/Cargo.lock'); do
  manifest="$(dirname "$lock")/Cargo.toml"
  if ! cargo metadata --locked --format-version 1 --manifest-path "$manifest" >/dev/null; then
    echo "STALE LOCK FILE: $lock (run: cargo metadata --manifest-path $manifest)" >&2
    status=1
  fi
done
exit "$status"
