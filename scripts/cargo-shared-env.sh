#!/bin/sh

# Shared Cargo output for all mount-rs worktrees. Source this file from a
# repository script, or use scripts/cargo-shared for one-off commands.
if [ -z "${CARGO_TARGET_DIR:-}" ]; then
  if [ -n "${MOUNT_RS_CARGO_TARGET_DIR:-}" ]; then
    CARGO_TARGET_DIR=$MOUNT_RS_CARGO_TARGET_DIR
  else
    user_home=${HOME:?HOME must be set}
    case "$(uname -s 2>/dev/null || printf unknown)" in
      Darwin)
        cache_root=$user_home/Library/Caches
        ;;
      *)
        cache_root=${XDG_CACHE_HOME:-$user_home/.cache}
        ;;
    esac
    CARGO_TARGET_DIR=$cache_root/mount-rs/cargo-target
  fi
  export CARGO_TARGET_DIR
fi

mkdir -p "$CARGO_TARGET_DIR"
