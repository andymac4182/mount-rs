#!/usr/bin/env python3
"""Offline verification only: no runtime, subprocess, network or artifact writes.

Root reviews and executes this draft. Usage: python3 -I -S -B THIS SPEC SPEC_SHA256
The fixed extractor is loaded only after its complete source hash qualifies.
Negative controls alter private in-memory copies, never the evidence files.
"""
import copy
import hashlib
import json
import math
import os
import re
import stat
import sys
import types
from fractions import Fraction
from pathlib import Path

EXTRACTOR_SHA = "27ef362db1c1778eb18cc51cd373258551276e42fad277cfe854c5f9ed41bf02"
MODES = ("mostly_idle", "all_active")
PATTERNS = ("sequential_read", "random_read", "sequential_overwrite", "random_overwrite",
            "mixed", "hot_file", "append_truncate", "churn")
CELLS = {(m, p) for m in MODES for p in PATTERNS}
ALLOC = ("rust_allocations", "rust_deallocations", "rust_reallocations",
         "rust_allocated_bytes", "rust_freed_bytes")
SQL = {"tidb.sql." + x for x in ("session", "ddl", "metadata_read", "metadata_write",
       "inode_read", "inode_write", "block_read", "block_write", "flush_probe")}
QUIC = {"udp_rx_bytes", "udp_tx_bytes", "udp_rx_datagrams", "udp_tx_datagrams",
        "udp_rx_ios", "udp_tx_ios", "sent_packets", "lost_bytes", "lost_packets",
        "congestion_events", "sent_plpmtud_probes", "lost_plpmtud_probes", "black_holes_detected"}
COUNTERS = ("calls", "success", "error", "cancelled", "elapsed_ns", "bytes",
            "returned_rows", "returned_row_observations")
GUARDS = {"host_free_floor": 68719476736, "rss_cap_per_owned_process": 25769803776,
          "phase_seconds": 600, "population_seconds": 600, "request_seconds": 30,
          "setup_seconds": 600, "work_seconds": 1800, "child_cleanup_seconds": 95,
          "client_cleanup_seconds": 30, "expected_receipt_seconds": 30, "oracle_cleanup_seconds": 30}
LIMIT = 32 * 1024**2


class Rejected(Exception):
    pass


def need(ok, message):
    if not ok:
        raise Rejected(message)


def uint(value):
    need(type(value) is int and 0 <= value <= 2**64 - 1, "invalid counter")
    return value


def sha(value):
    need(isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value), "invalid digest")
    return value


def read(path, expected):
    need(isinstance(path, str) and Path(path).is_absolute(), "explicit absolute path required")
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
    try:
        a = os.fstat(fd)
        need(stat.S_ISREG(a.st_mode) and a.st_uid == os.geteuid() and a.st_nlink == 1
             and 0 <= a.st_size <= LIMIT, "file admission failed")
        chunks, count = [], 0
        while True:
            block = os.read(fd, min(1024**2, LIMIT + 1 - count))
            if not block:
                break
            chunks.append(block)
            count += len(block)
            need(count <= LIMIT, "bounded read exceeded")
        raw = b"".join(chunks)
        b, c = os.fstat(fd), os.stat(path, follow_symlinks=False)
        keys = ("st_dev", "st_ino", "st_uid", "st_mode", "st_nlink", "st_size", "st_mtime_ns", "st_ctime_ns")
        need(all(getattr(a, k) == getattr(b, k) == getattr(c, k) for k in keys)
             and len(raw) == a.st_size and hashlib.sha256(raw).hexdigest() == sha(expected),
             "file identity or hash changed")
        return raw
    finally:
        os.close(fd)


def unique(pairs):
    result = {}
    for key, value in pairs:
        need(key not in result, "duplicate JSON key")
        result[key] = value
    return result


def constant(_):
    raise Rejected("nonfinite JSON constant")


def load(ref):
    need(set(ref) == {"path", "sha256"}, "closed artifact reference required")
    return json.loads(read(ref["path"], ref["sha256"]), object_pairs_hook=unique, parse_constant=constant)


