#!/usr/bin/env python3
"""End-to-end probe of `mount-rs serve-http` on a durable split-store drive.

usage: scripts/serve-http-e2e.py [--smoke] <mount-rs binary> <empty work dir>
Prints one line per check: PASS/FAIL name detail. Exits 1 on any FAIL.

Runs on Linux, macOS and Windows. `--smoke` keeps the version check, the
basic round trip (health, auth, put, get, list), both graceful stops and the
restart, and leaves out the path, limit and concurrency suites; the release
workflow runs it against each packaged binary.

Graceful shutdown is asked for the way an operator on that platform would:
SIGTERM and then SIGINT on Unix; Ctrl-C and then Ctrl-Break on Windows,
delivered as console control events to the server's own console. On Windows
the full run also closes the server's console window, which is what clicking
its close button does: the server must release its drive within the five
seconds Windows allows after a console close, both when idle and when a
request that would hold the drain open for much longer is in flight.
"""
import concurrent.futures as cf
import hashlib
import http.client
import json
import os
import re
import signal
import subprocess
import sys
import threading
import time

ARGS = [a for a in sys.argv[1:] if a != "--smoke"]
SMOKE = len(ARGS) != len(sys.argv) - 1
if len(ARGS) != 2:
    raise SystemExit(__doc__)
WINDOWS = os.name == "nt"
# Absolute, so the path means the same thing whatever the server's directory.
BIN, WORK = os.path.abspath(ARGS[0]), os.path.abspath(ARGS[1])
if WINDOWS and not os.path.exists(BIN) and os.path.exists(BIN + ".exe"):
    BIN += ".exe"
TOKEN = "e2e-token-" + "x" * 24
CAP = 1 << 20  # 1 MiB request cap for the test
UPLOAD = 4 << 20  # largest file; bodies above CAP are streamed to storage
ENTRIES = 64
failures = []


def check(name, ok, detail=""):
    print(("PASS" if ok else "FAIL"), name, detail, flush=True)
    if not ok:
        failures.append(name)


def write_config(name="http.json", timeout_ms=5000):
    os.makedirs(os.path.join(WORK, "state"), exist_ok=True)
    cfg = {
        "version": 1,
        "http": {
            "host": "127.0.0.1",
            "port": 0,
            "max_request_bytes": CAP,
            "max_upload_bytes": UPLOAD,
            "max_directory_entries": ENTRIES,
            "request_timeout_ms": timeout_ms,
            "drain_timeout_ms": timeout_ms,
            "drives": [
                {
                    "id": "files",
                    "token": {"env": "E2E_TOKEN"},
                    "driver": {
                        "kind": "splitstore",
                        "storage": {
                            "metadata": {"kind": "sqlite", "path": "./state/meta.db"},
                            "blocks": {"kind": "sqlite", "path": "./state/blocks.db"},
                            "chunk_size_bytes": 65536,
                            "owner": "e2e",
                        },
                    },
                }
            ],
        },
    }
    path = os.path.join(WORK, name)
    with open(path, "w") as f:
        json.dump(cfg, f)
    return path


def start(config):
    env = dict(os.environ, E2E_TOKEN=TOKEN)
    # On Windows the server gets a console of its own, so a control event can
    # be sent to it without also reaching this script or the CI shell.
    flags = 0
    if WINDOWS:
        import ctypes
        flags = subprocess.CREATE_NEW_CONSOLE
        # A child inherits "ignore Ctrl-C" from its parent, and a CI runner
        # may start this script with it set. A console an operator opens
        # does not have it set, so clear it before starting the server.
        ctypes.WinDLL("kernel32").SetConsoleCtrlHandler(None, False)
    proc = subprocess.Popen(
        [BIN, "serve-http", "--config", config],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
        text=True,
        creationflags=flags,
    )
    line = proc.stdout.readline()
    m = re.search(r"http://127\.0\.0\.1:(\d+)", line)
    if not m:
        print(proc.stderr.read(), file=sys.stderr)
        raise SystemExit(f"no readiness line: {line!r}")
    # Kept, so a check can look for what the server said as it stopped.
    proc.out, proc.err = [line], []
    for stream, lines in ((proc.stdout, proc.out), (proc.stderr, proc.err)):
        threading.Thread(target=lines.extend, args=(stream,), daemon=True).start()
    return proc, int(m.group(1))


