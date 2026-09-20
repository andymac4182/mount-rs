#!/usr/bin/env bash
# Run a bounded, real native cross-process mount demo.
#
# The demo intentionally uses only the local host driver. It does not read
# provider credentials, contact a cloud service, or treat a probe/log line as
# a mount: readiness requires both the CLI's mounted line and the host mount
# table entry for the exact disposable mountpoint.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
DEMO_ROOT="$REPO_ROOT/examples/demo"
CLI_BIN="$REPO_ROOT/target/debug/mount-rs"
RUST_SOURCE="$DEMO_ROOT/rust-fs-io.rs"
NODE_SOURCE="$DEMO_ROOT/node-fs-io.mjs"

MOUNT_WAIT_SECONDS=60
STOP_WAIT_SECONDS=20
UNMOUNT_WAIT_SECONDS=20
COMMAND_WAIT_SECONDS=15
CLIENT_WAIT_SECONDS=15

PLATFORM="$(uname -s)"
RUN_DIR=""
MOUNTPOINT=""
BACKING=""
CONFIG=""
CLI_STDOUT=""
CLI_STDERR=""
CLI_PID=""
CLIENT_PID=""
RUST_BIN=""
TRANSPORT=""
CLI_STATUS=""

RUST_FILE="rust-process.txt"
JS_FILE="javascript-process.txt"
RUST_PAYLOAD="rust wrote bytes through the native mount-rs view"
JS_PAYLOAD="javascript wrote bytes through the native mount-rs view"

say() {
    printf '%s\n' "$*"
}

blocked() {
    printf 'BLOCKED: %s\n' "$*" >&2
    exit 1
}

command_exists() {
    command -v "$1" >/dev/null 2>&1
}

process_alive() {
    kill -0 "$1" 2>/dev/null
}

wait_for_process_exit() {
    local pid="$1"
    local seconds="$2"
    local deadline=$(( $(date +%s) + seconds ))
    while process_alive "$pid"; do
        if [ "$(date +%s)" -ge "$deadline" ]; then
            return 1
        fi
        sleep 0.1
    done
    return 0
}

run_bounded() {
    local seconds="$1"
    shift
    local command_pid
    "$@" >/dev/null 2>&1 &
    command_pid=$!
    if ! wait_for_process_exit "$command_pid" "$seconds"; then
        kill -TERM "$command_pid" 2>/dev/null || true
        if ! wait_for_process_exit "$command_pid" 1; then
            kill -KILL "$command_pid" 2>/dev/null || true
        fi
        if wait_for_process_exit "$command_pid" 1; then
            set +e
            wait "$command_pid"
            set -e
        fi
        return 124
    fi
    set +e
    wait "$command_pid"
    local status=$?
    set -e
    return "$status"
}

run_client_bounded() {
    local label="$1"
    shift
    local output="$RUN_DIR/$label.log"
    CLIENT_PID=""
    "$@" >"$output" 2>&1 &
    CLIENT_PID=$!
    if ! wait_for_process_exit "$CLIENT_PID" "$CLIENT_WAIT_SECONDS"; then
        kill -TERM "$CLIENT_PID" 2>/dev/null || true
        if ! wait_for_process_exit "$CLIENT_PID" 1; then
            kill -KILL "$CLIENT_PID" 2>/dev/null || true
        fi
        if wait_for_process_exit "$CLIENT_PID" 1; then
            set +e
            wait "$CLIENT_PID"
            set -e
            CLIENT_PID=""
        fi
        say "BLOCKED: $label did not finish within ${CLIENT_WAIT_SECONDS}s" >&2
        sed -n '1,120p' "$output" >&2 || true
        return 1
    fi
    set +e
    wait "$CLIENT_PID"
    local status=$?
    set -e
    CLIENT_PID=""
    sed -n '1,120p' "$output"
    if [ "$status" -ne 0 ]; then
        say "BLOCKED: $label exited with status $status" >&2
        return 1
    fi
    return 0
}

