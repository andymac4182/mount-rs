#!/usr/bin/env python3
"""End-to-end checks of a two-node serve-http forwarding cluster.

usage: MOUNT_RS_REDIS_URL=redis://... serve-http-cluster-e2e.py <binary> <empty work dir>

Starts two `serve-http` owners on loopback that share a disposable Redis
namespace and forward to each other over mutually authenticated HTTP/3:
node a owns drive a and node b owns drive b. A client that picks either node
for any request, as a round-robin load balancer would, must see one
consistent drive. A second cluster then has its membership file changed
while it runs, to add and remove an ingress. A third has two failover
candidates for one drive on shared SQLite storage: its owner is killed, then
stopped cleanly, and the drive must move each time with no acknowledged
write lost and never two holders of its writer lease. Needs `openssl` for
the test PKI. Prints PASS/FAIL per check and exits 1 on any FAIL.

A node can run another build: MOUNT_RS_E2E_BINARY_A, _B and _C name the
binary of the nodes called a, b and c in every cluster above, and a node
without one runs <binary>. With one node on an earlier release, the same
checks then prove that the two releases forward to each other, carry each
other's sessions and hand a drive over in a failover. Every node of the
first cluster is also shown a route record whose owner accepts only later
versions of the server-to-server protocol, which it must refuse without
sending the request.
"""
import concurrent.futures as cf
import hashlib
import http.client
import json
import os
import re
import signal
import socket
import sqlite3
import subprocess
import sys
import threading
import time
import urllib.parse
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
    def __init__(self, name, proc, port, events=None):
        self.name, self.proc, self.port = name, proc, port
        # (time, record) for each `drive_failover` line the node wrote.
        self.events = events if events is not None else []


def write_peers(work, peers):
    """Replace the membership file in one step, as an operator should."""
    path = os.path.join(work, "peers.json")
    with open(path + ".new", "w") as f:
        json.dump(peers, f)
    os.replace(path + ".new", path)


def start_cluster(binary, work, nodes, sessions=True, extra_http=None, ingress=(),
                  peer_connections=None, later=(), forwarding=None, shared=None,
                  namespace=None):
    """Return {node: Node}. Node `n` in `nodes` owns drive `n`; nodes in
    `ingress` own no drive and only forward. `peer_connections` maps a node
    to its forwarding.peer_connections. Nodes in `later` (which must also be
    in `ingress`) get a certificate and a config but are neither listed in
    the membership file nor started; every node's record is kept in
    `peers.all.json`. `forwarding` maps a node to extra forwarding keys.
    With `shared` (extra storage keys), every node in `nodes` instead lists
    one drive `shared` on the same SQLite files, under its own owner label."""
    os.makedirs(work, exist_ok=True)
    everyone = list(nodes) + list(ingress)
    pins = make_pki(work, everyone)
    addresses = {n: f"127.0.0.1:{free_udp()}" for n in everyone}
    peers = [{"node": n, "boot": f"{n}-boot", "address": addresses[n],
              "tls_server_name": "localhost", "spki_sha256": pins[n]} for n in everyone]
    with open(os.path.join(work, "peers.all.json"), "w") as f:
        json.dump(peers, f)
    write_peers(work, [p for p in peers if p["node"] not in later])
    namespace = namespace or "fwd-" + uuid.uuid4().hex[:12]
    started = {}
    try:
        for n in everyone:
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
            if shared is not None:
                os.makedirs(os.path.join(work, "shared"), exist_ok=True)
                http["drives"][0]["id"] = "shared"
                http["drives"][0]["driver"]["storage"].update(
                    metadata={"kind": "sqlite", "path": "./shared/meta.db"},
                    blocks={"kind": "sqlite", "path": "./shared/blocks.db"}, **shared)
            if n in ingress:
                http["drives"] = []
                http["remote_sessions"] = False
                http["forwarding"]["owner"] = False
            if peer_connections and n in peer_connections:
                http["forwarding"]["peer_connections"] = peer_connections[n]
            http["forwarding"].update((forwarding or {}).get(n, {}))
            http.update(extra_http or {})
            path = os.path.join(work, f"{n}.json")
            with open(path, "w") as f:
                json.dump({"version": 1, "http": http}, f)
            if n not in later:
                started[n] = launch(binary, work, n)
    except BaseException:
        stop_cluster(started)
        raise
    return started


