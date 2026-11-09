#!/bin/sh
set -eu

# Actual TiDB + RustFS composition, with process reopen but no replicated
# restart or multi-server mount acceptance. The parent harnesses own cleanup.
repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_dir"
case "${1:-}" in
  '')
    if [ "$#" -ne 0 ]; then
      echo "test-tidb-rustfs-smoke.sh accepts no public arguments" >&2
      exit 2
    fi
    case "${MOUNT_RS_TIDB_TOPOLOGY:-single}" in
      single) ;;
      *) echo "This smoke gate requires topology=single; use test-tidb.sh for replicated restart acceptance" >&2; exit 2 ;;
    esac
    export MOUNT_RS_TIDB_TOPOLOGY=single
    export MOUNT_RS_TIDB_CHUNKED_RUSTFS=1
    export MOUNT_RS_TIDB_CHUNKED_RUSTFS_REOPEN=1
    export RUSTFS_COMBO_ONLY=1
    export RUSTFS_COMBO_NAME=tidb-rustfs-smoke
    export RUSTFS_COMBO_COMMAND='sh scripts/test-tidb-rustfs-smoke.sh --inside-rustfs'
    exec sh scripts/test-rustfs.sh
    ;;
  --inside-rustfs)
    : "${RUSTFS_RUN_DIR:?must be called by the RustFS harness}"
    : "${RUSTFS_COMBO_PREFIX:?must be called by the RustFS harness}"
    export MOUNT_RS_TIDB_COMPOSITION_COMMAND='sh scripts/test-tidb-rustfs-smoke.sh --inside-tidb'
    exec sh scripts/test-tidb.sh
    ;;
  --inside-tidb)
    : "${RUSTFS_RUN_DIR:?must be called by the RustFS harness}"
    : "${MOUNT_RS_TIDB_URL:?must be called by the TiDB harness}"
    # The contract test cleans its scoped data after process reopen. It cannot
    # be reused as a persisted restart phase without a different fixture policy.
    if [ "${MOUNT_RS_TIDB_EXPECT_PERSISTED:-}" != 0 ]; then
      echo "This composition only supports an initial single-node smoke run" >&2
      exit 2
    fi
    ./scripts/cargo-shared test -p mount-rs-tidb --test chunked_rustfs --locked -- --ignored --nocapture
    ./scripts/cargo-shared test -p mount-rs-tidb --test lost_block_ack_rustfs --locked -- --ignored --nocapture
    ./scripts/cargo-shared test -p mount-rs-cli --test http_remote --locked -- tidb_cli_http_cached_reopen --exact --ignored --nocapture
    ./scripts/cargo-shared test -p mount-rs-cli --test http_subprocess --locked -- tidb_remote_sessions_reopen_rustfs_blocks_after_server_shutdown --exact --ignored --nocapture
    ./scripts/cargo-shared test -p mount-rs-cli --test http_subprocess --locked -- tidb_remote_sessions_recover_rustfs_after_process_kill_and_lease_expiry --exact --ignored --nocapture
    ;;
  *) echo "Unknown smoke gate phase" >&2; exit 2 ;;
esac
