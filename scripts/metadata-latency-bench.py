#!/usr/bin/env python3
"""Metadata latency over the HTTP API on a three-node forwarding cluster.

usage: metadata-latency-bench.py --binary mount-rs --client metadata_latency
           --work <empty dir> [options] MODE...

Starts three `serve-http` owners on loopback that share a Redis namespace
and forward to each other over mutually authenticated HTTP/3. Drive i
(of --drives, 100 by default) is owned by node i % 3; every client has its
own drive and its own keep-alive connection. Each MODE runs the load client
once:

  local[:RATE]      every client calls the node that owns its drive
  forwarded[:RATE]  every client calls the next node, which forwards

RATE is requests a second per client; 0 or none sends back to back.

Storage is TiDB metadata and RustFS blocks by default (--store tidb, which
needs MDB_TIDB_URL, MDB_S3_ENDPOINT, MDB_S3_BUCKET, MDB_S3_KEY and
MDB_S3_SECRET), or SQLite files in the work directory (--store sqlite).
MDB_REDIS_URL names the Redis for the route registry. Needs `openssl` for
the test PKI.

Prints a table per mode and writes every summary to <work>/results.json.
"""
import argparse
import hashlib
import json
import os
import re
import socket
import subprocess
import sys
import threading
import uuid

NODES = ["n0", "n1", "n2"]
OPS = ["ping", "stat", "head", "list", "mkdir", "rename", "delete"]
TOKEN = "metadata-latency-" + "y" * 24


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
       "-nodes", "-keyout", "ca.key", "-out", "ca.pem", "-days", "2", "-subj", "/CN=mount-rs bench CA",
       "-addext", "basicConstraints=critical,CA:TRUE", "-addext", "keyUsage=critical,keyCertSign",
       cwd=work)
    pins = {}
    for node in nodes:
        sh("openssl", "req", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes",
           "-keyout", f"{node}.key", "-out", f"{node}.csr", "-subj", f"/CN={node}", cwd=work)
        with open(os.path.join(work, f"{node}.ext"), "w") as f:
            f.write(f"subjectAltName=DNS:localhost,URI:urn:agent-tunnel:peer:{node}\n"
                    "keyUsage=critical,digitalSignature\n"
                    "extendedKeyUsage=clientAuth,serverAuth\n"
                    "basicConstraints=critical,CA:FALSE\n")
        sh("openssl", "x509", "-req", "-in", f"{node}.csr", "-CA", "ca.pem", "-CAkey", "ca.key",
           "-CAcreateserial", "-out", f"{node}.pem", "-days", "2", "-extfile", f"{node}.ext", cwd=work)
        der = subprocess.run(["openssl", "pkey", "-in", f"{node}.key", "-pubout", "-outform", "DER"],
                             cwd=work, check=True, capture_output=True).stdout
        pins[node] = hashlib.sha256(der).hexdigest()
    return pins


def storage(args, run, node, drive):
    if args.store == "sqlite":
        stores = {"metadata": {"kind": "sqlite", "path": f"./{node}-{drive}-meta.db"},
                  "blocks": {"kind": "sqlite", "path": f"./{node}-{drive}-blocks.db"}}
    else:
        stores = {
            "metadata": {"kind": "tidb", "connection": {"env": "MDB_TIDB_URL"},
                         "volume_key": f"mdb-{run}-{drive}", "durable": True},
            "blocks": {"kind": "rustfs", "endpoint": os.environ["MDB_S3_ENDPOINT"],
                       "bucket": os.environ["MDB_S3_BUCKET"], "region": "us-east-1",
                       "prefix": f"mdb-{run}-{drive}", "access_key_id": {"env": "MDB_S3_KEY"},
                       "secret_access_key": {"env": "MDB_S3_SECRET"}, "durable": True},
        }
    stores.update({"chunk_size_bytes": 65536, "owner": node})
    if args.ownership:
        stores["ownership_mode"] = args.ownership
    return stores


