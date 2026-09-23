#!/usr/bin/env python3
"""Run the native two-CLI FDB/RustFS NFS case inside the disposable RustFS harness.

Set MOUNT_RS_NATIVE_FDB_SERVER, MOUNT_RS_NATIVE_FDB_CLI, and
MOUNT_RS_NATIVE_FDB_CLIENT_LIB_DIR to matching arm64 FoundationDB 7.4 paths.
Invoke this with RUSTFS_COMBO_COMMAND from scripts/test-rustfs.sh so the RustFS
container and bucket are test owned as well.
"""

from __future__ import annotations

import os
import plistlib
import re
import secrets
import shutil
import signal
import socket
import subprocess
import tempfile
import time
from pathlib import Path
from urllib.parse import urlsplit


REPO = Path(__file__).resolve().parent.parent


def cluster_connection_string(run_id: str, port: int) -> str:
    # FoundationDB 7.4 permits underscores in the description, but its
    # cluster ID accepts alphanumeric characters only.
    if not run_id.isascii() or not run_id.isalnum() or not 1 <= port <= 65535:
        raise ValueError("disposable FoundationDB cluster ID/port is invalid")
    return f"mount_rs:{run_id}@127.0.0.1:{port}\n"


def required_file(name: str) -> Path:
    value = os.environ.get(name)
    if not value:
        raise RuntimeError(f"set {name} to the native FoundationDB 7.4 path")
    path = Path(value).resolve()
    if not path.is_file():
        raise RuntimeError(f"{name} does not name a file: {path}")
    return path


def fdbcli(cli: Path, cluster: Path, command: str, env: dict[str, str]) -> tuple[int, str]:
    result = subprocess.run(
        [str(cli), "-C", str(cluster), "--exec", command],
        env=env,
        capture_output=True,
        text=True,
        timeout=10,
        check=False,
    )
    return result.returncode, result.stdout + result.stderr


def stop_process(process: subprocess.Popen[bytes]) -> None:
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=10)


def owned_process_pids(run_dir: Path, server_bin: Path) -> set[int]:
    result = subprocess.run(
        ["ps", "-axo", "pid=,command="],
        capture_output=True,
        text=True,
        check=True,
        timeout=10,
    )
    target_prefix = str(run_dir / "target" / "debug" / "deps" / "native_two_process_foundationdb-")
    native_prefix = str(run_dir / "native" / "mount-rs-cli-two-process-")
    server_prefix = str(server_bin)
    cluster_argument = f" -C {run_dir / 'fdb.cluster'}"
    owned = set()
    for line in result.stdout.splitlines():
        pid_text, separator, command = line.strip().partition(" ")
        if not separator or not pid_text.isdigit():
            continue
        command = command.strip()
        if (
            command.startswith(target_prefix)
            or (command.startswith(native_prefix) and "/mount-rs-feature-on-test-cli" in command)
            or (command.startswith(server_prefix) and cluster_argument in command)
        ):
            pid = int(pid_text)
            if pid != os.getpid():
                owned.add(pid)
    return owned


def stop_owned_processes(run_dir: Path, server_bin: Path) -> None:
    for signum in (signal.SIGTERM, signal.SIGKILL):
        deadline = time.monotonic() + 10
        while True:
            owned = owned_process_pids(run_dir, server_bin)
            if not owned:
                return
            for pid in owned:
                try:
                    os.kill(pid, signum)
                except ProcessLookupError:
                    pass
            if time.monotonic() >= deadline:
                break
            time.sleep(0.1)
    remaining = owned_process_pids(run_dir, server_bin)
    if remaining:
        raise RuntimeError(f"preserving native root with live owned processes: {sorted(remaining)}")


def is_mounted(target: Path) -> bool:
    table = subprocess.run(
        ["mount"], capture_output=True, text=True, check=True, timeout=10
    ).stdout
    canonical = target.resolve()
    for line in table.splitlines():
        _, separator, mounted = line.partition(" on ")
        if separator and " (" in mounted:
            entry = Path(mounted.rsplit(" (", 1)[0])
            if entry.resolve() == canonical:
                return True
    return False


