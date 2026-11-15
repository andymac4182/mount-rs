#!/usr/bin/env python3
"""End-to-end checks of a two-node serve-http forwarding cluster.

usage: MOUNT_RS_REDIS_URL=redis://... serve-http-cluster-e2e.py <binary> <empty work dir>

Starts two `serve-http` owners on loopback that share a disposable Redis
namespace and forward to each other over mutually authenticated HTTP/3:
node a owns drive a and node b owns drive b. A client that picks either node
for any request, as a round-robin load balancer would, must see one
consistent drive. Needs `openssl` for the test PKI. Prints PASS/FAIL per
check and exits 1 on any FAIL.
"""
import concurrent.futures as cf
import hashlib
import http.client
import json
import os
import re
import signal
import socket
import subprocess
import sys
import threading
import time
import uuid


TOKEN = "cluster-token-" + "x" * 24


def sh(*args, cwd):
    subprocess.run(args, cwd=cwd, check=True, capture_output=True)


def free_udp():
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def make_pki(work, nodes):
    """A CA and one peer certificate per node, as the forwarding transport expects."""
    sh("openssl", "req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256",
       "-nodes", "-keyout", "ca.key", "-out", "ca.pem", "-days", "2", "-subj", "/CN=mount-rs test CA",
       "-addext", "basicConstraints=critical,CA:TRUE", "-addext", "keyUsage=critical,keyCertSign",
       cwd=work)
    pins = {}
    for node in nodes:
        sh("openssl", "req", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes",
           "-keyout", f"{node}.key", "-out", f"{node}.csr", "-subj", f"/CN={node}", cwd=work)
        ext = os.path.join(work, f"{node}.ext")
        with open(ext, "w") as f:
            f.write(f"subjectAltName=DNS:localhost,URI:urn:agent-tunnel:peer:{node}\n"
                    "keyUsage=critical,digitalSignature\n"
                    "extendedKeyUsage=clientAuth,serverAuth\n"
                    "basicConstraints=critical,CA:FALSE\n")
        sh("openssl", "x509", "-req", "-in", f"{node}.csr", "-CA", "ca.pem", "-CAkey", "ca.key",
           "-CAcreateserial", "-out", f"{node}.pem", "-days", "2", "-extfile", ext, cwd=work)
        der = subprocess.run(["openssl", "pkey", "-in", f"{node}.key", "-pubout", "-outform", "DER"],
                             cwd=work, check=True, capture_output=True).stdout
        pins[node] = hashlib.sha256(der).hexdigest()
    return pins


class Node:
    def __init__(self, name, proc, port):
        self.name, self.proc, self.port = name, proc, port


def start_cluster(binary, work, nodes, sessions=True, extra_http=None):
    """Return {node: Node}. Node `n` owns drive `n`."""
    os.makedirs(work, exist_ok=True)
    pins = make_pki(work, nodes)
    addresses = {n: f"127.0.0.1:{free_udp()}" for n in nodes}
    peers = [{"node": n, "boot": f"{n}-boot", "address": addresses[n],
              "tls_server_name": "localhost", "spki_sha256": pins[n]} for n in nodes]
    with open(os.path.join(work, "peers.json"), "w") as f:
        json.dump(peers, f)
    namespace = "fwd-" + uuid.uuid4().hex[:12]
    started = {}
    try:
        for n in nodes:
            os.makedirs(os.path.join(work, n), exist_ok=True)
            http = {
                "host": "127.0.0.1", "port": 0, "remote_sessions": sessions,
                "max_request_bytes": 16 << 20,
                "drives": [{"id": n, "token": {"env": "CLUSTER_TOKEN"}, "driver": {
                    "kind": "splitstore", "storage": {
                        "metadata": {"kind": "sqlite", "path": f"./{n}/meta.db"},
                        "blocks": {"kind": "sqlite", "path": f"./{n}/blocks.db"},
                        "chunk_size_bytes": 65536, "owner": n}}}],
                "forwarding": {
                    "peers": "peers.json", "certificate": f"{n}.pem", "private_key": f"{n}.key",
                    "ca": "ca.pem", "redis_url": {"env": "MOUNT_RS_REDIS_URL"},
                    "namespace": namespace, "bind": "127.0.0.1:0", "owner": True},
            }
            http.update(extra_http or {})
            path = os.path.join(work, f"{n}.json")
            with open(path, "w") as f:
                json.dump({"version": 1, "http": http}, f)
            started[n] = launch(binary, work, n)
    except BaseException:
        stop_cluster(started)
        raise
    return started


