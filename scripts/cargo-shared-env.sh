#!/bin/sh

# Cargo output isolated by canonical checkout path under the per-user cache.
# Source from a repository script declaring repo_dir, REPO_ROOT or script_dir,
# or from inside the checkout; use scripts/cargo-shared for one-off commands.
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
    mount_rs_checkout_hint=${repo_dir:-${REPO_ROOT:-${script_dir:+$script_dir/..}}}
    mount_rs_checkout_root=$(git -C "${mount_rs_checkout_hint:-.}" rev-parse --show-toplevel) || return 1
    mount_rs_checkout_root=$(CDPATH='' cd -- "$mount_rs_checkout_root" && pwd -P) || return 1
    mount_rs_checkout_key=$(printf '%s\n' "$mount_rs_checkout_root" | git -C "$mount_rs_checkout_root" hash-object --stdin) || return 1
    CARGO_TARGET_DIR=$cache_root/mount-rs/cargo-target/$mount_rs_checkout_key
  fi
  export CARGO_TARGET_DIR
fi

mkdir -p "$CARGO_TARGET_DIR"