def cleanup_owned_native_root(root: Path, *, remove: bool = True) -> None:
    marker = root / ".mount-rs-native-fdb-rustfs-owned"
    if root.is_symlink() or marker.is_symlink() or not marker.is_file():
        raise RuntimeError(f"preserving native root without ownership marker: {root}")
    if marker.read_text().strip() != str(os.getpid()):
        raise RuntimeError(f"preserving native root without matching ownership: {root}")
    scopes = list(root.glob("mount-rs-cli-two-process-*"))
    for scope in scopes:
        if scope.is_symlink() or not scope.is_dir():
            raise RuntimeError(f"preserving unexpected native scope: {scope}")
        for name in ("mount-a", "mount-b"):
            mountpoint = scope / name
            if mountpoint.is_symlink():
                raise RuntimeError(f"preserving native root with symlinked mountpoint: {mountpoint}")
    for scope in scopes:
        for name in ("mount-a", "mount-b"):
            mountpoint = scope / name
            if mountpoint.is_symlink():
                raise RuntimeError(f"preserving native root with symlinked mountpoint: {mountpoint}")
            if mountpoint.is_dir() and is_mounted(mountpoint):
                if mountpoint.is_symlink():
                    raise RuntimeError(
                        f"preserving native root with symlinked mountpoint: {mountpoint}"
                    )
                subprocess.run(
                    ["umount", "-f", str(mountpoint)],
                    capture_output=True,
                    text=True,
                    check=False,
                    timeout=15,
                )
            if mountpoint.is_symlink():
                raise RuntimeError(f"preserving native root with symlinked mountpoint: {mountpoint}")
            if mountpoint.is_dir() and is_mounted(mountpoint):
                raise RuntimeError(f"preserving native root with attached mount: {mountpoint}")
    if remove:
        shutil.rmtree(root)


def verify_owned_run_dir(run_dir: Path) -> None:
    marker = run_dir / ".mount-rs-native-fdb-rustfs-owned"
    if (
        run_dir.is_symlink()
        or not run_dir.name.startswith("mount-rs-native-fdb-rustfs-")
        or marker.is_symlink()
        or not marker.is_file()
        or marker.read_text().strip() != str(os.getpid())
    ):
        raise RuntimeError(f"preserving unexpected native FDB/RustFS run directory: {run_dir}")


def attached_owned_fdb_data_device(run_dir: Path, data_dir: Path) -> str | None:
    verify_owned_run_dir(run_dir)
    image = run_dir / "fdb-data.sparseimage"
    inventory = plistlib.loads(
        subprocess.run(
            ["hdiutil", "info", "-plist"],
            capture_output=True,
            check=True,
            timeout=10,
        ).stdout
    )
    matches = [
        entry
        for entry in inventory.get("images", [])
        if Path(entry.get("image-path", "/")).resolve() == image.resolve()
    ]
    if len(matches) > 1:
        raise RuntimeError(f"preserving ambiguous owned FoundationDB image: {image}")
    if not matches:
        if is_mounted(data_dir):
            raise RuntimeError(f"preserving FoundationDB data mount without owned image: {data_dir}")
        return None
    entry = matches[0]
    entities = entry.get("system-entities", [])
    mounted = [
        Path(entity["mount-point"]).resolve()
        for entity in entities
        if "mount-point" in entity
    ]
    device = entities[0].get("dev-entry") if entities else None
    if (
        entry.get("owner-uid") != os.getuid()
        or entry.get("image-type") != "sparse disk image"
        or mounted != [data_dir.resolve()]
        or not isinstance(device, str)
        or re.fullmatch(r"/dev/disk[0-9]+", device) is None
    ):
        raise RuntimeError(f"preserving unexpected owned FoundationDB image attachment: {image}")
    return device