def node_binary(n, binary):
    """The binary node `n` runs: its own from the environment, or `binary`."""
    return os.environ.get(f"MOUNT_RS_E2E_BINARY_{n.upper()}") or binary


def read_events(stream, events):
    """Keep the node's `drive_failover` and `peer_protocol_refused` records;
    drain everything else."""
    for line in stream:
        if '"drive_failover"' in line or '"peer_protocol_refused"' in line:
            try:
                events.append((time.time(), json.loads(line)))
            except ValueError:
                pass


def launch(binary, work, n, config=None):
    """Start node `n` from the config `start_cluster` wrote for it."""
    path = os.path.join(work, config or f"{n}.json")
    proc = subprocess.Popen([node_binary(n, binary), "serve-http", "--config", path], text=True,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            env=dict(os.environ, CLUSTER_TOKEN=TOKEN))
    line = proc.stdout.readline()
    m = re.search(r"127\.0\.0\.1:(\d+)", line)
    if not m:
        raise SystemExit(f"{n} did not start: {line!r} {proc.stderr.read()[-2000:]}")
    events = []
    threading.Thread(target=proc.stdout.read, daemon=True).start()
    threading.Thread(target=read_events, args=(proc.stderr, events), daemon=True).start()
    return Node(n, proc, int(m.group(1)), events)


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
UPLOAD = 32 << 20
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


def conditional():
    """Entity tags and preconditions, on the owner and through a forwarder."""
    for ingress in ("b", "a"):
        via = "local" if ingress == "b" else "forwarded"
        path = fs("b", f"/cond-{ingress}")
        s, h, _ = req(ingress, "PUT", path, b"one", headers={"If-None-Match": "*"})
        first = h.get("etag") or h.get("ETag")
        check(f"{via}: create-only PUT carries an ETag", s == 200 and bool(first), (s, first))
        s, _, _ = req(ingress, "PUT", path, b"again", headers={"If-None-Match": "*"})
        check(f"{via}: create-only PUT of an existing file is 412", s == 412, s)
        other = "a" if ingress == "b" else "b"
        s, h, _ = req(other, "GET", path)
        check(f"{via}: both ingresses report the same tag",
              s == 200 and (h.get("etag") or h.get("ETag")) == first, (s, h.get("etag"), first))
        s, h, d = req(ingress, "GET", path, headers={"If-None-Match": first})
        check(f"{via}: revalidation is 304 with no body", s == 304 and d == b"", (s, len(d)))
        s, _, _ = req(ingress, "PUT", path, b"lost", headers={"If-Match": '"stale"'})
        s2, _, d2 = req(other, "GET", path)
        check(f"{via}: a stale If-Match is 412 and changes nothing",
              s == 412 and d2 == b"one", (s, d2))
        s, h, _ = req(ingress, "PUT", path, b"two", headers={"If-Match": first})
        second = h.get("etag") or h.get("ETag")
        check(f"{via}: a current If-Match replaces", s == 200 and second and second != first,
              (s, second))
        with cf.ThreadPoolExecutor(8) as pool:
            statuses = list(pool.map(
                lambda i: req(ingress, "PUT", path, f"writer {i}".encode(),
                              headers={"If-Match": second})[0],
                range(8)))
        check(f"{via}: of 8 writes on one tag exactly one wins",
              statuses.count(200) == 1 and statuses.count(412) == 7, statuses)


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
    streamed = os.urandom(3 * CAP + 7)
    s, _, _ = req("a", "PUT", fs("b", "/streamed"), streamed, timeout=120)
    s2, _, d = req("b", "GET", fs("b", "/streamed"), timeout=120)
    s3, _, d3 = req("a", "GET", fs("b", "/streamed"), timeout=120)
    check("forwarded upload above the request cap is streamed and stored whole",
          (s, s2, s3) == (200, 200, 200) and d == streamed and d3 == streamed, (s, s2, s3, len(d)))
    s, _, d = req("a", "PUT", fs("b", "/over"), os.urandom(UPLOAD + 1), timeout=120)
    check("forwarded upload over the upload limit is 413", s == 413, (s, d[:80]))
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