def start(args, run, owner):
    pins = make_pki(args.work, NODES)
    addresses = {n: f"127.0.0.1:{free_udp()}" for n in NODES}
    with open(os.path.join(args.work, "peers.json"), "w") as f:
        json.dump([{"node": n, "boot": f"{n}-boot", "address": addresses[n],
                    "tls_server_name": "localhost", "spki_sha256": pins[n]} for n in NODES], f)
    procs, ports = {}, {}
    for n in NODES:
        drives = [{"id": d, "token": {"env": "MDB_TOKEN"},
                   "driver": {"kind": "splitstore", "storage": storage(args, run, n, d)}}
                  for d, o in owner.items() if o == n]
        forwarding = {"peers": "peers.json", "certificate": f"{n}.pem", "private_key": f"{n}.key",
                      "ca": "ca.pem", "redis_url": {"env": "MDB_REDIS_URL"},
                      "namespace": f"mdb-{run}", "bind": "127.0.0.1:0", "owner": True,
                      "refresh_active_routes": args.refresh_active_routes}
        if args.peer_connections:
            forwarding["peer_connections"] = args.peer_connections
        http = {"host": "127.0.0.1", "port": 0, "drives": drives, "forwarding": forwarding}
        if args.store == "tidb":
            http["tidb_metadata_pool_limit"] = args.tidb_pool
        path = os.path.join(args.work, f"{n}.json")
        with open(path, "w") as f:
            json.dump({"version": 1, "http": http}, f)
        proc = subprocess.Popen([args.binary, "serve-http", "--config", path], cwd=args.work,
                                text=True, stdout=subprocess.PIPE,
                                stderr=open(os.path.join(args.work, f"{n}.stderr"), "w"),
                                env=dict(os.environ, MDB_TOKEN=TOKEN))
        procs[n] = proc
        line = proc.stdout.readline()
        match = re.search(r"127\.0\.0\.1:(\d+)", line)
        if not match:
            raise SystemExit(f"{n} did not start: {line!r}; see {n}.stderr")
        threading.Thread(target=proc.stdout.read, daemon=True).start()
        ports[n] = int(match.group(1))
    return procs, ports


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--binary", required=True)
    parser.add_argument("--client", required=True, help="the metadata_latency example binary")
    parser.add_argument("--work", required=True)
    parser.add_argument("--store", choices=["tidb", "sqlite"], default="tidb")
    parser.add_argument("--drives", type=int, default=100)
    parser.add_argument("--seconds", type=float, default=20)
    parser.add_argument("--warmup", type=float, default=3)
    parser.add_argument("--ops", default="stat,head,list,mkdir,rename,delete")
    parser.add_argument("--ownership", choices=["exclusive", "shared"])
    parser.add_argument("--refresh-active-routes", action="store_true")
    parser.add_argument("--peer-connections", type=int)
    parser.add_argument("--tidb-pool", type=int, default=32)
    parser.add_argument("--client-threads", type=int, default=4)
    parser.add_argument("--label", default="")
    parser.add_argument("modes", nargs="+")
    args = parser.parse_args()
    os.makedirs(args.work, exist_ok=True)
    run = uuid.uuid4().hex[:8]
    owner = {f"d{i:03}": NODES[i % 3] for i in range(args.drives)}
    procs, ports = start(args, run, owner)
    results = {}
    try:
        for mode in args.modes:
            route, _, rate = mode.partition(":")
            if route not in ("local", "forwarded"):
                raise SystemExit(f"unknown mode {mode}")
            clients = []
            for d, o in owner.items():
                node = o if route == "local" else NODES[(NODES.index(o) + 1) % 3]
                clients.append({"url": f"http://127.0.0.1:{ports[node]}", "drive": d, "token": TOKEN,
                                "forwarded": route != "local"})
            plan = {"seconds": args.seconds, "warmup": args.warmup, "rate": float(rate or 0),
                    "ops": args.ops.split(","), "threads": args.client_threads, "clients": clients}
            path = os.path.join(args.work, f"plan-{mode.replace(':', '-')}.json")
            with open(path, "w") as f:
                json.dump(plan, f)
            out = subprocess.run([args.client, path], capture_output=True, text=True)
            if out.returncode:
                raise SystemExit(f"load client failed for {mode}:\n{out.stderr[-3000:]}")
            if out.stderr.strip():
                print(out.stderr[-2000:], file=sys.stderr)
            report = json.loads(out.stdout)
            results[mode] = report
            print(f"\n== {args.label} {mode}  errors={report['errors']}")
            print(f"  {'op':7} {'count':>8} {'p50 ms':>8} {'p90 ms':>8} {'p99 ms':>8} {'p99.9 ms':>9} {'<1 ms':>7}")
            for op in OPS:
                s = report.get(f"{route}/{op}", {})
                if s.get("count"):
                    print(f"  {op:7} {s['count']:8} {s['p50_ms']:8.3f} {s['p90_ms']:8.3f} {s['p99_ms']:8.3f} "
                          f"{s['p999_ms']:9.3f} {s['under_1ms'] * 100:6.1f}%")
            sys.stdout.flush()
    finally:
        for proc in procs.values():
            proc.terminate()
        for proc in procs.values():
            try:
                proc.wait(30)
            except subprocess.TimeoutExpired:
                proc.kill()
        with open(os.path.join(args.work, "results.json"), "w") as f:
            json.dump(results, f, indent=1)


if __name__ == "__main__":
    main()