def rational(value, expected=None):
    need(isinstance(value, dict) and set(value) == {"numerator", "denominator", "value"},
         "rational inventory changed")
    n, d = uint(value["numerator"]), uint(value["denominator"])
    need(d > 0 and math.gcd(n, d) == 1, "rational is not canonical")
    result = Fraction(n, d)
    f = value["value"]
    need(type(f) in (int, float) and math.isfinite(f)
         and math.isclose(f, float(result), rel_tol=1e-12, abs_tol=1e-12), "rational display differs")
    if expected is not None:
        need(result == expected, "independent rational arithmetic differs")
    return result


def indexed(rows, key):
    need(isinstance(rows, list), "row inventory is not an array")
    result = {key(row): row for row in rows}
    need(len(result) == len(rows), "duplicate row")
    return result


def allocations(value):
    need(value["available"] is True and value["status"] == "measured"
         and set(value["counters"]) == set(ALLOC), "allocation coverage differs")
    return {key: uint(value["counters"][key]) for key in ALLOC}


def observation(obs, filesystem):
    if filesystem:
        need(obs["available"] is False and obs["configured"] is False
             and obs["status"] == "not_configured" and obs["http"] is None
             and obs["generic_adapter_cache"] is None, "filesystem HTTP/cache fabricated")
    else:
        need(isinstance(obs["http"], list) and obs["http"]
             and isinstance(obs["generic_adapter_cache"], dict), "RustFS HTTP/cache coverage missing")
        for row in obs["http"]:
            for value in row["counters"].values():
                uint(value)


def process(value):
    cpu = uint(value["cpu_user_us"]) + uint(value["cpu_system_us"])
    elapsed = rational(value["observation_elapsed_seconds"])
    need(elapsed > 0, "process observation duration absent")
    rational(value["percent_of_one_core"], Fraction(cpu, 10000) / elapsed)
    for endpoint in value["rss_endpoints_bytes"].values():
        if endpoint is not None:
            uint(endpoint)
    if value["lifetime_peak_rss_bytes"] is not None:
        uint(value["lifetime_peak_rss_bytes"])
    # Original RustFS extractor has no public OS-byte field; preserve this coverage difference.
    if "os_accounting" in value:
        os_io = value["os_accounting"]
        if os_io["available"]:
            need(os_io["status"] == "measured" and os_io["source"] in
                 ("darwin_proc_pid_rusage_v2", "linux_proc_self_io"), "OS accounting source differs")
            for key in ("interval_ns",):
                need(re.fullmatch(r"[1-9][0-9]{0,19}", os_io[key]), "OS interval is not exact")
                uint(int(os_io[key]))
            for key in ("read_bytes", "write_bytes"):
                need(re.fullmatch(r"0|[1-9][0-9]{0,19}", os_io["counters"][key]), "OS counter is not exact")
                uint(int(os_io["counters"][key]))
        else:
            need(os_io["status"] in ("disabled", "unsupported", "unavailable")
                 and os_io["counters"] is None, "OS accounting missingness fabricated")
    return cpu