def owner_connection_limit():
    """One owner admits more than the transport's default 16 peer connections."""
    work = os.path.join(WORK, "limit")
    cluster = start_cluster(BIN, work, ["a", "b"], ingress=["c"],
                            peer_connections={"b": 16, "c": 4})
    try:
        global nodes
        saved, nodes = nodes, cluster
        s, _, _ = req("a", "PUT", fs("a", "/limit"), b"limit")
        # Node b takes all 16 of its clients in turn, so 16 connections reach a.
        statuses = {req("b", "GET", fs("a", "/limit"))[0] for _ in range(48)}
        t = time.time()
        statuses |= {req("c", "GET", fs("a", "/limit"), timeout=15)[0] for _ in range(8)}
        took = time.time() - t
        check("an owner serves more than 16 peer connections", statuses == {200} and took < 5,
              (sorted(statuses), round(took, 2)))
    finally:
        nodes = saved
        stop_cluster(cluster)


def membership_reload():
    """A running cluster takes a changed membership file without a restart:
    node a by polling the file, node b on SIGHUP only."""
    work = os.path.join(WORK, "reload")
    cluster = start_cluster(BIN, work, ["a", "b"], ingress=["c"], later=["c"],
                            forwarding={"a": {"membership_reload_seconds": 1},
                                        "b": {"membership_reload_seconds": 0}})
    with open(os.path.join(work, "peers.all.json")) as f:
        everyone = json.load(f)
    owners = [p for p in everyone if p["node"] != "c"]

    def reload_b():
        cluster["b"].proc.send_signal(signal.SIGHUP)

    def through_c(want, seconds=15):
        """Statuses of both drives read through c, once both meet `want`."""
        deadline = time.time() + seconds
        while True:
            statuses = [req("c", "GET", fs(d, "/reload"), timeout=10)[0] for d in "ab"]
            if all(want(s) for s in statuses) or time.time() > deadline:
                return statuses
            time.sleep(0.2)

    try:
        global nodes
        saved, nodes = nodes, cluster
        for d in "ab":
            req(d, "PUT", fs(d, "/reload"), b"reload")
        # c is not a member yet: started now, the owners would refuse it.
        write_peers(work, everyone)
        reload_b()
        cluster["c"] = launch(BIN, work, "c")
        statuses = through_c(lambda s: s == 200)
        check("an ingress added to the membership file forwards without a restart",
              statuses == [200, 200], statuses)

        # A file that cannot be parsed changes nothing.
        with open(os.path.join(work, "peers.json"), "w") as f:
            f.write('[{"node": "a",')
        reload_b()
        time.sleep(2.5)
        statuses = [req("c", "GET", fs(d, "/reload"))[0] for d in "ab"]
        statuses += [req("a", "GET", fs("b", "/reload"))[0], req("b", "GET", fs("a", "/reload"))[0]]
        alive = all(n.proc.poll() is None for n in cluster.values())
        check("an invalid membership file leaves the membership in use",
              statuses == [200] * 4 and alive, (statuses, alive))

        # Taken out of the file, c is cut off by both owners.
        write_peers(work, owners)
        reload_b()
        statuses = through_c(lambda s: s >= 500)
        check("an ingress removed from the membership file is refused",
              all(s >= 500 for s in statuses), statuses)
        statuses = [req("a", "GET", fs("b", "/reload"))[0], req("b", "GET", fs("a", "/reload"))[0]]
        check("the remaining members still forward to each other", statuses == [200, 200],
              statuses)
    finally:
        nodes = saved
        codes = stop_cluster(cluster)
        check("the reloaded cluster exits 0 on SIGTERM", all(c == 0 for c in codes.values()),
              codes)