def launch(binary, work, n):
    """Start node `n` from the config `start_cluster` wrote for it."""
    path = os.path.join(work, f"{n}.json")
    proc = subprocess.Popen([binary, "serve-http", "--config", path], text=True,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            env=dict(os.environ, CLUSTER_TOKEN=TOKEN))
    line = proc.stdout.readline()
    m = re.search(r"127\.0\.0\.1:(\d+)", line)
    if not m:
        raise SystemExit(f"{n} did not start: {line!r} {proc.stderr.read()[-2000:]}")
    threading.Thread(target=proc.stdout.read, daemon=True).start()
    threading.Thread(target=proc.stderr.read, daemon=True).start()
    return Node(n, proc, int(m.group(1)))


restart_node = launch


def stop_cluster(nodes):
    codes = {}
    for node in nodes.values():
        if node.proc.poll() is None:
            node.proc.send_signal(signal.SIGTERM)
    for node in nodes.values():
        try:
            codes[node.name] = node.proc.wait(60)
        except subprocess.TimeoutExpired:
            node.proc.kill()
            codes[node.name] = "killed"
    return codes


BIN, WORK = sys.argv[1], sys.argv[2]
CAP = 8 << 20
failures = []
nodes = {}


def check(name, ok, detail=""):
    print(("PASS" if ok else "FAIL"), name, detail, flush=True)
    if not ok:
        failures.append(name)


def req(node, method, path, body=None, token=TOKEN, headers=None, timeout=30):
    conn = http.client.HTTPConnection("127.0.0.1", nodes[node].port, timeout=timeout)
    h = dict(headers or {})
    if token is not None:
        h["Authorization"] = f"Bearer {token}"
    if isinstance(body, (dict, list)):
        body = json.dumps(body).encode()
        h["Content-Type"] = "application/json"
    try:
        conn.request(method, path, body=body, headers=h)
        r = conn.getresponse()
        return r.status, dict(r.getheaders()), r.read()
    finally:
        conn.close()


def fs(drive, path):
    return f"/v1/drives/{drive}/fs{path}"


def every_route():
    """Each drive through each ingress: one local, one forwarded."""
    for drive in ("a", "b"):
        for ingress in ("a", "b"):
            via = "local" if drive == ingress else "forwarded"
            tag = f"drive {drive} via {ingress} ({via})"
            payload = os.urandom(300_000)
            name = f"/route-{ingress}.bin"
            s, _, d = req(ingress, "PUT", fs(drive, name), payload)
            check(f"{tag}: put", s == 200 and json.loads(d)["bytes"] == len(payload), (s, d[:80]))
            other = "b" if ingress == "a" else "a"
            s, _, d = req(other, "GET", fs(drive, name))
            check(f"{tag}: read back through the other ingress", s == 200 and d == payload, (s, len(d)))
            s, _, d = req(ingress, "GET", fs(drive, name), headers={"Range": "bytes=100-199"})
            check(f"{tag}: range", s == 206 and d == payload[100:200], (s, len(d)))
            s, h, d = req(ingress, "HEAD", fs(drive, name))
            check(f"{tag}: head", s == 200 and d == b"" and h.get("content-length") == str(len(payload)),
                  (s, h.get("content-length")))
            s, _, d = req(ingress, "GET", f"/v1/drives/{drive}/stat{name}")
            check(f"{tag}: stat", s == 200 and json.loads(d)["size"] == len(payload), s)
            s, _, _ = req(ingress, "GET", fs(drive, name), token="wrong-token-" + "y" * 24)
            check(f"{tag}: wrong token refused", s in (401, 403), s)
            s, _, _ = req(ingress, "DELETE", fs(drive, name))
            s2, _, _ = req(other, "GET", fs(drive, name))
            check(f"{tag}: delete seen by the other ingress", s == 204 and s2 == 404, (s, s2))
    s, _, _ = req("a", "GET", fs("nowhere", "/x"))
    check("unknown drive is refused, not forwarded forever", s in (401, 403, 404, 502, 503), s)


def round_robin():
    """Alternate ingresses on every request against one file."""
    ok = True
    detail = ""
    for i in range(200):
        drive = "ab"[i % 2]
        body = f"version {i}".encode()
        s, _, _ = req("ab"[i % 2], "PUT", fs(drive, "/rr"), body)
        s2, _, d = req("ab"[(i + 1) % 2], "GET", fs(drive, "/rr"))
        if s != 200 or s2 != 200 or d != body:
            ok, detail = False, (i, s, s2, d[:40])
            break
    check("200 writes each read back through the other ingress", ok, detail)


def big_and_limits():
    big = os.urandom(CAP)
    s, _, _ = req("a", "PUT", fs("b", "/big"), big, timeout=120)
    s2, _, d = req("a", "GET", fs("b", "/big"), timeout=120)
    check("forwarded upload and download at the cap", s == 200 and s2 == 200 and d == big, (s, s2, len(d)))
    s, _, d = req("a", "PUT", fs("b", "/over"), os.urandom(CAP + 1), timeout=120)
    check("forwarded upload over the cap is 413", s == 413, (s, d[:80]))
    s, _, _ = req("b", "GET", fs("b", "/over"))
    check("rejected forwarded upload left nothing", s == 404, s)