def verify_run(run, tables, filesystem):
    label = run["label"]
    archive = run["archive_bindings"]
    source_files = indexed(archive["source_files"], lambda x: x["path"])
    need(len(source_files) == archive["source_count"] == run["archive_source_count"]
         and sha(archive["manifest_sha256"]) == sha(run["archive_manifest_sha256"])
         and sha(archive["binary_sha256"]) == sha(run["binary_sha256"]), "archive binding inventory differs")
    for source_file in source_files.values():
        sha(source_file["sha256"])
    need(run["geometry"] == {"servers": 10, "clients": 10, "drives": 10, "partitions": 5,
         "files_per_drive": 1000, "seconds_per_cell": 5, "configured_partitions_per_server": 5,
         "construction_mode": "lazy", "metadata_provider": "tidb",
         "block_provider": "filesystem" if filesystem else "rustfs"}, "D10 geometry differs")
    need(run["guard_limits"] == GUARDS and run["coverage"]["metrics_complete"] is True
         and run["coverage"]["coverage_complete"] is False and run["coverage"]["allocation_profiling"] is True,
         "guard or metrics qualification differs")
    resources = run["resources"]
    need(uint(resources["minimum_observed_host_free_bytes"]) >= GUARDS["host_free_floor"]
         and uint(resources["controller_peak_rss_bytes"]) <= GUARDS["rss_cap_per_owned_process"]
         and len(resources["worker_peak_rss_bytes"]) == 10
         and all(uint(v) <= GUARDS["rss_cap_per_owned_process"] for v in resources["worker_peak_rss_bytes"]), "resource guard observation differs")
    good = run["correctness"]
    need(all(good[k] is True for k in ("owner_benchmarked", "workload_complete", "fresh_oracle_complete", "workers_clean_reaped"))
         and good["verified_passes"] == 2 and good["crossnode_completed_pairs"] == 100
         and good["crossnode_verified_bytes"] == 409600 and good["revocation_denials"] == 10
         and good["scope_denials"] == {"partition": 10, "sibling": 10}, "correctness witness differs")
    requests = good["cumulative_requests"]
    need(requests["failed"] == requests["uncertain"] == 0
         and uint(requests["attempts"]) == uint(requests["acknowledged"]), "uncertain or failed request")
    bindings = indexed(run["oracle_and_expected_file_bindings"], lambda x: x["file"])
    need(set(bindings) == {"oracle-receipts/initial.json", "oracle-receipts/final.json"}
         | {"expected/drive-" + str(d) + ".json" for d in range(10)}, "full oracle/Drive binding inventory differs")
    for phase in ("initial", "final"):
        witness = bindings["oracle-receipts/" + phase + ".json"]
        corpus = good["fresh_corpus"]
        need(witness["pass"] == phase and witness["files"] == corpus[phase + "_files"] == 10000
             and uint(witness["bytes"]) == uint(corpus[phase + "_bytes"])
             and sha(witness["sha256"]) == sha(corpus[phase + "_receipt_sha256"]), "full corpus witness differs")
    for drive in range(10):
        witness = bindings["expected/drive-" + str(drive) + ".json"]
        need(witness["drive"] == drive, "Drive ledger identity differs")
        sha(witness["sha256"])
    cells = indexed(run["cells"], lambda c: (c["mode"], c["pattern"]))
    rows = indexed([t for t in tables if t["run"] == label], lambda t: (t["mode"], t["pattern"]))
    need(set(cells) == set(rows) == CELLS and len(cells) == 16, "sixteen-cell inventory differs")
    frames = indexed(run["all_metric_frame_bindings"], lambda f: f["file"])
    frame_ids = indexed(list(frames.values()), lambda f: (f["sequence"], f["role"], f["server"], f["phase"], f["boundary"]))
    active = indexed(run["active_boundary_sha256"], lambda f: (f["sequence"], f["role"], f["server"]))
    expected_active = set()
    for key, c in cells.items():
        t = rows[key]
        cycles, acks = uint(c["cycles"]), uint(c["acknowledged_requests"])
        need(cycles > 0 and acks == cycles * (4 if c["pattern"] == "churn" else 3), "cycle/RPC count differs")
        clients = 1 if c["mode"] == "mostly_idle" else 10
        need(c["active_clients"] == t["active_clients"] == clients
             and c["idle_liveness_acknowledgments"] == 10 - clients
             and t["complete_cycles"] == cycles and t["acknowledged_requests"] == acks
             and t["source_revision"] == run["source_revision"], "cell geometry/table differs")
        seconds = rational(c["exact_active_elapsed_seconds"], Fraction(str(c["active_elapsed_seconds"])))
        need(seconds > 0 and len(c["rpc_latency_histogram_log2_us"]) == 32
             and sum(map(uint, c["rpc_latency_histogram_log2_us"])) == acks
             and math.isclose(c["cycles_per_second"], float(Fraction(cycles) / seconds), rel_tol=1e-12), "time/histogram differs")
        for name, count in (("complete_cycles_per_second", cycles), ("acknowledged_requests_per_second", acks)):
            rational(c[name], Fraction(count) / seconds)
            need(t[name] == c[name], "throughput table differs")
        payload = c["logical_payload"]
        need(payload["payload_request_bytes"] == 4096, "logical payload size differs")
        reads, writes = uint(payload["read_bytes"]), uint(payload["write_bytes"])
        pattern = c["pattern"]
        if pattern in ("sequential_read", "random_read"):
            need(reads == cycles * 4096 and writes == 0, "read cycle payload differs")
        elif pattern in ("sequential_overwrite", "random_overwrite"):
            need(writes == cycles * 4096 and reads == 0, "overwrite cycle payload differs")
        elif pattern == "append_truncate":
            need(reads == 0 and writes <= cycles * 4096, "append cycle payload differs")
        elif pattern == "churn":
            need(reads == writes == 0, "churn payload differs")
        else:
            need(reads + writes == cycles * 4096, "mixed cycle payload differs")
        for role in ("read", "write"):
            count = uint(payload[role + "_bytes"])
            need(count % 4096 == 0 and t[role + "_bytes"] == count, "logical payload table differs")
            rational(payload[role + "_bytes_per_second"], Fraction(count) / seconds)
            need(t[role + "_bytes_per_second"] == payload[role + "_bytes_per_second"], "payload rate table differs")
        sequences = c["metric_sequences"]
        need(len(sequences) == 3 and sequences == list(range(sequences[0], sequences[0] + 3)), "cell sequences differ")
        for seq, boundary in zip(sequences, ("before_active", "after_active", "after_idle")):
            for role, server in [("controller", None)] + [("worker", n) for n in range(10)]:
                identity = (seq, role, server)
                f = frame_ids[identity + ("/".join(key), boundary)]
                need(f["phase"] == "/".join(key) and f["boundary"] == boundary, "metric-frame geometry differs")
                if boundary != "after_idle":
                    expected_active.add(identity)
                    need(sha(active[identity]["sha256"]) == sha(f["sha256"]), "active/full frame binding differs")
        workers, controller = c["workers"], c["controller"]
        ps = indexed(workers["per_process"], lambda p: p["server"])
        need(set(ps) == set(range(10)) and workers["process_server_order"] == list(range(10)), "worker metric roster differs")
        sql = indexed(workers["sql"], lambda x: x["name"])
        need(set(sql) == SQL, "SQL family inventory differs")
        sql_calls, sql_rows = 0, 0
        for name, row in sql.items():
            for counter in COUNTERS:
                expected = sum(uint(next(x[counter] for x in p["storage"] if x["name"] == name)) for p in ps.values())
                need(uint(row[counter]) == expected, "SQL worker/cluster aggregate differs")
            need(row["calls"] == row["success"] + row["error"] + row["cancelled"], "SQL outcomes differ")
            sql_calls += row["calls"]
            sql_rows += row["returned_rows"]
        rational(workers["exact_sql_calls_per_cycle"], Fraction(sql_calls, cycles))
        rational(workers["exact_sql_rows_per_cycle"], Fraction(sql_rows, cycles))
        need(t["worker_sql_calls"] == sql_calls and t["worker_sql_returned_rows"] == sql_rows
             and t["worker_sql_calls_per_cycle"] == workers["exact_sql_calls_per_cycle"]
             and t["worker_sql_rows_per_cycle"] == workers["exact_sql_rows_per_cycle"], "SQL table arithmetic differs")
        for role, group in (("worker", workers), ("controller", controller)):
            values = allocations(group["allocations"])
            need(t[role + "_allocations"] == group["allocations"], "allocation table differs")
            individuals = [p["allocations"] for p in ps.values()] if role == "worker" else [controller["process_detail"]["allocations"]]
            need(values == {k: sum(allocations(p)[k] for p in individuals) for k in ALLOC}, "allocation aggregate differs")
            need(set(t[role + "_allocation_counters_per_cycle"]) == set(ALLOC), "allocation quotient inventory differs")
            for k in ALLOC:
                rational(t[role + "_allocation_counters_per_cycle"][k], Fraction(values[k], cycles))
            rational(t[role + "_allocation_requests_per_cycle"], Fraction(values["rust_allocations"] + values["rust_reallocations"], cycles))
        worker_cpu = sum(process(p["process"]) for p in ps.values())
        controller_cpu = process(controller["process_detail"])
        need(worker_cpu == t["worker_cpu_us"] == workers["process"]["cpu_user_us"] + workers["process"]["cpu_system_us"]
             and controller_cpu == t["controller_cpu_us"] == controller["process"]["cpu_user_us"] + controller["process"]["cpu_system_us"], "CPU aggregate differs")
        need(set(workers["quic"]) == QUIC and all(set(p["quic"]) == QUIC for p in ps.values()), "QUIC inventory differs")
        for name, value in workers["quic"].items():
            need(uint(value) == sum(uint(p["quic"][name]) for p in ps.values()), "QUIC aggregate differs")
        need(t["worker_quic_udp"] == workers["quic"] and t["distributed_blob_cache_configured"] is False, "QUIC/cache table differs")
        for p in ps.values():
            observation(p["object_store_observation"], filesystem)
        observation(controller["object_store_observation"], filesystem)
        if filesystem:
            need(t["object_store_http_status"] == "not_configured"
                 and all(t[k] is None for k in ("observed_worker_http_dispatches", "worker_http_by_method", "generic_adapter_cache_cluster_endpoints")), "filesystem table fabricated counters")
        else:
            attempts = sum(uint(h["counters"]["attempts_started"]) for p in ps.values() for h in p["object_store_observation"]["http"])
            need(t["observed_worker_http_dispatches"] == attempts
                 and sum(uint(h["attempts_started"]) for h in t["worker_http_by_method"]) == attempts
                 and t["generic_adapter_cache_cluster_endpoints"] is not None, "RustFS HTTP aggregate differs")
    need(set(active) == expected_active and len(active) == 352, "active frame inventory differs")
    cgroup = run["rustfs_guest_cgroup"]
    if filesystem:
        need(cgroup["available"] is False and cgroup["configured"] is False
             and cgroup["status"] == "not_configured" and cgroup["counters"] is None,
             "filesystem cgroup counters fabricated")
        q = run["filesystem_qualification"]
        need(q["performance_eligible"] is True and q["status"] == "qualified"
             and q["data_retained"] is True and q["root_identity_verified"] is True,
             "filesystem qualification absent")
    else:
        need(uint(cgroup["interval_ns"]) > 0 and isinstance(cgroup["devices"], list) and cgroup["devices"], "RustFS cgroup coverage absent")
        sha(cgroup["before_sha256"]); sha(cgroup["after_sha256"])
        devices = indexed(cgroup["devices"], lambda x: x["device"])
        for row in devices.values():
            need(set(row["delta"]) == {"rbytes", "wbytes", "rios", "wios", "dbytes", "dios"}, "cgroup counter inventory differs")
            for value in row["delta"].values():
                uint(value)
    return cells


