#!/usr/bin/env python3
"""End-to-end probe of `mount-rs serve-http` on a durable split-store drive.

usage: scripts/serve-http-e2e.py <mount-rs binary> <empty work dir>
Prints one line per check: PASS/FAIL name detail. Exits 1 on any FAIL.
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

BIN, WORK = sys.argv[1], sys.argv[2]
TOKEN = "e2e-token-" + "x" * 24
CAP = 1 << 20  # 1 MiB request cap for the test
ENTRIES = 64
failures = []


def check(name, ok, detail=""):
    print(("PASS" if ok else "FAIL"), name, detail, flush=True)
    if not ok:
        failures.append(name)


def write_config():
    os.makedirs(os.path.join(WORK, "state"), exist_ok=True)
    cfg = {
        "version": 1,
        "http": {
            "host": "127.0.0.1",
            "port": 0,
            "max_request_bytes": CAP,
            "max_directory_entries": ENTRIES,
            "request_timeout_ms": 5000,
            "drain_timeout_ms": 5000,
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
    path = os.path.join(WORK, "http.json")
    with open(path, "w") as f:
        json.dump(cfg, f)
    return path


def start(config):
    env = dict(os.environ, E2E_TOKEN=TOKEN)
    proc = subprocess.Popen(
        [BIN, "serve-http", "--config", config],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
        text=True,
    )
    line = proc.stdout.readline()
    m = re.search(r"http://127\.0\.0\.1:(\d+)", line)
    if not m:
        print(proc.stderr.read(), file=sys.stderr)
        raise SystemExit(f"no readiness line: {line!r}")
    threading.Thread(target=proc.stdout.read, daemon=True).start()
    threading.Thread(target=proc.stderr.read, daemon=True).start()
    return proc, int(m.group(1))


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
    s, _, _ = req("GET", "/healthz", token=None)
    check("healthz", s == 200, s)
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
    s, _, d = req("PUT", FS + "/cap-over", os.urandom(CAP + 1))
    check("upload over the cap is 413", s == 413, (s, d[:80]))
    s2, _, _ = req("GET", FS + "/cap-over")
    check("rejected upload left nothing", s2 == 404, s2)
    req("POST", "/v1/drives/files/ops/mkdir", {"path": "/many"})
    for i in range(ENTRIES):
        req("PUT", FS + f"/many/{i:04}", b"x")
    s, _, d = req("GET", "/v1/drives/files/entries/many")
    check("listing at the entry cap", s == 200 and len(json.loads(d)) == ENTRIES, (s, len(d)))
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
    proc.send_signal(signal.SIGTERM)
    t.join(30)
    code = proc.wait(30)
    check("SIGTERM exits 0", code == 0, code)
    proc2, PORT = start(config)
    s, _, d = req("GET", FS + "/durable")
    check("data survives restart", s == 200 and d == marker, s)
    s, _, d = req("GET", FS + "/inflight")
    acked = result.get("status") == 200
    check("in-flight upload: acknowledged means stored, else absent or whole",
          (acked and s == 200 and d == big) or (not acked and (s == 404 or d == big)),
          (result.get("status"), s, len(d)))
    s, _, d = req("GET", FS + "/conc/7")
    check("earlier files survive restart", s == 200 and len(d) == 50_007, s)
    return proc2


def main():
    global PORT
    config = write_config()
    proc, PORT = start(config)
    try:
        basics()
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
    finally:
        proc.send_signal(signal.SIGTERM)
        proc.wait(30)
    print("FAILURES:", failures if failures else "none")
    sys.exit(1 if failures else 0)


main()