def attach_owned_fdb_data_image(run_dir: Path, data_dir: Path, run_id: str) -> str:
    verify_owned_run_dir(run_dir)
    image = run_dir / "fdb-data.sparseimage"
    subprocess.run(
        [
            "hdiutil", "create", "-size", "1g", "-fs", "APFS", "-type", "SPARSE",
            "-volname", f"mount_rs_fdb_{run_id}", str(image),
        ],
        capture_output=True,
        check=True,
        timeout=30,
    )
    subprocess.run(
        ["hdiutil", "attach", "-nobrowse", "-mountpoint", str(data_dir), str(image)],
        capture_output=True,
        check=True,
        timeout=30,
    )
    device = attached_owned_fdb_data_device(run_dir, data_dir)
    if device is None:
        raise RuntimeError("test-owned FoundationDB data image did not attach")
    return device


def detach_owned_fdb_data_image(run_dir: Path, data_dir: Path) -> None:
    device = attached_owned_fdb_data_device(run_dir, data_dir)
    if device is None:
        return
    result = subprocess.run(
        ["hdiutil", "detach", device],
        capture_output=True,
        text=True,
        check=False,
        timeout=30,
    )
    if result.returncode != 0 or attached_owned_fdb_data_device(run_dir, data_dir) is not None:
        raise RuntimeError(f"preserving attached owned FoundationDB data image: {device}")


def remove_owned_run_dir_if_detached(run_dir: Path, data_dir: Path, native_root: Path) -> None:
    verify_owned_run_dir(run_dir)
    if attached_owned_fdb_data_device(run_dir, data_dir) is not None:
        raise RuntimeError(f"preserving native run directory with attached data image: {run_dir}")
    if native_root.exists() or native_root.is_symlink():
        raise RuntimeError(f"preserving native run directory with uncleared NFS root: {run_dir}")
    shutil.rmtree(run_dir)


def finish_owned_run_cleanup(
    run_dir: Path, data_dir: Path, native_root: Path, *, processes_stopped: bool
) -> None:
    # Exact NFS paths may be detached even if a process exit is uncertain,
    # but the backing disk/root must remain until all owners are proved gone.
    cleanup_owned_native_root(native_root, remove=False)
    if not processes_stopped:
        raise RuntimeError("preserving owned FDB image/root because processes are still live")
    detach_owned_fdb_data_image(run_dir, data_dir)
    cleanup_owned_native_root(native_root)
    remove_owned_run_dir_if_detached(run_dir, data_dir, native_root)


def create_owned_native_fixture(
    port: int,
) -> tuple[Path, Path, Path, Path, Path, Path, str]:
    # mkdtemp creates a private 0700 root. No process, NFS mount, or disk
    # image exists until this setup succeeds, so a partial setup may be
    # removed under its exact new prefix even if the marker write failed.
    run_dir = Path(tempfile.mkdtemp(prefix="mount-rs-native-fdb-rustfs-"))
    native_root = run_dir / "native"
    data_dir = run_dir / "data"
    log_dir = run_dir / "logs"
    cluster = run_dir / "fdb.cluster"
    server_log = run_dir / "fdbserver.stdout.log"
    run_id = secrets.token_hex(8)
    try:
        run_dir.chmod(0o700)
        (run_dir / ".mount-rs-native-fdb-rustfs-owned").write_text(f"{os.getpid()}\n")
        native_root.mkdir(mode=0o700)
        (native_root / ".mount-rs-native-fdb-rustfs-owned").write_text(f"{os.getpid()}\n")
        data_dir.mkdir()
        log_dir.mkdir()
        cluster.write_text(cluster_connection_string(run_id, port))
    except BaseException:
        if run_dir.is_dir() and not run_dir.is_symlink() and run_dir.name.startswith(
            "mount-rs-native-fdb-rustfs-"
        ):
            shutil.rmtree(run_dir)
        raise
    return run_dir, native_root, data_dir, log_dir, cluster, server_log, run_id