is_mounted() {
    [ -n "$MOUNTPOINT" ] || return 1
    case "$PLATFORM" in
        Linux)
            awk -v target="$MOUNTPOINT" \
                '$2 == target { found = 1 } END { exit(found ? 0 : 1) }' \
                /proc/self/mounts
            ;;
        Darwin)
            mount | awk -v target="$MOUNTPOINT" \
                'index($0, " on " target " (") { found = 1 } END { exit(found ? 0 : 1) }'
            ;;
        *)
            return 1
            ;;
    esac
}

wait_for_native_mount() {
    local deadline=$(( $(date +%s) + MOUNT_WAIT_SECONDS ))
    while :; do
        if grep -Fq "mounted $TRANSPORT" "$CLI_STDOUT" && is_mounted; then
            return 0
        fi
        if ! process_alive "$CLI_PID"; then
            return 1
        fi
        if [ "$(date +%s)" -ge "$deadline" ]; then
            return 1
        fi
        sleep 0.2
    done
}

wait_for_unmount() {
    local deadline=$(( $(date +%s) + UNMOUNT_WAIT_SECONDS ))
    while is_mounted; do
        if [ "$(date +%s)" -ge "$deadline" ]; then
            return 1
        fi
        sleep 0.2
    done
    return 0
}

show_diagnostics() {
    say "mount-rs probe:"
    sed -n '1,120p' "$RUN_DIR/probe.log" >&2 || true
    say "mount-rs stdout:"
    sed -n '1,160p' "$CLI_STDOUT" >&2 || true
    say "mount-rs stderr:"
    sed -n '1,160p' "$CLI_STDERR" >&2 || true
}

stop_cli() {
    [ -n "$CLI_PID" ] || return 0
    if process_alive "$CLI_PID"; then
        kill -INT "$CLI_PID" 2>/dev/null || true
    fi
    if ! wait_for_process_exit "$CLI_PID" "$STOP_WAIT_SECONDS"; then
        return 1
    fi
    set +e
    wait "$CLI_PID"
    CLI_STATUS=$?
    set -e
    CLI_PID=""
    return 0
}

unmount_exact_path() {
    if ! is_mounted; then
        return 0
    fi

    case "$PLATFORM" in
        Darwin)
            if command_exists umount; then
                run_bounded "$COMMAND_WAIT_SECONDS" umount -f "$MOUNTPOINT" || true
                run_bounded "$COMMAND_WAIT_SECONDS" umount "$MOUNTPOINT" || true
            fi
            ;;
        Linux)
            if command_exists fusermount3; then
                run_bounded "$COMMAND_WAIT_SECONDS" fusermount3 -u "$MOUNTPOINT" || true
            fi
            if is_mounted && command_exists fusermount; then
                run_bounded "$COMMAND_WAIT_SECONDS" fusermount -u "$MOUNTPOINT" || true
            fi
            if is_mounted && command_exists umount; then
                run_bounded "$COMMAND_WAIT_SECONDS" umount "$MOUNTPOINT" || true
            fi
            ;;
    esac
}

