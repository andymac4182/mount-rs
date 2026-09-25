#!/usr/bin/env python3
"""Run isolated plain and TLS/auth directory fixtures; requires redis-server."""
import os
from pathlib import Path
import secrets
import shutil
import socket
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent


def run_fixture(test_name, authenticated):
    executable = shutil.which("redis-server")
    if not executable:
        raise SystemExit("redis-server is required for these explicit integration tests")
    with tempfile.TemporaryDirectory(prefix="mount-rs-cache-redis-") as directory:
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            port = reservation.getsockname()[1]
        password = secrets.token_hex(24) if authenticated else None
        config = Path(directory) / "redis.conf"
        text = f'bind 127.0.0.1\nport {port}\nsave ""\nappendonly no\ndir "{directory}"\n'
        if password:
            text += f"requirepass {password}\n"
        config.write_text(text)
        config.chmod(0o600)
        with (Path(directory) / "redis.log").open("wb") as log:
            process = subprocess.Popen([executable, str(config)], stdout=log, stderr=log)
            try:
                ready = False
                for _ in range(100):
                    if process.poll() is not None:
                        raise RuntimeError("isolated Redis failed to start")
                    try:
                        with socket.create_connection(("127.0.0.1", port), timeout=0.1) as client:
                            client.sendall(b"*1\r\n$4\r\nPING\r\n")
                            response = client.recv(128)
                            ready = response.startswith((b"+PONG", b"-NOAUTH"))
                    except OSError:
                        pass
                    if ready:
                        break
                    time.sleep(0.05)
                if not ready:
                    raise RuntimeError("isolated Redis readiness timed out")
                environment = os.environ.copy()
                environment["MOUNT_RS_CACHE_REDIS_ADDRESS"] = f"127.0.0.1:{port}"
                if password:
                    environment["MOUNT_RS_CACHE_REDIS_PASSWORD"] = password
                else:
                    environment.pop("MOUNT_RS_CACHE_REDIS_PASSWORD", None)
                subprocess.run([str(ROOT / "scripts/cargo-shared"), "test", "-p", "mount-rs-blob-cache", "--lib", test_name, "--offline", "--locked", "--", "--ignored", "--nocapture"], cwd=ROOT, env=environment, check=True)
            finally:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()


if __name__ == "__main__":
    run_fixture("real_redis_hints_leases_expire_and_untrusted_ids_never_enter", False)
    run_fixture("real_redis_tls_auth_and_certificate_validation", True)