# Windows has no signals to send to another process. What an operator has is
# the console: Ctrl-C, Ctrl-Break, or closing the window. This helper joins
# the server's console and raises the event there, which is what the console
# does when the key is pressed. It ignores the event itself.
CONSOLE_EVENT_HELPER = """
import ctypes, sys, time
pid, event = int(sys.argv[1]), int(sys.argv[2])
kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
kernel32.FreeConsole()
if not kernel32.AttachConsole(pid):
    sys.exit("AttachConsole failed: %d" % ctypes.get_last_error())
# The event reaches every process on the console, this one included, so
# ignore it here. This has to follow AttachConsole to take effect.
ignore = ctypes.WINFUNCTYPE(ctypes.c_int, ctypes.c_uint)(lambda _event: 1)
kernel32.SetConsoleCtrlHandler(None, 1)
if not kernel32.SetConsoleCtrlHandler(ignore, 1):
    sys.exit("SetConsoleCtrlHandler failed: %d" % ctypes.get_last_error())
if not kernel32.GenerateConsoleCtrlEvent(event, 0):
    sys.exit("GenerateConsoleCtrlEvent failed: %d" % ctypes.get_last_error())
time.sleep(0.5)
print("sent")
"""


def request_stop(proc, second=False):
    """Ask the server to stop gracefully; returns the name of what was sent."""
    if WINDOWS:
        event = signal.CTRL_BREAK_EVENT if second else signal.CTRL_C_EVENT
        sent = subprocess.run(
            [sys.executable, "-I", "-c", CONSOLE_EVENT_HELPER, str(proc.pid), str(int(event))],
            capture_output=True, text=True, timeout=30,
            creationflags=subprocess.CREATE_NO_WINDOW,
        )
        # 0xC000013A is the helper ended by the event it raised: the event
        # was delivered to the console, which is all that is needed of it.
        if sent.returncode not in (0, 0xC000013A):
            raise SystemExit("could not send a console event: exit "
                             f"{sent.returncode:#x} {sent.stdout.strip()} {sent.stderr.strip()}")
        return "Ctrl-Break" if second else "Ctrl-C"
    proc.send_signal(signal.SIGINT if second else signal.SIGTERM)
    return "SIGINT" if second else "SIGTERM"


# Closing a console window posts WM_CLOSE to it; the console then sends
# CTRL_CLOSE_EVENT to every process attached to it and ends them about five
# seconds later. GenerateConsoleCtrlEvent cannot raise that event, so this
# helper does what the close button does: it finds the server's console
# window by attaching to the console, leaves it again, and posts WM_CLOSE.
# A console with no window (none can be created in this session) is a
# failure, not a skip.
CONSOLE_CLOSE_HELPER = """
import ctypes, sys
from ctypes import wintypes
pid = int(sys.argv[1])
kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
user32 = ctypes.WinDLL("user32", use_last_error=True)
kernel32.GetConsoleWindow.restype = wintypes.HWND
user32.PostMessageW.argtypes = [wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM]
kernel32.FreeConsole()
if not kernel32.AttachConsole(pid):
    sys.exit("AttachConsole failed: %d" % ctypes.get_last_error())
window = kernel32.GetConsoleWindow()
kernel32.FreeConsole()
if not window:
    sys.exit("the server's console has no window to close")
WM_CLOSE = 0x0010
if not user32.PostMessageW(window, WM_CLOSE, 0, 0):
    sys.exit("PostMessageW failed: %d" % ctypes.get_last_error())
print("posted WM_CLOSE to console window %#x" % window)
"""
# What Windows ends a console process with once the time it allows after a
# console close has run out.
STATUS_CONTROL_C_EXIT = 0xC000013A