cleanup() {
    local status=$?
    local cleanup_failed=0
    trap - EXIT INT TERM
    set +e

    if [ -n "$CLI_PID" ]; then
        if process_alive "$CLI_PID"; then
            kill -INT "$CLI_PID" 2>/dev/null || true
            if ! wait_for_process_exit "$CLI_PID" "$STOP_WAIT_SECONDS"; then
                kill -TERM "$CLI_PID" 2>/dev/null || true
                if ! wait_for_process_exit "$CLI_PID" 1; then
                    kill -KILL "$CLI_PID" 2>/dev/null || true
                fi
            fi
        fi
        if wait_for_process_exit "$CLI_PID" 1; then
            wait "$CLI_PID" 2>/dev/null || true
        else
            cleanup_failed=1
        fi
        CLI_PID=""
    fi

    if [ -n "$CLIENT_PID" ]; then
        kill -TERM "$CLIENT_PID" 2>/dev/null || true
        if ! wait_for_process_exit "$CLIENT_PID" 1; then
            kill -KILL "$CLIENT_PID" 2>/dev/null || true
        fi
        if wait_for_process_exit "$CLIENT_PID" 1; then
            wait "$CLIENT_PID" 2>/dev/null || true
        else
            cleanup_failed=1
        fi
        CLIENT_PID=""
    fi

    if [ -n "$MOUNTPOINT" ] && is_mounted; then
        unmount_exact_path
    fi
    if [ -n "$MOUNTPOINT" ] && is_mounted; then
        cleanup_failed=1
        say "BLOCKED: native mount remains at $MOUNTPOINT; preserving $RUN_DIR for manual cleanup" >&2
    fi

    if [ "$cleanup_failed" -eq 0 ] && [ -n "$RUN_DIR" ] && [ -d "$RUN_DIR" ]; then
        case "$RUN_DIR" in
            "${TMP_PARENT:-/tmp}"/mount-rs-demo.*)
                rm -rf "$RUN_DIR"
                ;;
            *)
                cleanup_failed=1
                say "BLOCKED: refusing to remove unexpected demo directory $RUN_DIR" >&2
                ;;
        esac
    fi

    if [ "$cleanup_failed" -ne 0 ] && [ "$status" -eq 0 ]; then
        status=1
    fi
    exit "$status"
}

for required_command in cargo node rustc mktemp awk grep sed sleep date uname mount; do
    command_exists "$required_command" || blocked "required command '$required_command' is not available"
done

case "$PLATFORM" in
    Darwin|Linux) ;;
    *) blocked "native demo supports macOS and Linux only (detected $PLATFORM)" ;;
esac

TMP_PARENT="$(cd "${TMPDIR:-/tmp}" && pwd -P)" || \
    blocked "temporary directory ${TMPDIR:-/tmp} is not accessible"
RUN_DIR="$(mktemp -d "$TMP_PARENT/mount-rs-demo.XXXXXX")" || \
    blocked "could not create a private temporary demo directory under $TMP_PARENT"
trap cleanup EXIT
trap 'exit 130' INT TERM

MOUNTPOINT="$RUN_DIR/mount"
BACKING="$RUN_DIR/backing"
CONFIG="$RUN_DIR/config.json"
CLI_STDOUT="$RUN_DIR/cli.stdout"
CLI_STDERR="$RUN_DIR/cli.stderr"
RUST_BIN="$RUN_DIR/rust-fs-io"
mkdir "$MOUNTPOINT" "$BACKING"
chmod 700 "$RUN_DIR" "$MOUNTPOINT" "$BACKING"
MOUNTPOINT="$(cd "$MOUNTPOINT" && pwd -P)"

say "Building the CLI and compiling the process-level Rust client..."
cargo build --locked -p mount-rs-cli
rustc --edition=2024 "$RUST_SOURCE" -o "$RUST_BIN"

NO_COLOR=1 "$CLI_BIN" probe >"$RUN_DIR/probe.log" 2>&1 || {
    say "mount-rs probe failed; no native demo was attempted" >&2
    sed -n '1,120p' "$RUN_DIR/probe.log" >&2
    exit 1
}

if ! grep -Fq "platform: " "$RUN_DIR/probe.log"; then
    say "mount-rs probe returned an unrecognised result; no native demo was attempted" >&2
    sed -n '1,120p' "$RUN_DIR/probe.log" >&2
    exit 1
fi

case "$PLATFORM" in
    Darwin)
        if grep -Fq 'nfs: usable' "$RUN_DIR/probe.log"; then
            TRANSPORT="nfs"
        else
            blocked "macOS NFS is unavailable. The demo requires /sbin/mount_nfs, native NFS client access, an owned temporary mountpoint, and any required terminal Privacy permissions. See the probe output below.