def redis_command(*args):
    """The reply of one command to the test Redis: a string for a bulk
    reply ("" for nil), the reply line otherwise. None if the URL needs more
    than a plain connection."""
    url = urllib.parse.urlparse(os.environ["MOUNT_RS_REDIS_URL"])
    if url.scheme != "redis" or url.username or url.password:
        return None

    def encode(*words):
        words = [w if isinstance(w, bytes) else str(w).encode() for w in words]
        return b"*%d\r\n" % len(words) + b"".join(b"$%d\r\n%s\r\n" % (len(w), w) for w in words)

    with socket.create_connection((url.hostname, url.port or 6379), timeout=10) as conn:
        database = url.path.strip("/")
        command = b""
        if database and database != "0":
            command += encode("SELECT", database)
        command += encode(*args)
        conn.sendall(command)
        reader = conn.makefile("rb")
        if database and database != "0":
            reader.readline()
        header = reader.readline()
        if header.startswith(b"$-1"):
            return ""
        if not header.startswith(b"$"):
            return header.decode().strip()
        return reader.read(int(header[1:]) + 2)[:-2].decode()


def redis_get(key):
    """GET `key` from the test Redis, "" if there is none, or None if this
    Redis cannot be read from here (the route is then checked through the
    lease only)."""
    reply = redis_command("GET", key)
    return reply if reply is None or not reply.startswith(("+", "-", ":")) else ""


def protocol_refusal(namespace):
    """A route record whose owner accepts only later protocol versions.

    The owner named is the other, running node, so a request that was sent
    would be answered by it. A node of this build must not send it: its
    client gets the cluster's 503 and its log says why. A node of an
    earlier release cannot read the record at all and answers the same."""
    with open(os.path.join(WORK, "peers.json")) as f:
        peers = {p["node"]: p for p in json.load(f)}
    for via, owner in (("a", "b"), ("b", "a")):
        drive = f"future-{via}"
        peer = peers[owner]
        record = json.dumps({
            "volume": drive, "node": owner, "boot": peer["boot"],
            "registration": "from-a-later-release", "fence": 1, "address": peer["address"],
            "tls_server_name": peer["tls_server_name"],
            "protocols": {"min": 2, "max": 3}}, separators=(",", ":"))
        reply = redis_command("SET", f"mount-rs:{namespace}:route:{drive}",
                              f"{1:020}|{record}", "PX", 60000)
        if reply is None:
            print("INFO protocol refusal not checked: this Redis cannot be written from here",
                  flush=True)
            return
        current = node_binary(via, BIN) == BIN
        tag = f"a later owner's record through {via} ({'this build' if current else 'earlier release'})"
        t = time.time()
        s, h, d = req(via, "PUT", fs(drive, "/x"), b"never sent", timeout=15)
        check(f"{tag}: refused with 503, not-dispatched and Retry-After",
              reply == "+OK" and s == 503 and h.get("x-mount-dispatch") == "not-dispatched"
              and h.get("retry-after") == "1" and time.time() - t < 5,
              (reply, s, h.get("x-mount-dispatch"), d[:80]))
        if current:
            deadline = time.time() + 5
            logged = []
            while not logged and time.time() < deadline:
                logged = [e for _, e in list(nodes[via].events)
                          if e.get("event") == "peer_protocol_refused" and e.get("drive") == drive]
                time.sleep(0.05)
            check(f"{tag}: the node logs which owner and which versions",
                  [(e.get("role"), e.get("peer"), e.get("accepts"), e.get("speaks"))
                   for e in logged[:1]] == [("forwarder", owner, "2-3", "1-1")], logged[:2])
        s, _, _ = req(via, "GET", fs(owner, "/rr"))
        check(f"{tag}: the node still forwards to that owner's own drive", s == 200, s)