def controls(module, spec, fs_run, rustfs, rustfs_ref, label):
    private = load(spec["filesystem_input"])
    item = private["run"]
    need(private["schema"] == "mount-rs-native-filesystem-offline-input-v1"
         and item["label"] == fs_run["label"] and item["pins"]["terminal"] == fs_run["terminal_sha256"], "control input binding differs")
    root = Path(item["owner_root"]) / "combined-target"
    terminal, _ = module.read_json(root / "terminal.json", fs_run["terminal_sha256"])
    seqs = terminal["stages"][0]["metric_sequences"][:2]
    rows = {r["sequence"]: r for r in terminal["phase_metrics"]["boundaries"] if "sequence" in r}
    pair = [module.read_json(root / rows[s]["controller"]["file"], rows[s]["controller"]["sha256"])[0] for s in seqs]
    module.checked_delta(*pair)  # Positive control uses the actual fully pinned pair.
    module.verify_oracles(root, terminal, 10)
    module.matched_rustfs_summary(dict(rustfs_ref, label=label), fs_run)
    passed = []
    def rejects(name, operation):
        try:
            operation()
        except module.ExtractionError:
            passed.append(name)
        else:
            raise Rejected("negative control accepted: " + name)
    changed = copy.deepcopy(pair[1])
    changed["delta_from_previous"]["counters"]["allocations"]["counters"]["rust_allocations"] += 1
    rejects("allocation_delta_tamper", lambda: module.checked_delta(pair[0], changed))
    oracle_path = root / "oracle-receipts/initial.json"
    witness, witness_sha = module.read_json(oracle_path, terminal["fresh_oracle_passes"][0]["receipt"]["sha256"])
    bad_witness = copy.deepcopy(witness)
    bad_witness["completed_files"] -= 1
    original_read = module.read_json
    try:
        module.read_json = lambda path, pin=None: (bad_witness, witness_sha) if Path(path) == oracle_path else original_read(path, pin)
        rejects("incomplete_full_oracle", lambda: module.verify_oracles(root, copy.deepcopy(terminal), 10))
    finally:
        module.read_json = original_read
    bad_pair = copy.deepcopy(rustfs)
    candidate = next(r for r in bad_pair["runs"] if r["label"] == label)
    candidate["source_revision"] = "0" * 40 if candidate["source_revision"] != "0" * 40 else "1" * 40
    try:
        module.read_json = lambda _path, _pin=None: (bad_pair, rustfs_ref["sha256"])
        rejects("mismatched_source_pair", lambda: module.matched_rustfs_summary(dict(rustfs_ref, label=label), fs_run))
    finally:
        module.read_json = original_read
    need(len(passed) == 3, "negative controls incomplete")
    return passed