$(sed -n '1,120p' "$RUN_DIR/probe.log")"
        fi
        ;;
    Linux)
        if grep -Fq 'nfs: usable' "$RUN_DIR/probe.log"; then
            TRANSPORT="nfs"
        elif grep -Fq 'fuse: usable' "$RUN_DIR/probe.log"; then
            TRANSPORT="fuse"
        else
            blocked "neither native NFS nor Linux FUSE is usable. NFS needs mount.nfs, kernel NFS support, and mount capability; FUSE needs /dev/fuse, fusermount3/fusermount, and permission to mount. See the probe output below.
$(sed -n '1,120p' "$RUN_DIR/probe.log")"
        fi
        ;;
esac

node --input-type=module - "$CONFIG" "$MOUNTPOINT" "$BACKING" "$TRANSPORT" <<'NODE'
import { writeFile } from "node:fs/promises";

const [configPath, mountpoint, backing, transport] = process.argv.slice(2);
const config = {
  version: 1,
  mountpoint,
  transport,
  quiet: true,
  driver: { kind: "host", root: backing },
};
await writeFile(configPath, `${JSON.stringify(config, null, 2)}\n`, { mode: 0o600 });
NODE

NO_COLOR=1 "$CLI_BIN" validate-config --config "$CONFIG" >"$RUN_DIR/config.validation" 2>&1 || {
    say "BLOCKED: the generated durable demo config did not validate" >&2
    sed -n '1,120p' "$RUN_DIR/config.validation" >&2
    exit 1
}

say "Starting the supervised mount-rs CLI with native $TRANSPORT..."
(
    exec env NO_COLOR=1 "$CLI_BIN" mount --config "$CONFIG"
) >"$CLI_STDOUT" 2>"$CLI_STDERR" &
CLI_PID=$!

if ! wait_for_native_mount; then
    say "BLOCKED: mount-rs did not report and expose a native $TRANSPORT mount within ${MOUNT_WAIT_SECONDS}s" >&2
    show_diagnostics
    exit 1
fi
say "PASS: kernel reports the CLI-owned native $TRANSPORT mount ready"

run_client_bounded rust-write-read \
    "$RUST_BIN" write-read "$MOUNTPOINT" "$RUST_FILE" "$RUST_PAYLOAD"
say "PASS: independent Rust process wrote and read bytes through the mounted path"

run_client_bounded node-write-read \
    node "$NODE_SOURCE" write-read "$MOUNTPOINT" "$JS_FILE" "$JS_PAYLOAD"
say "PASS: independent Node.js fs/promises process wrote and read bytes through the mounted path"

run_client_bounded rust-final-verify \
    "$RUST_BIN" verify "$MOUNTPOINT" "$JS_FILE" "$JS_PAYLOAD"
say "PASS: independent Rust process finally verified the bytes written by JavaScript"

if ! stop_cli; then
    say "BLOCKED: mount-rs did not stop cleanly within ${STOP_WAIT_SECONDS}s after SIGINT" >&2
    show_diagnostics
    exit 1
fi
if [ "$CLI_STATUS" -ne 0 ]; then
    say "BLOCKED: mount-rs exited with status $CLI_STATUS instead of completing its unmount path" >&2
    show_diagnostics
    exit 1
fi
if ! grep -Fq 'unmounted' "$CLI_STDOUT"; then
    say "BLOCKED: mount-rs exited without reporting unmounted" >&2
    show_diagnostics
    exit 1
fi
if ! wait_for_unmount; then
    say "BLOCKED: native $TRANSPORT mount remained after the CLI stop deadline" >&2
    show_diagnostics
    exit 1
fi
say "PASS: CLI stopped and the exact mountpoint is no longer mounted"

rmdir "$MOUNTPOINT"
run_client_bounded rust-backing-verify \
    "$RUST_BIN" verify "$BACKING" "$JS_FILE" "$JS_PAYLOAD"
say "PASS: durable host-backed bytes remain after unmount"
say "PASS: bounded end-to-end demo complete (transport=$TRANSPORT; Rust + Node.js shared one CLI-owned native mount)"