def main() -> None:
    def interrupted(_signum: int, _frame: object) -> None:
        raise KeyboardInterrupt("native FDB/RustFS runner interrupted")

    signal.signal(signal.SIGTERM, interrupted)
    server_bin = required_file("MOUNT_RS_NATIVE_FDB_SERVER")
    cli_bin = required_file("MOUNT_RS_NATIVE_FDB_CLI")
    library_dir_value = os.environ.get("MOUNT_RS_NATIVE_FDB_CLIENT_LIB_DIR")
    if not library_dir_value:
        raise RuntimeError("set MOUNT_RS_NATIVE_FDB_CLIENT_LIB_DIR")
    library_dir = Path(library_dir_value).resolve()
    if not (library_dir / "libfdb_c.dylib").is_file():
        raise RuntimeError("native FoundationDB client library is missing")
    rustfs_run = os.environ.get("RUSTFS_RUN_DIR")
    rustfs_container = os.environ.get("RUSTFS_HARNESS_CONTAINER")
    if not rustfs_run or not rustfs_container:
        raise RuntimeError("run from scripts/test-rustfs.sh's disposable container harness")
    rustfs_run_dir = Path(rustfs_run)
    owned = rustfs_run_dir / ".mount-rs-rustfs-owned"
    if rustfs_run_dir.is_symlink() or owned.is_symlink() or not owned.is_file():
        raise RuntimeError("RustFS harness ownership marker is unavailable")
    if owned.read_text().strip() != rustfs_container:
        raise RuntimeError("RustFS harness ownership marker does not match")
    endpoint = urlsplit(os.environ.get("RUSTFS_ENDPOINT", ""))
    if (
        endpoint.scheme != "http"
        or endpoint.hostname != "127.0.0.1"
        or not endpoint.port
        or endpoint.path not in ("", "/")
        or endpoint.query
        or endpoint.fragment
        or endpoint.username
        or endpoint.password
    ):
        raise RuntimeError("disposable RustFS endpoint must be loopback HTTP")

    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
    (
        run_dir, native_root, data_dir, log_dir, cluster, server_log, run_id
    ) = create_owned_native_fixture(port)

    env = os.environ.copy()
    env["FDB_CLIENT_LIB_PATH"] = str(library_dir)
    env["DYLD_LIBRARY_PATH"] = str(library_dir) + (
        ":" + env["DYLD_LIBRARY_PATH"] if env.get("DYLD_LIBRARY_PATH") else ""
    )
    env["MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE"] = str(cluster)
    env["MOUNT_RS_FOUNDATIONDB_DISPOSABLE_CLUSTER"] = "1"
    env["MOUNT_RS_CLI_NATIVE_FOUNDATIONDB_TWO_PROCESS"] = "1"
    env["MOUNT_RS_CLI_NATIVE_RUSTFS_DISPOSABLE"] = "1"
    env["MOUNT_RS_CLI_NATIVE_NFS"] = "1"
    try:
        shared_target = Path(
            env.get("CARGO_TARGET_DIR", "/private/tmp/mount-rs-rustfs-native-target")
        ).resolve()
        shared_target.mkdir(parents=True, exist_ok=True)
        target_link = run_dir / "target"
        target_link.symlink_to(shared_target, target_is_directory=True)
        env["CARGO_TARGET_DIR"] = str(target_link)
        env["TMPDIR"] = str(native_root)
        attach_owned_fdb_data_image(run_dir, data_dir, run_id)
    except BaseException:
        # No FDB/CLI process exists yet, but an interrupted hdiutil attach
        # may already have mounted the image. Never rmtree a mounted data dir.
        detach_owned_fdb_data_image(run_dir, data_dir)
        cleanup_owned_native_root(native_root)
        remove_owned_run_dir_if_detached(run_dir, data_dir, native_root)
        raise

    server: subprocess.Popen[bytes] | None = None
    cargo: subprocess.Popen[bytes] | None = None
    passed = False
    try:
        with server_log.open("wb") as output:
            server = subprocess.Popen(
                [
                    str(server_bin),
                    "-p",
                    f"127.0.0.1:{port}",
                    "-C",
                    str(cluster),
                    "-d",
                    str(data_dir),
                    "-L",
                    str(log_dir),
                    "-m",
                    "512MiB",
                    "-M",
                    "128MiB",
                    "--cache-memory",
                    "128MiB",
                ],
                env=env,
                stdout=output,
                stderr=subprocess.STDOUT,
            )
        deadline = time.monotonic() + 90
        last_output = ""
        last_status = ""
        probe_key = f"mount_rs_ready_{run_id}"
        probe_value = run_id
        configured = False
        while time.monotonic() < deadline:
            if server.poll() is not None:
                raise RuntimeError(f"test-owned fdbserver exited {server.returncode}")
            try:
                if not configured:
                    code, last_output = fdbcli(cli_bin, cluster, "configure new single ssd", env)
                    configured = (code == 0 and "Database created" in last_output) or (
                        "Database already exists" in last_output
                    )
                if configured:
                    code, last_output = fdbcli(
                        cli_bin,
                        cluster,
                        f"writemode on; set {probe_key} {probe_value}",
                        env,
                    )
                    if code == 0 and "Committed (" in last_output:
                        code, last_output = fdbcli(cli_bin, cluster, f"get {probe_key}", env)
                        if code == 0 and f"`{probe_key}' is `{probe_value}'" in last_output:
                            break
                _, last_status = fdbcli(cli_bin, cluster, "status minimal", env)
            except subprocess.TimeoutExpired:
                last_status = "fdbcli command exceeded 10 seconds"
            time.sleep(1)
        else:
            raise RuntimeError(
                "disposable FoundationDB did not complete a test-owned transaction: "
                f"last_command={last_output[-300:]} status={last_status[-300:]}"
            )
        print(f"NATIVE_FDB_RUSTFS_READY cluster=127.0.0.1:{port}", flush=True)

        cargo = subprocess.Popen(
            [
                str(REPO / "scripts/cargo-shared"),
                "test",
                "--locked",
                "-p",
                "mount-rs-cli",
                "--features",
                "foundationdb",
                "--test",
                "native_two_process_foundationdb",
                "--",
                "cli_two_process_foundationdb_rustfs_volume_stays_coherent_and_reopens",
                "--exact",
                "--ignored",
                "--nocapture",
            ],
            cwd=REPO,
            env=env,
        )
        try:
            result = cargo.wait(timeout=600)
        except subprocess.TimeoutExpired as error:
            raise RuntimeError("native two-CLI FDB/RustFS test exceeded 600 seconds") from error
        if result != 0:
            raise RuntimeError(f"native two-CLI FDB/RustFS test exited {result}")
        passed = True
    except BaseException:
        if server_log.is_file():
            print("NATIVE_FDB_SERVER_STDOUT_TAIL", server_log.read_text(errors="replace")[-1600:], flush=True)
        for trace in sorted(log_dir.glob("trace.*")):
            errors = [
                line for line in trace.read_text(errors="replace").splitlines()
                if 'Severity="40"' in line or 'Severity="30"' in line
            ]
            if errors:
                print("NATIVE_FDB_SERVER_TRACE_ERRORS", "\n".join(errors[-8:]), flush=True)
        raise
    finally:
        cleanup_errors = []
        if cargo is not None:
            try:
                stop_process(cargo)
            except BaseException as error:
                cleanup_errors.append(f"cargo leader: {error}")
        try:
            stop_owned_processes(run_dir, server_bin)
        except BaseException as error:
            cleanup_errors.append(f"owned descendants: {error}")
        if server is not None:
            try:
                stop_process(server)
            except BaseException as error:
                cleanup_errors.append(f"fdbserver leader: {error}")
        processes_stopped = not cleanup_errors and (server is None or server.poll() is not None)
        try:
            finish_owned_run_cleanup(
                run_dir, data_dir, native_root, processes_stopped=processes_stopped
            )
        except BaseException as error:
            cleanup_errors.append(f"owned NFS/image/temp: {error}")
        if cleanup_errors:
            print(
                f"NATIVE_FDB_RUSTFS_CLEANUP_INCOMPLETE run_dir={run_dir} errors={cleanup_errors}",
                flush=True,
            )
            raise RuntimeError("native FDB/RustFS runner could not prove exact cleanup")
        print("NATIVE_FDB_RUSTFS_OWNED_NFS_AND_TEMP_CLEAN", flush=True)
    if passed:
        print("NATIVE_FDB_RUSTFS_TWO_PROCESS_PASS", flush=True)


if __name__ == "__main__":
    main()