def main():
    need(len(sys.argv) == 3, "usage: verifier SPEC SPEC_SHA256")
    spec = load({"path": sys.argv[1], "sha256": sys.argv[2]})
    need(set(spec) == {"schema", "filesystem_public", "rustfs_public", "filesystem_input", "filesystem_extractor", "rustfs_label", "verifier_sha256"}
         and spec["schema"] == "mount-rs-native-final-pair-verification-input-v1", "closed verification input required")
    verifier_sha = hashlib.sha256(read(str(Path(__file__).absolute()), spec["verifier_sha256"])).hexdigest()
    need(spec["filesystem_extractor"]["sha256"] == EXTRACTOR_SHA, "extractor source is not the reviewed pin")
    source = read(spec["filesystem_extractor"]["path"], EXTRACTOR_SHA)
    module = types.ModuleType("pinned_filesystem_extractor")
    module.__file__ = spec["filesystem_extractor"]["path"]
    exec(compile(source, module.__file__, "exec"), module.__dict__)
    public, rustfs = load(spec["filesystem_public"]), load(spec["rustfs_public"])
    need(public["schema"] == "mount-rs-native-filesystem-public-observations-v1"
         and public["extractor_sha256"] == EXTRACTOR_SHA and len(public["runs"]) == 1
         and public["input_spec_sha256"] == spec["filesystem_input"]["sha256"]
         and rustfs["schema"] == "mount-rs-native-public-observations-v2", "public schema/source differs")
    fs_run = public["runs"][0]
    label = spec["rustfs_label"]
    need(isinstance(label, str) and re.fullmatch(r"[a-z0-9_]{1,64}", label), "closed RustFS label required")
    selected = [r for r in rustfs["runs"] if r["label"] == label]
    need(len(selected) == 1, "selected RustFS arm is missing or duplicate")
    rs_run = selected[0]
    for key in ("source_revision", "binary_sha256", "archive_manifest_sha256", "protected_fixture_digest"):
        need(fs_run[key] == rs_run[key], "same-source pair identity differs")
    need(len(public["tables"]) == 16, "filesystem table inventory differs")
    verify_run(fs_run, public["tables"], True)
    verify_run(rs_run, rustfs["tables"], False)
    expected_match = module.matched_rustfs_summary(dict(spec["rustfs_public"], label=label), fs_run)
    need(public["matched_comparison"] == expected_match
         and public["denominators"]["comparison"] == expected_match["scope"], "final matched comparison differs")
    passed = controls(module, spec, fs_run, rustfs, spec["rustfs_public"], label)
    result = {"schema": "mount-rs-native-final-pair-verification-v1", "qualified": True,
              "input_spec_sha256": sys.argv[2], "verifier_sha256": verifier_sha,
              "filesystem_public_sha256": spec["filesystem_public"]["sha256"],
              "rustfs_public_sha256": spec["rustfs_public"]["sha256"], "extractor_sha256": EXTRACTOR_SHA,
              "source_revision": fs_run["source_revision"], "binary_sha256": fs_run["binary_sha256"],
              "arms": 2, "cells_per_arm": 16, "servers": 10, "drives": 10, "partitions": 5,
              "files_per_drive": 1000, "fresh_passes_per_arm": 2, "crossnode_pairs_per_arm": 100,
              "independent_arithmetic": ["rational_throughput", "logical_payload", "sql_cluster_sums_and_quotients",
                 "allocation_cluster_sums_and_quotients", "process_cpu_percent_and_aggregates", "quic_cluster_sums", "http_missingness_and_counts"],
              "negative_controls_rejected": passed,
              "scope": "offline public-pair consistency with pinned extractor rejection controls; no new workload, crash/power-loss, physical IOPS, backend saturation, cross-host or 10000-client proof"}
    sys.stdout.write(json.dumps(result, sort_keys=True, separators=(",", ":"), allow_nan=False) + "\n")


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        sys.stderr.write("pair verifier rejected: " + (str(error) if isinstance(error, Rejected) else type(error).__name__) + "\n")
        sys.exit(1)