def concurrency():
    blobs = {i: os.urandom(20_000 + i) for i in range(64)}

    def put(i):
        return req("ab"[i % 2], "PUT", fs("ab"[(i // 2) % 2], f"/c{i}"), blobs[i])[0]

    def get(i):
        s, _, d = req("ab"[(i + 1) % 2], "GET", fs("ab"[(i // 2) % 2], f"/c{i}"))
        return s == 200 and d == blobs[i]

    with cf.ThreadPoolExecutor(32) as pool:
        statuses = list(pool.map(put, blobs))
        check("64 parallel puts across both ingresses and drives", all(s == 200 for s in statuses),
              sorted(set(statuses)))
        ok = list(pool.map(get, list(blobs) * 4))
        check("256 parallel reads across both ingresses match", all(ok), ok.count(False))


def sessions():
    """A session lives on the drive's owner; any ingress may carry its requests."""
    s, _, d = req("a", "POST", "/v1/drives/b/sessions")
    check("session created through the non-owner", s == 201, (s, d[:80]))
    if s != 201:
        return
    token = json.loads(d)["session"]
    sh = {"x-mount-session": token}
    flags = {"read": True, "write": True, "create": True, "truncate": True,
             "append": False, "exclusive": False}
    s, _, d = req("b", "POST", "/v1/drives/b/sessions/handles",
                  {"operation": "open", "path": "/sess", "flags": flags, "mode": 420}, headers=sh)
    check("session handle opened through the owner", s == 200, (s, d[:80]))
    handle = json.loads(d)["handle"] if s == 200 else 0
    s, _, _ = req("a", "PUT", "/v1/drives/b/sessions/handles", b"hello world",
                  headers={**sh, "x-mount-handle": str(handle), "x-mount-position": "0"})
    s2, _, _ = req("b", "PUT", "/v1/drives/b/sessions/handles", b"HELLO",
                   headers={**sh, "x-mount-handle": str(handle), "x-mount-position": "0"})
    s3, _, d = req("a", "GET", "/v1/drives/b/sessions/handles",
                   headers={**sh, "x-mount-handle": str(handle), "x-mount-position": "0",
                            "x-mount-length": "64"})
    check("positional writes and reads on one handle through both ingresses",
          (s, s2, s3) == (200, 200, 200) and d == b"HELLO world", (s, s2, s3, d))
    s, _, _ = req("b", "POST", "/v1/drives/b/sessions/handles",
                  {"operation": "close", "handle": handle}, headers=sh)
    s2, _, d = req("a", "GET", fs("b", "/sess"))
    check("handle close, then plain read sees the session's writes", s == 204 and d == b"HELLO world",
          (s, s2, d))
    s, _, _ = req("b", "DELETE", "/v1/drives/b/sessions", headers=sh)
    check("session closed through the other ingress", s == 204, s)
    s, _, _ = req("a", "POST", "/v1/drives/a/sessions/handles", {"operation": "capabilities"},
                  headers=sh)
    check("drive b's session is refused on drive a", s in (400, 401, 403, 404, 409), s)


def owner_restart():
    marker = os.urandom(50_000)
    s, _, _ = req("a", "PUT", fs("b", "/survives"), marker)
    check("write to drive b before its owner stops", s == 200, s)
    nodes["b"].proc.send_signal(signal.SIGTERM)
    code = nodes["b"].proc.wait(60)
    check("owner b exits 0 on SIGTERM", code == 0, code)
    t = time.time()
    s, _, d = req("a", "GET", fs("b", "/survives"), timeout=30)
    took = time.time() - t
    check("drive b through a fails fast while its owner is down", s >= 500 and took < 2,
          (s, round(took, 2), d[:80]))
    s, _, d = req("a", "GET", fs("a", "/c0"))
    check("drive a still served while b is down", s == 200, s)
    restarted = restart_node(BIN, WORK, "b")
    nodes["b"] = restarted
    deadline = time.time() + 30
    s, d = 0, b""
    while time.time() < deadline:
        s, _, d = req("a", "GET", fs("b", "/survives"))
        if s == 200:
            break
        time.sleep(0.2)
    check("drive b reachable through a again after its owner restarts",
          s == 200 and d == marker, (s, round(30 - (deadline - time.time()), 1)))


def main():
    global nodes
    nodes = start_cluster(BIN, WORK, ["a", "b"], extra_http={"max_request_bytes": CAP})
    try:
        every_route()
        round_robin()
        big_and_limits()
        concurrency()
        sessions()
        owner_restart()
    finally:
        codes = stop_cluster(nodes)
        check("both nodes exit 0 on SIGTERM", all(c == 0 for c in codes.values()), codes)
    print("FAILURES:", failures if failures else "none")
    sys.exit(1 if failures else 0)


main()