def close_console(proc):
    """Close the server's console window; returns the exit code and seconds."""
    closed = subprocess.run(
        [sys.executable, "-I", "-c", CONSOLE_CLOSE_HELPER, str(proc.pid)],
        capture_output=True, text=True, timeout=30,
        creationflags=subprocess.CREATE_NO_WINDOW,
    )
    t0 = time.time()
    if closed.returncode != 0:
        proc.kill()
        proc.wait(30)
        raise SystemExit("could not close the server's console window: exit "
                         f"{closed.returncode:#x} {closed.stdout.strip()} {closed.stderr.strip()}")
    print("INFO", closed.stdout.strip(), flush=True)
    try:
        code = proc.wait(30)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(30)
        return "no exit within 30 s", time.time() - t0
    return code, time.time() - t0


def shown(code):
    return code if isinstance(code, str) else f"{code:#x}"


def restart_after_close(config, what):
    """Start again at once: only a released writer lease lets it start."""
    global PORT
    try:
        proc, PORT = start(config)
    except SystemExit as error:
        check(f"lease released by {what}: immediate restart starts", False, error)
        raise
    check(f"lease released by {what}: immediate restart starts", True)
    return proc


def console_close(config):
    """Windows: close the server's console, idle and then mid-request."""
    global PORT
    proc, PORT = start(config)
    marker = os.urandom(50_000)
    s, _, _ = req("PUT", FS + "/console-close", marker)
    check("put before console close", s == 200, s)
    code, took = close_console(proc)
    check("console close: exits 0 within 5 s", code == 0 and took < 5,
          (shown(code), f"{took:.2f}s"))
    check("console close: reports a clean stop",
          any("http stopped" in line for line in proc.out), proc.out[-3:])
    proc = restart_after_close(config, "console close")
    s, _, d = req("GET", FS + "/console-close")
    check("data survives console close", s == 200 and d == marker, s)
    stop(proc)

    # A request that has sent part of its body and then waits holds the
    # drain open for as long as the drain and request limits allow: 60 s
    # here, far beyond the 5 s Windows waits after a console close. The
    # server must cut the drain short and release the drive before then.
    long_config = write_config("http-long-drain.json", 60_000)
    proc, PORT = start(long_config)
    stalled = http.client.HTTPConnection("127.0.0.1", PORT, timeout=60)
    stalled.putrequest("PUT", FS + "/stalled")
    stalled.putheader("Authorization", f"Bearer {TOKEN}")
    stalled.putheader("Content-Length", str(CAP // 2))
    stalled.endheaders()
    stalled.send(b"x" * 4096)
    time.sleep(0.5)
    code, took = close_console(proc)
    stalled.close()
    killed = code == STATUS_CONTROL_C_EXIT or isinstance(code, str)
    check("console close mid-request: exits by itself within 5 s", not killed and took < 5,
          (shown(code), f"{took:.2f}s"))
    check("console close mid-request: says the drain was cut short",
          any("drain cut short" in line for line in proc.err), proc.err[-3:])
    proc = restart_after_close(config, "console close mid-request")
    s, _, d = req("GET", FS + "/console-close")
    check("data survives console close mid-request", s == 200 and d == marker, s)
    s, _, _ = req("GET", FS + "/stalled")
    check("unfinished upload is not stored", s == 404, s)
    stop(proc)


def stop(proc, second=False):
    """Ask for a graceful stop and check the server exits 0 within 30 s."""
    name = request_stop(proc, second)
    try:
        code = proc.wait(30)
    except subprocess.TimeoutExpired:
        proc.kill()
        code = "no exit within 30 s"
    check(f"{name} exits 0", code == 0, code)


PORT = None


def req(method, path, body=None, token=TOKEN, headers=None):
    conn = http.client.HTTPConnection("127.0.0.1", PORT, timeout=30)
    h = dict(headers or {})
    if token is not None:
        h["Authorization"] = f"Bearer {token}"
    if isinstance(body, (dict, list)):
        body = json.dumps(body).encode()
        h["Content-Type"] = "application/json"
    conn.request(method, path, body=body, headers=h)
    r = conn.getresponse()
    data = r.read()
    conn.close()
    return r.status, dict(r.getheaders()), data


FS = "/v1/drives/files/fs"


def basics():
    s, h, _ = req("GET", "/healthz", token=None)
    check("healthz", s == 200, s)
    lower = {k.lower(): v for k, v in h.items()}
    check("a response carries a generated request id",
          len(lower.get("x-request-id", "")) == 32, lower.get("x-request-id"))
    s, h, _ = req("GET", "/healthz", token=None, headers={"X-Request-Id": "e2e-probe.1"})
    lower = {k.lower(): v for k, v in h.items()}
    check("a client request id is echoed", lower.get("x-request-id") == "e2e-probe.1",
          lower.get("x-request-id"))
    s, _, d = req("GET", "/readyz", token=None)
    check("readyz", s == 200 and b"ready" in d, (s, d[:60]))
    s, _, _ = req("GET", "/v1/drives", token=None)
    check("no token is refused", s == 401, s)
    s, _, _ = req("GET", "/v1/drives", token="wrong")
    check("wrong token is refused", s in (401, 403), s)
    s, _, _ = req("GET", "/v1/drives/other/fs/x")
    check("unknown drive is not found or refused", s in (401, 403, 404), s)

    s, _, _ = req("POST", "/v1/drives/files/ops/mkdir", {"path": "/a/b", "recursive": True})
    check("mkdir recursive", s == 200, s)
    payload = os.urandom(300_000)
    s, _, d = req("PUT", FS + "/a/b/f.bin", payload)
    check("put", s == 200 and json.loads(d)["bytes"] == len(payload), (s, d[:80]))
    s, _, d = req("GET", FS + "/a/b/f.bin")
    check("get matches", s == 200 and d == payload, (s, len(d)))
    s, h, d = req("GET", FS + "/a/b/f.bin", headers={"Range": "bytes=1000-1999"})
    check("range read", s == 206 and d == payload[1000:2000], (s, len(d)))
    s, h, d = req("GET", FS + "/a/b/f.bin", headers={"Range": "bytes=999999999-"})
    check("unsatisfiable range is 416", s == 416, s)
    s, h, d = req("HEAD", FS + "/a/b/f.bin")
    check("head has length, no body", s == 200 and d == b"" and h.get("content-length") == str(len(payload)), (s, h.get("content-length")))
    s, _, _ = req("PUT", FS + "/a/b/f.bin", b"short")
    s2, _, d = req("GET", FS + "/a/b/f.bin")
    check("overwrite replaces whole file", s == 200 and d == b"short", (s, s2, d[:10]))
    s, _, _ = req("POST", "/v1/drives/files/ops/truncate", {"path": "/a/b/f.bin", "length": 2})
    _, _, d = req("GET", FS + "/a/b/f.bin")
    check("truncate", s == 200 and d == b"sh", (s, d))
    s, _, _ = req("POST", "/v1/drives/files/ops/rename", {"from": "/a/b/f.bin", "to": "/a/g.bin"})
    s2, _, _ = req("GET", FS + "/a/b/f.bin")
    s3, _, d = req("GET", FS + "/a/g.bin")
    check("rename", s == 200 and s2 == 404 and d == b"sh", (s, s2, s3))
    s, _, d = req("DELETE", FS + "/a")
    check("delete non-empty dir refused", s in (400, 409), (s, d[:80]))
    s, _, _ = req("DELETE", FS + "/a/g.bin")
    s2, _, _ = req("DELETE", FS + "/a/g.bin")
    check("delete then 404", s == 204 and s2 == 404, (s, s2))
    s, _, d = req("GET", "/v1/drives/files/stat/a/b")
    check("stat dir", s == 200 and "mode" in json.loads(d), s)
    req("PUT", FS + "/a/b/listed.txt", b"listed")
    s, _, d = req("GET", "/v1/drives/files/entries/a/b")
    check("list shows the file", s == 200 and "listed.txt" in d.decode(), (s, d[:120]))
    s, _, d = req("POST", "/v1/drives/files/ops/sync")
    check("sync", s == 204, s)


def paths():
    for p in ["/../etc/passwd", "/a/../../x", "/%2e%2e/x", "/a%2F..%2F..%2Fx", "/a/%00b", "//x"]:
        s, _, d = req("GET", FS + p)
        check(f"path {p!r} refused", s in (400, 404), (s, d[:60]))
    s, _, _ = req("PUT", FS + "/caf%C3%A9.txt", b"unicode")
    s2, _, d = req("GET", FS + "/caf%C3%A9.txt")
    check("utf-8 name round-trips", s == 200 and d == b"unicode", (s, s2))


def limits():
    s, _, _ = req("PUT", FS + "/cap-exact", os.urandom(CAP))
    check("upload at the cap", s == 200, s)
    streamed = os.urandom(3 * CAP + 7)
    s, _, d = req("PUT", FS + "/streamed", streamed)
    s2, _, d2 = req("GET", FS + "/streamed")
    check("upload above the request cap is streamed and stored whole",
          s == 200 and json.loads(d)["bytes"] == len(streamed) and d2 == streamed, (s, s2, len(d2)))
    s, _, _ = req("PUT", FS + "/upload-exact", os.urandom(UPLOAD))
    check("upload at the upload limit", s == 200, s)
    s, _, d = req("PUT", FS + "/cap-over", os.urandom(UPLOAD + 1))
    check("upload over the upload limit is 413", s == 413, (s, d[:80]))
    s2, _, _ = req("GET", FS + "/cap-over")
    check("rejected upload left nothing", s2 == 404, s2)
    req("POST", "/v1/drives/files/ops/mkdir", {"path": "/many"})
    for i in range(ENTRIES):
        req("PUT", FS + f"/many/{i:04}", b"x")
    s, _, d = req("GET", "/v1/drives/files/entries/many")
    check("listing at the entry cap", s == 200 and len(json.loads(d)) == ENTRIES, (s, len(d)))
    paged, cursor, pages = [], None, 0
    while True:
        query = "?limit=7" + (f"&cursor={cursor}" if cursor else "")
        s, _, d = req("GET", "/v1/drives/files/entries/many" + query)
        if s != 200:
            break
        page = json.loads(d)
        paged += [entry["name"] for entry in page["entries"]]
        cursor = page.get("next_cursor")
        pages += 1
        if not cursor or pages > ENTRIES:
            break
    check("paginated listing returns every entry once, in name order",
          s == 200 and paged == [f"{i:04}" for i in range(ENTRIES)], (s, pages, len(paged)))
    s, _, d = req("GET", "/v1/drives/files/entries/many?limit=0")
    check("bad limit is 400 EINVAL", s == 400 and json.loads(d)["error"]["code"] == "EINVAL", (s, d[:80]))
    req("PUT", FS + "/many/extra", b"x")
    s, _, d = req("GET", "/v1/drives/files/entries/many")
    check("listing over the entry cap refused, not truncated", s >= 400, (s, d[:100]))


def concurrency():
    req("POST", "/v1/drives/files/ops/mkdir", {"path": "/conc"})
    blobs = {i: os.urandom(50_000 + i) for i in range(64)}

    def put(i):
        return req("PUT", FS + f"/conc/{i}", blobs[i])[0]

    with cf.ThreadPoolExecutor(32) as pool:
        statuses = list(pool.map(put, blobs))
    check("64 parallel distinct puts", all(s == 200 for s in statuses), sorted(set(statuses)))

    def get(i):
        s, _, d = req("GET", FS + f"/conc/{i}")
        return s == 200 and d == blobs[i]

    with cf.ThreadPoolExecutor(32) as pool:
        ok = list(pool.map(get, list(blobs) * 4))
    check("256 parallel reads match", all(ok), ok.count(False))

    versions = [bytes([v]) * 200_000 for v in range(16)]

    def put_same(v):
        return req("PUT", FS + "/conc/same", versions[v])[0]

    with cf.ThreadPoolExecutor(16) as pool:
        statuses = list(pool.map(put_same, range(16)))
    s, _, d = req("GET", FS + "/conc/same")
    check("racing puts to one file leave one whole version",
          all(x == 200 for x in statuses) and d in versions, (sorted(set(statuses)), len(d), len(set(d))))

    t0 = time.time()
    n = 0
    with cf.ThreadPoolExecutor(32) as pool:
        futs = [pool.submit(req, "GET", FS + f"/conc/{i % 64}") for i in range(2000)]
        n = sum(1 for f in futs if f.result()[0] == 200)
    dt = time.time() - t0
    print(f"INFO 2000 reads of ~50 KB over 32 clients: {n} ok in {dt:.2f}s ({n / dt:.0f}/s)")

    t0 = time.time()
    with cf.ThreadPoolExecutor(32) as pool:
        futs = [pool.submit(req, "PUT", FS + f"/conc/w{i % 128}", b"y" * 4096) for i in range(1000)]
        n = sum(1 for f in futs if f.result()[0] == 200)
    dt = time.time() - t0
    print(f"INFO 1000 4 KiB writes over 32 clients: {n} ok in {dt:.2f}s ({n / dt:.0f}/s)")


def shutdown_and_restart(proc, config):
    global PORT
    marker = os.urandom(100_000)
    s, _, _ = req("PUT", FS + "/durable", marker)
    check("put before shutdown", s == 200, s)
    big = os.urandom(CAP)
    result = {}

    def slow_put():
        try:
            result["status"] = req("PUT", FS + "/inflight", big)[0]
        except Exception as error:  # connection closed counts as not acknowledged
            result["status"] = repr(error)

    t = threading.Thread(target=slow_put)
    t.start()
    time.sleep(0.01)
    stop(proc)
    t.join(30)
    proc2, PORT = start(config)
    s, _, d = req("GET", FS + "/durable")
    check("data survives restart", s == 200 and d == marker, s)
    s, _, d = req("GET", FS + "/inflight")
    acked = result.get("status") == 200
    check("in-flight upload: acknowledged means stored, else absent or whole",
          (acked and s == 200 and d == big) or (not acked and (s == 404 or d == big)),
          (result.get("status"), s, len(d)))
    if not SMOKE:
        s, _, d = req("GET", FS + "/conc/7")
        check("earlier files survive restart", s == 200 and len(d) == 50_007, s)
    return proc2


def main():
    global PORT
    version = subprocess.run([BIN, "--version"], capture_output=True, text=True, timeout=30)
    check("--version", version.returncode == 0 and version.stdout.startswith("mount-rs "),
          (version.returncode, version.stdout.strip()))
    config = write_config()
    proc, PORT = start(config)
    stopped = False
    try:
        basics()
        if not SMOKE:
            paths()
            limits()
            concurrency()
        proc = shutdown_and_restart(proc, config)
        # A second instance on the same storage must refuse to start.
        env = dict(os.environ, E2E_TOKEN=TOKEN)
        second = subprocess.run([BIN, "serve-http", "--config", config], env=env,
                                capture_output=True, text=True, timeout=30)
        check("second instance on the same drive refuses", second.returncode != 0,
              (second.returncode, second.stderr.strip()[-120:]))
        # The other stop an operator has: SIGINT on Unix, Ctrl-Break on Windows.
        stopped = True
        stop(proc, second=True)
        if WINDOWS and not SMOKE:
            console_close(config)
    finally:
        if not stopped and proc.poll() is None:
            proc.kill()
            proc.wait(30)
    print("FAILURES:", failures if failures else "none")
    sys.exit(1 if failures else 0)


main()