def failover():
    """Two candidates for one drive on shared SQLite storage, and an ingress.

    The documented bounds (docs/serve-http-failover.md), with the 15 s route
    TTL, this drive's 20 s lease, a 1 s poll and a 2 s stagger:
      after a crash:      max(15 + k x 2, 20) + 5 + open  = 25 s + open
      after a clean stop: 1 + k x 2 + 5 + open            =  8 s + open
    """
    lease_seconds, crash_bound, clean_bound, slack = 20, 25, 8, 10
    work = os.path.join(WORK, "failover")
    namespace = "fwd-" + uuid.uuid4().hex[:12]
    candidate = {"failover": {}}
    cluster = start_cluster(BIN, work, ["a", "b"], ingress=["c"], namespace=namespace,
                            shared={"lease_ttl_ms": lease_seconds * 1000},
                            forwarding={"a": candidate, "b": candidate})
    drive = "shared"

    def states(node, state):
        return [(t, e) for t, e in list(cluster[node].events) if e.get("state") == state]

    def lease():
        """(owner label, fence) of the drive's writer lease row."""
        with sqlite3.connect(os.path.join(work, "shared", "meta.db"), timeout=30) as db:
            return db.execute("SELECT owner, fence FROM mount_rs_metadata WHERE id=1").fetchone()

    def route():
        """(node, fence) of the published route, None if none is published,
        or False if this Redis cannot be read from here."""
        record = redis_get(f"mount-rs:{namespace}:route:{drive}")
        if record is None:
            return False
        if not record:
            return None
        published = json.loads(record[21:])
        return published["node"], published["fence"]

    def wait_owner(among, since, seconds):
        """The node in `among` that published the drive after `since`."""
        deadline = time.time() + seconds
        while time.time() < deadline:
            for n in among:
                if any(t >= since for t, _ in states(n, "published")):
                    return n
            time.sleep(0.05)
        return None

    def retried(via, method, path, body=None, seconds=90):
        """The request, repeated while it gets a 5xx or no answer. An
        ingress keeps one pooled connection per `peer_connections` to each
        owner; after an owner process is replaced, a request on a stale one
        gets a 502 once the ingress has noticed the old process is gone, and
        until then waits for up to the 30 s operation timeout. Give up on
        each attempt sooner, as a client would. Only used for requests that
        are safe to repeat."""
        deadline = time.time() + seconds
        while True:
            try:
                s, h, d = req(via, method, path, body, timeout=5)
            except TimeoutError:
                s, h, d = 504, {}, b"no answer within the client's 5 s"
            if s < 500 or time.time() > deadline:
                return s, h, d
            time.sleep(0.2)

    def consistent(tag, owner, fence):
        """The lease, the route and the owner's own report agree."""
        held = lease()
        check(f"{tag}: the writer lease is held by {owner} under fence {fence}",
              held == (owner, fence), held)
        published = route()
        if published is not False:
            check(f"{tag}: the published route names {owner} with the lease's fence",
                  published == (owner, fence), published)
        acquired = [e["fence"] for _, e in states(owner, "acquired")]
        check(f"{tag}: {owner} reports acquiring under that fence", acquired[-1:] == [fence],
              acquired)

    try:
        global nodes
        saved, nodes = nodes, cluster
        started = time.time() - 60
        first = wait_owner("ab", started, 30)
        check("one candidate published the shared drive at startup", first in ("a", "b"), first)
        if first is None:
            return
        standby = "b" if first == "a" else "a"
        fence = lease()[1]
        consistent("startup", first, fence)
        check("the standby did not open the drive",
              not states(standby, "acquired") and not states(standby, "open_failed"),
              cluster[standby].events)

        payload = os.urandom(200_000)
        s, _, _ = req("c", "PUT", fs(drive, "/kept.bin"), payload)
        check("write through the ingress", s == 200, s)
        got = [req(n, "GET", fs(drive, "/kept.bin")) for n in "abc"]
        check("read back through the owner, the standby and the ingress",
              all(s == 200 and d == payload for s, _, d in got), [s for s, _, _ in got])
        s, _, d = req("c", "POST", f"/v1/drives/{drive}/sessions")
        check("session created on the owner through the ingress", s == 201, (s, d[:80]))
        session = {"x-mount-session": json.loads(d)["session"]} if s == 201 else {}

        # Writers that keep going through the failover, on the two nodes
        # that stay up. A write that was answered 200 must never be lost.
        acked, seen, stop = {}, [], threading.Event()

        def writer(via):
            i = 0
            while not stop.is_set():
                name, body = f"/w-{via}-{i}", f"{via} {i} {uuid.uuid4().hex}".encode()
                try:
                    # While the dead owner's route is still published, a
                    # forwarder waits on it for up to its 30 s operation
                    # timeout. Give up sooner, as a client would; the
                    # outcome of such a write is unknown, like a 502.
                    s, h, _ = req(via, "PUT", fs(drive, name), body, timeout=5)
                except TimeoutError:
                    s, h = "timeout", {}
                except OSError as error:
                    s, h = repr(error), {}
                seen.append((time.time(), via, s, h))
                if s == 200:
                    acked[name] = body
                i += 1
                time.sleep(0.05)

        writers = [threading.Thread(target=writer, args=(via,), daemon=True)
                   for via in (standby, "c")]
        for thread in writers:
            thread.start()
        time.sleep(1)

        # A crash: SIGKILL withdraws no route and releases no lease.
        killed = time.time()
        cluster[first].proc.kill()
        cluster[first].proc.wait(30)
        took_over = wait_owner(standby, killed, crash_bound + slack + 30)
        took = time.time() - killed
        check("the standby takes over within the documented bound after a kill -9",
              took_over == standby and took <= crash_bound + slack,
              (took_over, round(took, 1), f"bound {crash_bound} s + open"))
        print(f"INFO crash failover took {took:.1f} s (lease {lease_seconds} s, route TTL 15 s)",
              flush=True)
        check("it found the dead owner's lease still held and waited for it",
              bool(states(standby, "lease_held")), cluster[standby].events)
        check("it did not take over before the dead owner's lease could expire",
              took >= lease_seconds - 6, round(took, 1))
        consistent("after the crash", standby, fence + 1)

        s, _, d = retried("c", "GET", fs(drive, "/kept.bin"))
        check("the file written before the crash is readable through the ingress",
              s == 200 and d == payload, (s, len(d)))
        s, _, _ = retried("c", "PUT", fs(drive, "/after-crash"), b"after the crash")
        s2, _, d = req(standby, "GET", fs(drive, "/after-crash"))
        check("a new write succeeds after the takeover", s == 200 and d == b"after the crash",
              (s, s2, d))
        time.sleep(1)
        stop.set()
        for thread in writers:
            thread.join(90)
        statuses = sorted({str(s) for _, _, s, _ in seen})
        check("writes during the outage were answered 200, 502 or 503, or timed out",
              set(statuses) <= {"200", "502", "503", "timeout"}, statuses)
        refused = [h for _, _, s, h in seen if s == 503]
        retryable = [h for h in refused if h.get("x-mount-dispatch") == "not-dispatched"
                     and h.get("retry-after") == "1"]
        check("503s while no owner was published say not-dispatched and Retry-After: 1",
              bool(refused) and len(retryable) == len(refused), (len(refused), len(retryable)))
        before = sum(1 for t, _, s, _ in seen if s == 200 and t < killed)
        after = sum(1 for t, _, s, _ in seen if s == 200 and t > killed + took)
        check("writers were served before the crash and again after the takeover",
              before > 0 and after > 0, (before, after))
        lost = [name for name, body in acked.items()
                if retried("c", "GET", fs(drive, name))[2] != body]
        check(f"all {len(acked)} writes acknowledged across the failover are intact", not lost,
              lost[:5])
        if session:
            s, _, d = req("c", "POST", f"/v1/drives/{drive}/sessions/handles",
                          {"operation": "capabilities"}, headers=session)
            check("a session from the dead owner is refused by the new one, not served",
                  400 <= s < 500, (s, d[:120]))
            print(f"INFO a lost session is answered {s} {d[:120]!r}", flush=True)

        # The killed node comes back as a standby and forwards to the owner.
        cluster[first] = launch(BIN, work, first)
        time.sleep(4)
        check("the killed node returns as a standby and does not take the drive back",
              not states(first, "acquired") and lease() == (standby, fence + 1),
              (cluster[first].events, lease()))
        s, _, d = retried(first, "GET", fs(drive, "/kept.bin"))
        s2, _, _ = retried(first, "PUT", fs(drive, "/through-standby"), b"forwarded")
        s3, _, d3 = retried("c", "GET", fs(drive, "/through-standby"))
        check("requests to the returned node are forwarded to the owner",
              (s, s2, s3) == (200, 200, 200) and d == payload and d3 == b"forwarded",
              (s, s2, s3))

        # A clean stop hands the drive over without waiting for the lease.
        stopped = time.time()
        cluster[standby].proc.send_signal(signal.SIGTERM)
        code = cluster[standby].proc.wait(60)
        released = time.time()
        handed = wait_owner(first, stopped, clean_bound + slack + 30)
        took = time.time() - released
        check("the owner exits 0 on SIGTERM and reports releasing the drive",
              code == 0 and [e.get("reason") for _, e in states(standby, "released")] == ["shutdown"],
              (code, cluster[standby].events[-3:]))
        check("a clean stop hands the drive over within the documented bound",
              handed == first and took <= clean_bound + slack,
              (handed, round(took, 1), f"bound {clean_bound} s + open"))
        print(f"INFO clean handover took {took:.1f} s after the owner exited", flush=True)
        consistent("after the clean stop", first, fence + 2)
        s, _, d = retried("c", "GET", fs(drive, "/after-crash"))
        s2, _, _ = retried("c", "PUT", fs(drive, "/after-handover"), b"handed over")
        s3, _, d3 = retried(first, "GET", fs(drive, "/after-handover"))
        check("the drive is readable and writable after the handover",
              (s, s2, s3) == (200, 200, 200) and d == b"after the crash" and d3 == b"handed over",
              (s, s2, s3))

        # Every acquisition was under its own fence, in order, and each
        # began only after the previous holder had gone: never two at once.
        # (The killed process's own records went with it; the lease kept count.)
        history = sorted((t, n, e["fence"]) for n in "ab" for t, e in states(n, "acquired"))
        history = [(n, f) for _, n, f in history]
        check("the drive was acquired once per failover, each under the next fence",
              history == [(standby, fence + 1), (first, fence + 2)], history)

        # Without `failover`, a second owner of the same drive is refused by
        # the lease at startup, as it always was.
        with open(os.path.join(work, f"{standby}.json")) as f:
            static = json.load(f)
        del static["http"]["forwarding"]["failover"]
        with open(os.path.join(work, "static.json"), "w") as f:
            json.dump(static, f)
        refused = subprocess.run([node_binary(standby, BIN), "serve-http", "--config",
                                  os.path.join(work, "static.json")],
                                 text=True, capture_output=True, timeout=60,
                                 env=dict(os.environ, CLUSTER_TOKEN=TOKEN))
        check("an owner without failover still fails at startup while the lease is held",
              refused.returncode == 1 and "acquire writer" in refused.stderr,
              (refused.returncode, refused.stderr[-200:]))
        check("and that attempt changed nothing", lease() == (first, fence + 2), lease())
    finally:
        nodes = saved
        codes = stop_cluster(cluster)
        check("the failover cluster exits 0 on SIGTERM",
              all(c == 0 for n, c in codes.items()), codes)


def main():
    global nodes
    for n in "abc":
        if node_binary(n, BIN) != BIN:
            print(f"INFO node {n} runs {node_binary(n, BIN)}; the others run {BIN}", flush=True)
    namespace = "fwd-" + uuid.uuid4().hex[:12]
    nodes = start_cluster(BIN, WORK, ["a", "b"], namespace=namespace,
                          extra_http={"max_request_bytes": CAP, "max_upload_bytes": UPLOAD})
    try:
        every_route()
        conditional()
        round_robin()
        big_and_limits()
        concurrency()
        sessions()
        protocol_refusal(namespace)
        owner_restart()
        owner_connection_limit()
        membership_reload()
        failover()
    finally:
        codes = stop_cluster(nodes)
        check("both nodes exit 0 on SIGTERM", all(c == 0 for c in codes.values()), codes)
    print("FAILURES:", failures if failures else "none")
    sys.exit(1 if failures else 0)


main()
