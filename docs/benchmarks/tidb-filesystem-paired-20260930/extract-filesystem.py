#!/usr/bin/env python3
"""Offline settled TiDB/filesystem D10 extractor derived from the pinned paired extractor.

Usage: python3 THIS_SCRIPT SPEC_JSON SPEC_SHA256
Read immutable artifacts only; emit allowlisted public JSON to stdout only.
No subprocesses, environment reads, writes, network, fixture mutation or runtime.
Root owns run qualification; exact run and input pins are mandatory.
"""
import gzip
import io
import hashlib
import json
import math
import sys
import re
import posixpath
import zlib
import os
import stat
from fractions import Fraction
from pathlib import Path, PurePosixPath


EMPTY_SHA256 = hashlib.sha256(b"").hexdigest()
MODES = ("mostly_idle", "all_active")
PATTERNS = ("sequential_read", "random_read", "sequential_overwrite",
            "random_overwrite", "mixed", "hot_file", "append_truncate", "churn")
CELLS = {(mode, pattern) for mode in MODES for pattern in PATTERNS}
SQL = tuple("tidb.sql." + name for name in
            ("session", "ddl", "metadata_read", "metadata_write", "inode_read",
             "inode_write", "block_read", "block_write", "flush_probe"))
WAITS = ("tidb.pool.checkout", "tidb.tx.commit", "tidb.tx.rollback")
BLOCKS = tuple("sdk.blocks." + name for name in
               ("get", "put", "flush", "verify_concurrent_backing"))
COUNTERS = ("calls", "success", "error", "cancelled", "elapsed_ns", "bytes",
            "returned_rows", "returned_row_observations")
QUIC = ("udp_rx_bytes", "udp_tx_bytes", "udp_rx_datagrams", "udp_tx_datagrams",
        "udp_rx_ios", "udp_tx_ios", "sent_packets", "lost_bytes", "lost_packets",
        "congestion_events", "sent_plpmtud_probes", "lost_plpmtud_probes", "black_holes_detected")
BUDGETS = {"host_free_floor": 64 * 1024**3,
           "rss_cap_per_owned_process": 24 * 1024**3,
           "phase_seconds": 600, "population_seconds": 600, "request_seconds": 30,
           "setup_seconds": 600, "work_seconds": 1800,
           "child_cleanup_seconds": 95, "client_cleanup_seconds": 30,
           "expected_receipt_seconds": 30, "oracle_cleanup_seconds": 30}
LIMIT = 32 * 1024**2
REQUIRED = ("core", "storage", "process", "service_quiescence", "server_quic", "runtime_activation")
UNAVAILABLE = ("sqlite_live_cache_sql", "direct_sdk_raw_object_store", "http_attempts", "physical_iops")
ALLOCATION_COUNTERS = ("rust_allocations", "rust_deallocations", "rust_reallocations",
                       "rust_allocated_bytes", "rust_freed_bytes")
ALLOCATION_SCOPE = ("optional System Rust allocator atomics; excludes foreign C allocators; "
                    "instrumentation affects throughput; live bytes are endpoint gauges")
PROCESS_GAUGES = ("rss_end_bytes", "lifetime_peak_rss_bytes", "sqlite_heap_end_bytes",
                  "sqlite_heap_lifetime_peak_bytes", "rust_live_end_bytes")


class ExtractionError(Exception):
    pass


def require(condition, message):
    if not condition:
        raise ExtractionError(message)


def integer(value):
    require(type(value) is int and 0 <= value <= 2**64 - 1, "invalid unsigned counter")
    return value


def digest(data):
    return hashlib.sha256(data).hexdigest()


def read_file(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
    try:
        before = os.fstat(fd)
        require(stat.S_ISREG(before.st_mode) and before.st_uid == os.geteuid()
                and before.st_nlink == 1 and 0 <= before.st_size <= LIMIT,
                "raw file is not one bounded owner-issued regular file")
        with os.fdopen(fd, "rb", closefd=False) as stream:
            raw = stream.read(LIMIT + 1)
        after = os.fstat(fd)
        named = os.stat(path, follow_symlinks=False)
        fields = ("st_dev", "st_ino", "st_uid", "st_mode", "st_nlink", "st_size", "st_mtime_ns", "st_ctime_ns")
        require(len(raw) == before.st_size and all(getattr(before, k) == getattr(after, k) == getattr(named, k)
                for k in fields), "raw input identity/size changed during extraction")
        return raw
    finally:
        os.close(fd)


def read_json(path, expected_hash=None):
    raw = read_file(path)
    actual_hash = digest(raw)
    require(expected_hash is None or actual_hash == expected_hash, "raw file hash mismatch")
    if path.suffix == ".gz":
        decoder = zlib.decompressobj(16 + zlib.MAX_WBITS)
        decoded = decoder.decompress(raw, LIMIT + 1)
        require(len(decoded) <= LIMIT and decoder.eof and not decoder.unused_data
                and not decoder.unconsumed_tail, "invalid, concatenated or excessive gzip input")
    else:
        decoded = raw
    return json.loads(decoded, object_pairs_hook=unique_object,
                      parse_float=FloatToken, parse_constant=reject_constant), actual_hash



def subtract_checked(before, after, delta):
    if isinstance(before, dict):
        require(isinstance(after, dict) and isinstance(delta, dict)
                and before.keys() == after.keys() == delta.keys(), "counter object shape changed")
        for key in before:
            subtract_checked(before[key], after[key], delta[key])
    elif isinstance(before, list):
        require(isinstance(after, list) and isinstance(delta, list)
                and len(before) == len(after) == len(delta), "counter array shape changed")
        for a, b, d in zip(before, after, delta):
            subtract_checked(a, b, d)
    else:
        a, b, d = integer(before), integer(after), integer(delta)
        require(b >= a and d == b - a, "counter reset or delta mismatch")


def check_entries(before, after, delta):
    require(len(before) == len(after) == len(delta), "metric entry shape changed")
    require(len({row["name"] for row in before}) == len(before), "duplicate metric name")
    for a, b, d in zip(before, after, delta):
        require(a.keys() == b.keys() == d.keys() and a["name"] == b["name"] == d["name"],
                "metric entry identity changed")
        for key in a:
            if key == "name":
                continue
            if key in ("in_flight", "max_elapsed_ns"):
                require(d[key]["before"] == a[key] and d[key]["after"] == b[key],
                        "endpoint gauge mismatch")
            else:
                subtract_checked(a[key], b[key], d[key])
        if "latency_log2_us" in d:
            require(len(d["latency_log2_us"]) == 32
                    and sum(d["latency_log2_us"]) == d["calls"], "latency histogram mismatch")
        if all(key in d for key in ("success", "error", "cancelled")):
            require(d["calls"] == d["success"] + d["error"] + d["cancelled"],
                    "operation outcome count mismatch")


def boundary(root, reference, row, role, server, terminal, hashes):
    rel = Path(reference["file"])
    require(not rel.is_absolute() and ".." not in rel.parts and rel.suffix == ".gz",
            "invalid boundary file reference")
    path = (root / rel).resolve()
    require(path.is_relative_to(root), "boundary file escapes owner")
    value, sha = read_json(path, reference["sha256"])
    require(reference["metrics_complete"] is True, "boundary index incomplete")
    require(value["schema"] == "mount-rs-phase-metrics-v1", "unexpected boundary schema")
    require(all(value[key] is True for key in
                ("enabled", "capture_complete", "metrics_complete", "accounting_complete")),
            "boundary incomplete")
    identity = value["identity"]
    require(all(identity[key] == row[key] for key in
                ("sequence", "generation", "phase", "boundary")), "boundary index identity mismatch")
    require(identity["role"] == role and identity["server"] == server
            and identity["binary_digest"] == terminal["source"]["binary_sha256"]
            and identity["source_digest"] == terminal["source"]["digest"]
            and identity["controller_pid"] == terminal["controller_resources"]["pid"],
            "boundary source/process role mismatch")
    if role == "worker":
        require(identity["pid"] == reference["pid"]
                and value["server_quic"]["complete"] is True
                and value["server_quic"]["counter_saturated"] is False
                and value["runtime_activation"]["complete"] is True,
                "worker observation incomplete")
        require(identity["pid"] == next(w["pid"] for w in terminal["workers"] if w["server"] == server),
                "worker terminal PID mismatch")
    else:
        require(identity["pid"] == identity["controller_pid"], "controller PID mismatch")
    require(all(value["quiescence"][key] is True for key in
                ("controller_work_drained", "application_quiescent", "instrumented_storage_in_flight_zero"))
            and value["storage"]["in_flight"] == 0, "boundary is not quiescent")
    hashes.append({"sequence": row["sequence"], "role": role, "server": server, "sha256": sha})
    return value


def checked_delta(before, after):
    a, b = before["identity"], after["identity"]
    require(a.keys() == b.keys() and all(a[key] == b[key] for key in a
                if key not in ("sequence", "phase", "boundary")), "boundary delta identity changed")
    require(a["phase"] == b["phase"] and a["boundary"] == "before_active"
            and b["boundary"] == "after_active" and b["sequence"] == a["sequence"] + 1,
            "unexpected active boundary pair")
    raw = after["delta_from_previous"]
    require(raw["complete"] is True and raw["before_sequence"] == a["sequence"],
            "delta baseline mismatch or incomplete")
    delta = raw["counters"]
    for family in ("core", "storage"):
        check_entries(before[family]["entries"], after[family]["entries"], delta[family])
    for key, value in delta["process_counters"].items():
        subtract_checked(before["process_since_baseline"][key],
                         after["process_since_baseline"][key], value)
    for key, value in delta["process_gauges"].items():
        require(value["before"] == before["process_since_baseline"][key]
                and value["after"] == after["process_since_baseline"][key], "process gauge mismatch")
        for endpoint in ("before", "after"):
            if value[endpoint] is not None:
                integer(value[endpoint])
    require(set(delta["process_gauges"]) == set(PROCESS_GAUGES),
            "process gauge inventory changed")
    require(before["process_since_baseline"]["rust_allocator_instrumented"] is True
            and after["process_since_baseline"]["rust_allocator_instrumented"] is True,
            "allocation instrumentation unavailable or changed")
    allocation = delta["allocations"]
    require(set(allocation) == {"available", "status", "counters", "scope"}
            and allocation["available"] is True and allocation["status"] == "measured"
            and allocation["scope"] == ALLOCATION_SCOPE
            and set(allocation["counters"]) == set(ALLOCATION_COUNTERS),
            "allocation delta coverage or inventory changed")
    for key in ALLOCATION_COUNTERS:
        subtract_checked(before["process_since_baseline"][key],
                         after["process_since_baseline"][key], allocation["counters"][key])
    for sample in (before, after):
        integer(sample["process_since_baseline"]["rust_live_end_bytes"])
    if a["role"] == "worker":
        check_entries(before["server_quic"]["entries"], after["server_quic"]["entries"], delta["service"])
        for sample in (before, after):
            transport = sample["server_quic"]["transport"]
            require(transport["registry_complete"] is True
                    and transport["unobserved_connections"] == transport["missing_final_samples"] == 0,
                    "QUIC registry incomplete")
        transport_delta = delta["server_transport"]["observed_total"]
        subtract_checked(before["server_quic"]["transport"]["observed_total"],
                         after["server_quic"]["transport"]["observed_total"], transport_delta)
        subtract_checked(before["server_quic"]["transport"]["retired_connections"],
                         after["server_quic"]["transport"]["retired_connections"],
                         delta["server_transport"]["retired_connections"])
    return delta


def operations(deltas, names):
    result = []
    for name in names:
        rows = [next(row for row in delta["storage"] if row["name"] == name) for delta in deltas]
        row = {"name": name, **{key: sum(integer(item[key]) for item in rows) for key in COUNTERS}}
        row["latency_log2_us"] = [sum(integer(item["latency_log2_us"][i]) for item in rows)
                                   for i in range(32)]
        result.append(row)
    return result


def process_summary(deltas):
    return {
        "counters": {key: integer(sum(integer(d["process_counters"][key]) for d in deltas))
                     for key in ("cpu_user_us", "cpu_system_us", "minor_faults", "major_faults",
                                 "block_inputs", "block_outputs", "voluntary_context_switches",
                                 "involuntary_context_switches")},
        "cpu_user_us": sum(integer(d["process_counters"]["cpu_user_us"]) for d in deltas),
        "cpu_system_us": sum(integer(d["process_counters"]["cpu_system_us"]) for d in deltas),
        "rss_endpoints_bytes": [{"before": d["process_gauges"]["rss_end_bytes"]["before"],
                                 "after": d["process_gauges"]["rss_end_bytes"]["after"]} for d in deltas],
        "lifetime_peak_rss_bytes": [d["process_gauges"]["lifetime_peak_rss_bytes"]["after"] for d in deltas],
        "process_gauges": [{key: {endpoint: d["process_gauges"][key][endpoint]
                                   for endpoint in ("before", "after")}
                            for key in PROCESS_GAUGES} for d in deltas],
    }


def allocation_summary(deltas):
    require(deltas and all(d["allocations"]["available"] is True
            and d["allocations"]["status"] == "measured"
            and d["allocations"]["scope"] == ALLOCATION_SCOPE
            and set(d["allocations"]["counters"]) == set(ALLOCATION_COUNTERS)
            for d in deltas), "allocation aggregate coverage incomplete")
    return {"available": True, "status": "measured",
            "counters": {key: integer(sum(integer(d["allocations"]["counters"][key])
                                          for d in deltas)) for key in ALLOCATION_COUNTERS},
            "scope": ALLOCATION_SCOPE}



def extract(receipt_path, expected):
    drives = expected["drives"]
    partitions = drives // 2
    total_files = drives * 1000
    receipt_path = receipt_path.resolve()
    require(receipt_path.name == "receipt.json", "expected owner receipt")
    owner = receipt_path.parent
    receipt, receipt_sha = read_json(receipt_path, expected["receipt"])
    require(receipt["schema"] == "mount-rs.filesystem-stage-owner.candidate.v1"
            and receipt["status"] == "benchmarked" and receipt["failure"] is None
            and receipt["finish_errors"] == [] and receipt["filesystem_data_retained"] is True
            and receipt["benchmark_group_absent"] is True
            and receipt["benchmark_direct_child_reaped"] is True
            and receipt["original_container_mutations"] is False
            and receipt["new_container_mutations"] is False,
            "owner is not qualified and settled")
    for key, pin in (("combined_head", "source"), ("combined_binary_sha256", "binary"),
                     ("combined_manifest_sha256", "manifest"), ("producer_sha256", "producer"),
                     ("combined_source_count", "source_count"), ("combined_filtered_test_count", "filtered_tests")):
        require(receipt[key] == expected[pin], "owner source/binary/producer pin mismatch")
    require(receipt["source_pins_after_equal_before"] is True
            and receipt["mount"] == "filesystem"
            and receipt["whole_limit_seconds"] == 3300
            and receipt["test_limit_seconds"] == 3000
            and receipt["cleanup_reserve_seconds"] == 120,
            "owner source pins changed")
    benchmark = receipt["benchmark"]
    require(all(benchmark[key] is True for key in ("both_modes_and_eight_patterns", "exact_named_case_passed",
                "initial_and_final_oracles", "revocation_and_workers_clean", "sixteen_positive_cells")),
            "owner correctness qualification incomplete")
    terminal_path = (owner / "combined-target/terminal.json").resolve()
    require(terminal_path.is_relative_to(owner) and Path(benchmark["terminal_path"]).resolve() == terminal_path
            and benchmark["terminal_sha256"] == expected["terminal"], "unexpected terminal reference")
    terminal, terminal_sha = read_json(terminal_path, benchmark["terminal_sha256"])
    require(terminal["schema"] == "mount-rs-production-target-v1" and terminal["outcome"] == "success"
            and terminal["cleanup_errors"] == [] and terminal["error"] is None,
            "native terminal is not successful")
    require(all(terminal[key] is True for key in ("workload_complete", "fresh_oracle_complete", "metrics_complete")),
            "terminal correctness/metrics incomplete")
    source = terminal["source"]
    require(source["revision"] == expected["source"] and source["binary_sha256"] == expected["binary"]
            and source["digest"] == expected["fixture"] and source["checkout_status"] == ""
            and source["tracked_dirty_patch_sha256"] == EMPTY_SHA256
            and source["resource_profiling"] is True and source["allocation_profiling"] is True
            and source["debug_assertions"] is False and source["sdk_runtime"] is True,
            "unexpected source/build/profile qualification")
    config = terminal["configuration"]
    require(config == {"drives": drives, "files": 1000, "full_target": False, "population_seconds": 600,
                       "provider": "tidb", "seconds": 5}
            and (receipt["drives"], receipt["files_per_drive"], receipt["seconds_per_cell"]) == (drives, 1000, 5)
            and terminal["metadata_provider"] == "tidb" and terminal["block_provider"] == "filesystem",
            "geometry/provider mismatch")
    require(terminal["budgets"] == BUDGETS, "guard limits changed")
    workers = terminal["workers"]
    require(len(workers) == 10 and {w["server"] for w in workers} == set(range(10)), "worker fleet mismatch")
    require(len({w["pid"] for w in workers}) == 10
            and all(w["terminal"]["startup_diagnostics"]["configured_partitions"] == partitions for w in workers),
            "worker or partition identity mismatch")
    resources = [terminal["controller_resources"]] + [w["resources"] for w in workers]
    require(all(r["error"] is None and r["minimum_host_free_bytes"] >= BUDGETS["host_free_floor"]
                and r["peak_rss_bytes"] <= BUDGETS["rss_cap_per_owned_process"] for r in resources),
            "resource guard failed")
    require(all(w["exit_code"] == 0 and w["exit_signal"] is None and w["forced"] is False
                and w["reap_confirmed"] is True and w["terminal"]["clean"] is True
                and w["terminal"]["context_closed"] is True for w in workers), "worker cleanup incomplete")
    require(all(w["ready"]["startup_diagnostics"]["configured_partitions"] == partitions
                and w["ready"]["startup_diagnostics"]["construction_mode"] == "lazy" for w in workers),
            "worker partition geometry changed")
    require(terminal["connected_clients"] == drives and terminal["scope_denials"] == {"partition": drives, "sibling": drives}
            and terminal["revocation_denials"] == drives and terminal["verified_passes"] == 2
            and terminal["verified_files"] == terminal["namespace_files"] == total_files,
            "correctness witness mismatch")
    require(len(terminal["lanes"]) == drives and all(lane["failed"] == lane["uncertain"] == 0
                and lane["pending"] is None and lane["uncertain_request_ids"] == []
                and lane["attempts"] == lane["acknowledged"] for lane in terminal["lanes"]),
            "failed, uncertain, or pending requests")
    require(terminal["assigned_warmup"]["complete"] is True
            and terminal["assigned_warmup"]["acknowledged_stats"] == drives
            and terminal["crossnode_payload"]["complete"] is True
            and terminal["crossnode_payload"]["completed_pairs"] == drives * 10, "storage witnesses incomplete")
    phase = terminal["phase_metrics"]
    require(phase["enabled"] is True and phase["metrics_complete"] is True
            and phase["coverage_complete"] is False
            and phase["required_families"] == list(REQUIRED)
            and phase["known_unavailable_families"] == list(UNAVAILABLE), "unexpected phase coverage")
    timed_phases = {mode + "/" + pattern for mode, pattern in CELLS}
    timed_rows = [row for row in phase["boundaries"] if row.get("phase") in timed_phases]
    require(len(timed_rows) == 48 and all(row["complete"] is True and row["metrics_complete"] is True for row in timed_rows), "timed boundary inventory incomplete")
    index = {row["sequence"]: row for row in timed_rows}
    require(len(index) == len(timed_rows), "duplicate timed boundary sequence")
    stages = terminal["stages"]
    require(len(stages) == 16 and {(s["mode"], s["pattern"]) for s in stages} == CELLS,
            "expected sixteen distinct cells")
    cells, hashes = [], []
    for stage in stages:
        mode, pattern = stage["mode"], stage["pattern"]
        sequences = stage["metric_sequences"]
        require(len(sequences) == 3 and sequences == list(range(sequences[0], sequences[0] + 3)),
                "cell sequence triple invalid")
        rows = [index[sequence] for sequence in sequences]
        require(all(row["phase"] == mode + "/" + pattern for row in rows)
                and [row["boundary"] for row in rows] == ["before_active", "after_active", "after_idle"]
                and len({row["generation"] for row in rows}) == 1, "cell boundary identity invalid")
        active = max(1, min(100, drives // 100)) if mode == "mostly_idle" else drives
        cycles, acks = integer(stage["cycles"]), integer(stage["acknowledged_requests"])
        seconds = stage["timing"]["active_elapsed_seconds"]
        require(cycles > 0 and acks > 0 and isinstance(seconds, (int, float)) and type(seconds) is not bool and math.isfinite(seconds) and seconds > 0
                and stage["requested_seconds"] == 5
                and stage["configured_active_clients"] == stage["clients_with_completed_cycles"] == active
                and stage["connected_clients"] == drives
                and stage["idle_liveness_acknowledgments"] == drives - active, "cell geometry/result invalid")
        require(math.isclose(stage["timing"]["cycles_per_second"], cycles / seconds, rel_tol=1e-12),
                "cell rate denominator mismatch")
        processes = []
        for role, servers in (("controller", [None]), ("worker", list(range(10)))):
            for server in servers:
                snapshots = []
                for row in rows[:2]:
                    refs = [row["controller"]] if role == "controller" else [ref for ref in row["workers"] if ref["server"] == server]
                    require(len(refs) == 1, "missing/duplicate process boundary")
                    snapshots.append(boundary(terminal_path.parent, refs[0], row, role, server, terminal, hashes))
                processes.append(checked_delta(*snapshots))
        controller, worker_deltas = processes[:1], processes[1:]
        worker_sql = operations(worker_deltas, SQL)
        cells.append({"mode": mode, "pattern": pattern, "active_clients": active,
                      "cycles": cycles, "acknowledged_requests": acks,
                      "cycles_per_second": cycles / seconds, "active_elapsed_seconds": seconds,
                      "phase_elapsed_seconds": stage["timing"]["phase_elapsed_seconds"],
                      "overall_phase_elapsed_seconds": stage["elapsed_seconds"],
                      "metrics_observer_elapsed_seconds": stage["timing"]["metrics_observer_elapsed_seconds"],
                      "idle_liveness_acknowledgments": stage["idle_liveness_acknowledgments"],
                      "metric_sequences": sequences,
                      "workers": {"process_server_order": list(range(10)), "process": process_summary(worker_deltas),
                                  "allocations": allocation_summary(worker_deltas), "sql": worker_sql,
                                  "sql_calls_per_cycle": sum(row["calls"] for row in worker_sql) / cycles,
                                  "sql_rows_per_cycle": sum(row["returned_rows"] for row in worker_sql) / cycles,
                                  "waits": operations(worker_deltas, WAITS), "logical_blocks": operations(worker_deltas, BLOCKS),
                                  "quic": {key: sum(d["server_transport"]["observed_total"][key] for d in worker_deltas) for key in QUIC}},
                      "controller": {"process": process_summary(controller), "allocations": allocation_summary(controller),
                                     "sql": operations(controller, SQL),
                                     "waits": operations(controller, WAITS), "logical_blocks": operations(controller, BLOCKS)}})
    corpus = benchmark["fresh_oracle_corpus"]
    require(corpus["complete"] is True and corpus["completed_drives"] == drives
            and corpus["initial_files"] == corpus["final_files"] == total_files, "fresh corpus incomplete")
    return {"source_revision": source["revision"], "binary_sha256": source["binary_sha256"],
            "archive_manifest_sha256": receipt["combined_manifest_sha256"], "archive_source_count": receipt["combined_source_count"],
            "owner_producer_sha256": receipt["producer_sha256"], "owner_receipt_sha256": receipt_sha,
            "terminal_sha256": terminal_sha, "protected_fixture_digest": source["digest"],
            "protected_fixture_scope": "benchmark seams, not complete native archive source closure",
            "owner_source_pins_unchanged": True,
            "owner_source_pin_count": None,
            "owner_source_pin_count_scope": "owner does not export helper-plus-source pin count; full archive inventory verified separately",
            "geometry": {"servers": 10, "clients": drives, "drives": drives, "partitions": partitions, "files_per_drive": 1000, "seconds_per_cell": 5,
                         "configured_partitions_per_server": partitions, "construction_mode": "lazy",
                         "metadata_provider": "tidb", "block_provider": "filesystem"},
            "guard_limits": BUDGETS,
            "resources": {"minimum_observed_host_free_bytes": min(r["minimum_host_free_bytes"] for r in resources),
                          "controller_peak_rss_bytes": resources[0]["peak_rss_bytes"],
                          "worker_peak_rss_bytes": [w["resources"]["peak_rss_bytes"] for w in sorted(workers, key=lambda w: w["server"])]},
            "correctness": {"owner_benchmarked": True, "workload_complete": True, "fresh_oracle_complete": True,
                            "workers_clean_reaped": True, "scope_denials": {"partition": drives, "sibling": drives},
                            "revocation_denials": drives, "verified_passes": 2,
                            "crossnode_completed_pairs": drives * 10, "crossnode_verified_bytes": terminal["crossnode_payload"]["verified_bytes"],
                            "fresh_corpus": {key: corpus[key] for key in ("initial_files", "final_files", "initial_bytes", "final_bytes",
                                                                        "initial_receipt_sha256", "final_receipt_sha256")},
                            "cumulative_requests": {key: sum(lane[key] for lane in terminal["lanes"])
                                                    for key in ("attempts", "acknowledged", "failed", "uncertain")}},
            "coverage": {"metrics_complete": True, "coverage_complete": False,
                         "required_families": list(REQUIRED), "known_unavailable_families": list(UNAVAILABLE),
                         "allocation_profiling": True, "object_store_http_configured": False,
                         "rustfs_server_otlp_extracted": False, "tikv_cgroup_io_extracted": False},
            "cells": cells, "active_boundary_sha256": hashes,
            "backing_physical_io": {"available": False, "status": "unavailable",
                "reason": "no qualified host device or TiKV I/O observation; process OS bytes are not physical IOPS"},
            "rustfs_guest_cgroup": {"available": False, "configured": False, "status": "not_configured",
                "reason": "filesystem run has no new RustFS container or cgroup", "counters": None}}


# Everything below is the new offline extraction seam. No runtime is imported.
INPUT_PIN_FIELDS = {"source", "binary", "manifest", "producer", "source_count", "filtered_tests", "drives", "fixture", "receipt", "terminal", "supervisor", "supervisor_receipt"}
HISTORICAL_SHA = "d94225a9d87e15ca8ca61392e682086cbd8556606887e9a986fe5d6b4a4e320d"
HISTORICAL_SOURCE = "9d4d09d51f7e20ae1bf73c548d6d109a98d4e64b"
HISTORICAL_BINARY = "bd2ba529f302d47dfa4b7f00d57188ecfd889698eb37f098b35e58fc4cce872c"

ROLES = ("primary_data_mixed", "primary_probe_mixed", "qualification_data",
         "qualification_probe", "standalone_data", "standalone_probe")
METHODS = ("get", "head", "put", "delete", "post", "other")
HTTP_COUNTERS = (
    "attempts_started", "header_responses", "transport_errors", "cancelled_before_headers",
    "offered_bytes", "offered_known", "offered_unknown", "known_extra_future_boxes",
    "known_extra_response_body_boxes", "bodies_started", "body_eof", "body_errors",
    "body_dropped", "body_bytes", "body_chunks", "dispatch_elapsed_ns", "body_elapsed_ns",
)
HTTP_GAUGES = ("attempts_inflight", "bodies_inflight", "dispatch_max_ns", "body_max_ns")
CORE_PREFIXES = ("filesystem.", "provider.", "compact.", "blob_cache.", "service.", "catalog.", "wire.")
STORAGE_PREFIXES = ("tidb.", "sdk.metadata.", "sdk.blocks.", "blob_cache.", "object_store.backing_marker.")
NAME_RE = re.compile(r"[A-Za-z0-9_][A-Za-z0-9_.]*\Z")
SHA_RE = re.compile(r"[0-9a-f]{64}\Z")
FRAME_PATH_RE = re.compile(r"(?:worker-[0-9]+/)?metrics/(?:g[0-9]+-s[0-9]+|startup-g[0-9]+|closed-g[0-9]+|terminal)\.json\.gz\Z")
ID_KEYS = {"pid", "role", "server", "controller_pid", "generation", "sequence", "phase",
           "boundary", "source_digest", "binary_digest", "catalog_digest", "backend_prefix"}
PUBLIC_PHASES = {mode + "/" + pattern for mode, pattern in CELLS} | {
    "assigned_warmup", "controller_cleanup", "crossnode_routes", "final_fresh_oracle",
    "initial_fresh_oracle", "online_namespace", "online_payload", "refresh_replicas",
    "replica_close", "revocation", "routes_and_scope", "worker_cleanup", "worker_setup", "worker_startup"}
PUBLIC_BOUNDARIES = {"before", "after", "before_active", "after_active", "after_idle", "after_ready", "after_batch", "ready", "terminal"}


def validate_pins(pins):
    require(set(pins) == INPUT_PIN_FIELDS and isinstance(pins["source"], str)
            and re.fullmatch(r"[0-9a-f]{40}", pins["source"]) is not None,
            "filesystem source or inventory pins incomplete")
    require(type(pins["source_count"]) is int and 0 < pins["source_count"] <= 4096
            and type(pins["filtered_tests"]) is int and 0 < pins["filtered_tests"] <= 4096
            and type(pins["drives"]) is int and pins["drives"] == 10,
            "filesystem fixed geometry or explicit inventory pins invalid")
    for key in INPUT_PIN_FIELDS - {"source", "source_count", "filtered_tests", "drives"}:
        sha_shape(pins[key])


class FloatToken(float):
    def __new__(cls, literal):
        value = super().__new__(cls, literal)
        value.literal = literal
        return value


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, "duplicate JSON key")
        result[key] = value
    return result


def reject_constant(_literal):
    raise ExtractionError("nonfinite JSON constant")


def finite_seconds(value, positive=False):
    require(isinstance(value, (int, float)) and type(value) is not bool
            and math.isfinite(value) and (value > 0 if positive else value >= 0),
            "invalid elapsed seconds")
    return Fraction(value.literal if isinstance(value, FloatToken) else str(value))


def rational(value):
    require(isinstance(value, Fraction), "invalid rational input")
    return {"numerator": value.numerator, "denominator": value.denominator,
            "value": float(value)}


def rate(count, seconds):
    return rational(Fraction(integer(count)) / finite_seconds(seconds, positive=True))


def delta_value(before, after):
    if isinstance(before, dict):
        require(isinstance(after, dict) and before.keys() == after.keys(), "counter object shape changed")
        return {key: delta_value(before[key], after[key]) for key in before}
    if isinstance(before, list):
        require(isinstance(after, list) and len(before) == len(after), "counter array shape changed")
        return [delta_value(a, b) for a, b in zip(before, after)]
    a, b = integer(before), integer(after)
    require(b >= a, "counter reset")
    return b - a


def safe_relative(value):
    require(isinstance(value, str) and re.fullmatch(r"[A-Za-z0-9_./-]+", value) is not None,
            "invalid relative artifact name")
    rel = PurePosixPath(value)
    require(not rel.is_absolute() and ".." not in rel.parts and str(rel) == value,
            "unsafe relative artifact name")
    return rel


def contained(root, relative):
    rel = safe_relative(relative)
    path = root.joinpath(*rel.parts).resolve()
    require(path.is_relative_to(root), "artifact reference escapes root")
    return path


def sha_shape(value):
    require(isinstance(value, str) and SHA_RE.fullmatch(value) is not None, "invalid SHA-256 pin")
    return value


def hash_large(path, max_bytes):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
    try:
        before = os.fstat(fd)
        require(stat.S_ISREG(before.st_mode) and before.st_uid == os.geteuid()
                and before.st_nlink == 1 and 0 <= before.st_size <= max_bytes,
                "invalid bounded owner-issued immutable file")
        size = 0
        h = hashlib.sha256()
        with os.fdopen(fd, "rb", closefd=False) as stream:
            while True:
                block = stream.read(1024 * 1024)
                if not block:
                    break
                size += len(block)
                require(size <= max_bytes, "immutable input grew beyond limit")
                h.update(block)
        after = os.fstat(fd)
        named = os.stat(path, follow_symlinks=False)
        fields = ("st_dev", "st_ino", "st_uid", "st_mode", "st_nlink", "st_size", "st_mtime_ns", "st_ctime_ns")
        require(size == before.st_size and all(getattr(before, k) == getattr(after, k) == getattr(named, k)
                for k in fields), "immutable input identity/size changed during extraction")
        return h.hexdigest(), size
    finally:
        os.close(fd)


def verify_archive(spec, expected, source):
    manifest_path = Path(spec["manifest_path"]).resolve()
    require(manifest_path.name == "manifest.json", "invalid archive manifest name")
    manifest, manifest_sha = read_json(manifest_path, expected["manifest"])
    archive = manifest_path.parent
    require(manifest["schema"] == "mount-rs.structural-lock-native-release-archive.v1"
            and manifest["head"] == expected["source"]
            and manifest["binary"]["sha256"] == expected["binary"]
            and manifest["binary"]["target"] == "quic_production_target"
            and manifest["native_inventory"]["controller_filtered"] == expected["filtered_tests"]
            and manifest["native_inventory"]["benchmarks"] == 0
            and manifest["cargo_artifact"]["build_finished_success"] is True
            and manifest["pure_tests"]["exit_code"] == 0 and manifest["pure_tests"]["failed"] == 0,
            "frozen archive qualification mismatch")
    sha_shape(manifest["source_closure_sha256"])
    sha_shape(manifest["native_inventory"]["list_sha256"])
    binary_sha, binary_bytes = hash_large(archive / "quic_production_target", 512 * 1024**2)
    require(binary_sha == expected["binary"] and binary_bytes == integer(manifest["binary"]["bytes"]),
            "frozen binary hash or size mismatch")
    require(len(manifest["sources"]) == expected["source_count"], "archive source inventory mismatch")
    bindings = {}
    for row in manifest["sources"]:
        require(set(row) == {"path", "sha256", "bytes"}, "unexpected archive source record")
        rel = str(safe_relative(row["path"]))
        require(rel not in bindings, "duplicate archive source")
        sha_shape(row["sha256"])
        raw = read_file(contained(archive / "sources", rel))
        require(digest(raw) == row["sha256"] and len(raw) == integer(row["bytes"]),
                "frozen per-file source hash or size mismatch")
        bindings[rel] = {"path": rel, "sha256": row["sha256"], "bytes": len(raw)}
    seam_bindings = []
    for seam, sha in source["sources"].items():
        require(isinstance(seam, str) and re.fullmatch(r"[A-Za-z0-9_./-]+", seam) is not None,
                "invalid fixture source seam")
        rel = seam if seam.startswith("scripts/") else posixpath.normpath(
            "crates/mount-rs-service/tests/support/production_target/" + seam)
        safe_relative(rel)
        require(rel in bindings and bindings[rel]["sha256"] == sha_shape(sha),
                "fixture source seam absent from archive")
        seam_bindings.append({"path": rel, "sha256": sha})
    for rel, sha in source["protected_sha256"].items():
        safe_relative(rel)
        require(rel in bindings and bindings[rel]["sha256"] == sha_shape(sha),
                "protected source absent from archive")
    producer_sha, _ = hash_large(Path(spec["owner_producer_path"]).resolve(), LIMIT)
    require(producer_sha == expected["producer"], "owner producer file hash mismatch")
    return {"manifest_sha256": manifest_sha, "source_closure_sha256": manifest["source_closure_sha256"],
            "source_count": len(bindings), "source_files": [bindings[k] for k in sorted(bindings)],
            "fixture_source_bindings": sorted(seam_bindings, key=lambda x: x["path"]),
            "binary_sha256": binary_sha, "binary_bytes": binary_bytes,
            "native_inventory": {key: manifest["native_inventory"][key]
                                 for key in ("tests", "benchmarks", "controller_filtered", "list_sha256")}}


def parse_object_frames(frame):
    observation = frame["object_store_observation"]
    require(observation["enabled"] is True and observation["configured"] is False
            and observation["available"] is False and observation["complete"] is False
            and observation["status"] == "not_configured" and observation["records"] == [],
            "filesystem object-store observation fabricated or configured")
    for key in ("object_store_observation", "raw_object_store", "http_attempts"):
        coverage = frame["coverage"][key]
        require(coverage["configured"] is False and coverage["available"] is False
                and coverage["complete"] is False and coverage["status"] == "not_configured",
                "filesystem object-store/HTTP coverage changed")
    return []


def validate_frame(frame, role, server, terminal):
    identity = frame["identity"]
    require(set(identity) == ID_KEYS and frame["schema"] == "mount-rs-phase-metrics-v1"
            and all(frame[k] is True for k in ("enabled", "capture_complete", "metrics_complete", "accounting_complete")),
            "referenced metric frame incomplete")
    require(identity["role"] == role and identity["server"] == server
            and identity["controller_pid"] == terminal["controller_resources"]["pid"]
            and identity["source_digest"] == terminal["source"]["digest"]
            and identity["binary_digest"] == terminal["source"]["binary_sha256"], "metric source/process identity mismatch")
    require(identity["phase"] in PUBLIC_PHASES and identity["boundary"] in PUBLIC_BOUNDARIES,
            "unexpected public boundary label")
    for k in ("pid", "controller_pid", "generation", "sequence"):
        integer(identity[k])
    require(identity["pid"] > 0 and identity["controller_pid"] > 0, "zero process identity")
    worker = next((w for w in terminal["workers"] if w["server"] == server), None) if role == "worker" else None
    require(identity["pid"] == (worker["pid"] if worker is not None else identity["controller_pid"]), "metric PID mismatch")
    ready = terminal["workers"][0]["ready"] if worker is None else worker["ready"]
    require(identity["catalog_digest"] == ready["catalog_digest"] and identity["backend_prefix"] == ready["backend_prefix"],
            "metric catalog or backing namespace changed")
    require(frame["observer"]["_status"]["complete"] is True
            and frame["observer"]["_status"]["counter_saturated"] is False
            and all(frame["quiescence"][k] is True for k in
                    ("controller_work_drained", "application_quiescent", "instrumented_storage_in_flight_zero"))
            and integer(frame["storage"]["in_flight"]) == 0, "metric quiescence/accounting incomplete")
    for family in ("core", "storage", "process"):
        require(all(frame["coverage"][family][k] is True for k in ("enabled", "available", "complete")),
                "required metric family unavailable")
    require(frame["process_since_baseline"]["rust_allocator_instrumented"] is True,
            "referenced frame allocation instrumentation unavailable")
    for key in ALLOCATION_COUNTERS + ("rust_live_end_bytes",):
        integer(frame["process_since_baseline"][key])
    require(frame["coverage"]["blob_cache"]["configured"] is False
            and frame["coverage"]["blob_cache"]["available"] is False
            and frame["coverage"]["http_attempts"]["available"] is False
            and frame["coverage"]["physical_iops"]["available"] is False,
            "control cache or completeness profile changed")
    if worker is not None:
        require(frame["server_quic"]["complete"] is True
                and frame["server_quic"]["counter_saturated"] is False
                and frame["server_quic"]["transport"]["registry_complete"] is True
                and frame["server_quic"]["transport"]["unobserved_connections"] == 0
                and frame["server_quic"]["transport"]["missing_final_samples"] == 0
                and frame["runtime_activation"]["complete"] is True
                and frame["runtime_activation"]["available"] is True,
                "worker service/runtime coverage incomplete")
        runtime = frame["runtime_activation"]
        require(runtime["generation"] == identity["generation"]
                and runtime["capacity"] == terminal["configuration"]["drives"]
                and len(runtime["observations"]) == runtime["capacity"]
                and runtime["diagnostics"]["counter_saturated"] is False
                and runtime["diagnostics"]["concurrent_activity"] is False,
                "runtime geometry or quality mismatch")
    parse_object_frames(frame)


def audit_inventory(root, terminal, receipt):
    frames, bindings = {}, {}
    for row in terminal["phase_metrics"]["boundaries"]:
        require(row["complete"] is True and row["metrics_complete"] is True, "full metric inventory incomplete")
        refs = []
        if "controller" in row:
            refs.append(("controller", None, row["controller"], False))
        for ref in row.get("workers", []):
            refs.append(("worker", integer(ref["server"]), ref, False))
        for ref in row.get("readiness_metrics", []):
            refs.append(("worker", integer(ref["server"]), ref, True))
        if "sequence" in row:
            require(len(row["workers"]) == 10 and {r["server"] for r in row["workers"]} == set(range(10))
                    and row["issued_workers"] == 10 and row["error"] is None, "metric fleet boundary incomplete")
        for role, server, ref, startup in refs:
            rel = str(safe_relative(ref["file"]))
            require(FRAME_PATH_RE.fullmatch(rel) is not None, "unexpected metric artifact path")
            require((rel.startswith("worker-" + str(server) + "/") if role == "worker"
                     else rel.startswith("metrics/")), "metric artifact role/path mismatch")
            sha_shape(ref["sha256"])
            frame, sha = read_json(contained(root, rel), ref["sha256"])
            validate_frame(frame, role, server, terminal)
            identity = frame["identity"]
            require(ref.get("metrics_complete", True) is True, "metric reference incomplete")
            if startup:
                require(identity["generation"] == ref["generation"], "readiness generation identity mismatch")
                if "/startup-g" in rel:
                    require(identity["phase"] == "worker_startup" and identity["boundary"] == "ready"
                            and identity["generation"] == row["generation"]
                            and identity["sequence"] == row["sequence"] - 1, "startup frame identity mismatch")
                else:
                    require("/closed-g" in rel and identity["phase"] == "replica_close"
                            and identity["boundary"] == "after"
                            and identity["generation"] == row["generation"] - 1
                            and identity["sequence"] == row["sequence"], "closed replica frame identity mismatch")
            else:
                require(identity["phase"] == row["phase"] and identity["boundary"] == row["boundary"],
                        "metric boundary phase mismatch")
                for field in ("generation", "sequence"):
                    if field in row:
                        require(identity[field] == row[field], "metric boundary sequence/generation mismatch")
            if "pid" in ref:
                require(identity["pid"] == ref["pid"], "metric reference PID mismatch")
            require(rel not in bindings or bindings[rel]["sha256"] == sha, "conflicting artifact bindings")
            frames[rel] = frame
            bindings[rel] = {"file": rel, "sha256": sha, "role": role, "server": server,
                             "generation": identity["generation"], "sequence": identity["sequence"],
                             "phase": identity["phase"], "boundary": identity["boundary"]}
    require(len(frames) == receipt["encoded_metric_frames"], "encoded metric inventory count mismatch")
    for worker in terminal["workers"]:
        for kind in ("ready", "terminal"):
            ref = worker[kind]["phase_metrics"]
            rel = "worker-" + str(worker["server"]) + "/" + ref["file"]
            require(rel in frames and ref["metrics_complete"] is True, "worker metric alias unbound")
            if "sha256" in ref:
                require(ref["sha256"] == bindings[rel]["sha256"], "worker alias hash mismatch")
    return frames, [bindings[k] for k in sorted(bindings)]


def verify_runtime_delta(before, after, delta):
    a, b = before["runtime_activation"], after["runtime_activation"]
    require(a["schema"] == b["schema"] and a["generation"] == b["generation"]
            and a["capacity"] == b["capacity"] and a["available"] is b["available"] is True,
            "runtime delta identity mismatch")
    gauges = {"registered", "resident", "opening", "ready", "closing", "quarantined", "pinned"}
    require(a["pool"].keys() == b["pool"].keys() == delta["pool"].keys(), "runtime pool shape changed")
    for key in a["pool"]:
        if key in gauges:
            require(delta["pool"][key]["before"] == a["pool"][key]
                    and delta["pool"][key]["after"] == b["pool"][key], "runtime pool gauge mismatch")
        else:
            subtract_checked(a["pool"][key], b["pool"][key], delta["pool"][key])
    check_entries(a["diagnostics"]["entries"], b["diagnostics"]["entries"], delta["diagnostics"]["entries"])
    require(len(a["observations"]) == len(b["observations"]) == len(delta["observations"]), "runtime Drive inventory changed")
    for aa, bb, dd in zip(a["observations"], b["observations"], delta["observations"]):
        require(aa["drive"] == bb["drive"] == dd["drive"] and aa["expected_backing"] == bb["expected_backing"]
                and (aa["observed_backing"] is None or aa["observed_backing"] == bb["observed_backing"])
                and dd["observed_backing"]["before"] == aa["observed_backing"]
                and dd["observed_backing"]["after"] == bb["observed_backing"], "runtime Drive identity mismatch")
        subtract_checked(aa["constructed"], bb["constructed"], dd["constructed"])


def histogram_bounds(histogram, rpc=False):
    require(len(histogram) == 32, "unexpected latency histogram size")
    total = sum(integer(x) for x in histogram)
    result = {"samples": total, "kind": "log2_microsecond_bucket_bounds", "percentiles": {}}
    for name, n, d in (("p50", 50, 100), ("p95", 95, 100), ("p99", 99, 100)):
        if total == 0:
            result["percentiles"][name] = None
            continue
        rank = (total * n + d - 1) // d
        cumulative = 0
        for bucket, count in enumerate(histogram):
            cumulative += count
            if cumulative >= rank:
                result["percentiles"][name] = {
                    "rank": rank, "bucket": bucket,
                    "lower_us": 0 if bucket == 0 or (rpc and bucket == 1) else 2 ** (bucket - 1),
                    "upper_us_exclusive": None if bucket == 31 else 2**bucket,
                    "censored": bucket == 31,
                }
                break
    return result


def selected_entries(delta, family, prefixes):
    result = []
    for raw in delta[family]:
        name = raw["name"]
        require(isinstance(name, str) and NAME_RE.fullmatch(name) is not None, "invalid fixed metric label")
        if not name.startswith(prefixes):
            continue
        fields = ("calls", "elapsed_ns", "units") if family == "core" else COUNTERS
        row = {"name": name, **{k: integer(raw[k]) for k in fields}}
        if family != "core":
            row["latency_log2_us"] = [integer(x) for x in raw["latency_log2_us"]]
            row["latency_bounds"] = histogram_bounds(row["latency_log2_us"])
        result.append(row)
    return result


def aggregate_entries(rows):
    require(rows and all([r["name"] for r in x] == [r["name"] for r in rows[0]] for x in rows),
            "selected metric inventory changed")
    result = []
    for index, first in enumerate(rows[0]):
        row = {"name": first["name"]}
        for key in first:
            if key in ("name", "latency_bounds"):
                continue
            if key == "latency_log2_us":
                row[key] = [sum(integer(x[index][key][bucket]) for x in rows) for bucket in range(32)]
            else:
                row[key] = sum(integer(x[index][key]) for x in rows)
        if "latency_log2_us" in row:
            row["latency_bounds"] = histogram_bounds(row["latency_log2_us"])
        result.append(row)
    return result


def http_delta(before, after):
    parse_object_frames(before)
    parse_object_frames(after)
    return {"configured": False, "available": False, "status": "not_configured",
            "http": None, "generic_adapter_cache": None,
            "scope": "filesystem blobs do not use object-store/HTTP or generic object-store adapter cache"}


def process_detail(before, after, delta):
    seconds = finite_seconds(after["process_observation_elapsed_seconds"]) - finite_seconds(before["process_observation_elapsed_seconds"])
    require(seconds > 0, "process observation interval did not advance")
    cpu_us = integer(delta["process_counters"]["cpu_user_us"]) + integer(delta["process_counters"]["cpu_system_us"])
    for key in ("rss_end_bytes", "lifetime_peak_rss_bytes"):
        for endpoint in ("before", "after"):
            value = delta["process_gauges"][key][endpoint]
            if value is not None:
                integer(value)
    return {"cpu_user_us": delta["process_counters"]["cpu_user_us"],
            "cpu_system_us": delta["process_counters"]["cpu_system_us"],
            "observation_elapsed_seconds": rational(seconds),
            "percent_of_one_core": rational(Fraction(cpu_us, 10000) / seconds),
            "rss_endpoints_bytes": {k: delta["process_gauges"]["rss_end_bytes"][k] for k in ("before", "after")},
            "lifetime_peak_rss_bytes": delta["process_gauges"]["lifetime_peak_rss_bytes"]["after"],
            "process_gauges": {key: {endpoint: delta["process_gauges"][key][endpoint]
                                      for endpoint in ("before", "after")}
                               for key in PROCESS_GAUGES},
            "allocations": allocation_summary([delta]),
            "counters": {key: integer(value) for key, value in delta["process_counters"].items()},
            "os_accounting": process_os_accounting(before, after)}


def decimal_counter(value):
    require(isinstance(value, str) and len(value) <= 20
            and re.fullmatch(r"0|[1-9][0-9]*", value) is not None,
            "OS accounting requires exact unsigned decimal counters")
    return integer(int(value))


def process_os_accounting(before, after):
    scope = ("own-process OS byte accounting between metric process snapshots; includes all process/"
             "observer/background disk work; cached/dirty/writeback semantics vary by OS; not physical "
             "flash IOPS, TiKV/RustFS activity or isolated filesystem-provider I/O")
    interval = after["process_since_previous_boundary"]["os_io"]
    require(interval["schema"] == "mount-rs-os-io-v1", "unexpected OS accounting schema")
    device = interval["host_block_device"]
    require(device["status"] == "unselected" and device["complete"] is False
            and device["counters"] is None, "unrequested physical device observation")
    process = interval["process_disk"]
    require("available" not in process, "unexpected OS observation availability shape")
    if process["status"] != "available":
        require(process["status"] in ("disabled", "unsupported", "unavailable")
                and process["complete"] is False and process["counters"] is None,
                "OS accounting missingness fabricated")
        return {"available": False, "status": process["status"], "counters": None, "scope": scope}
    require(process["complete"] is True and interval["enabled_start"] is True
            and interval["enabled_end"] is True, "OS accounting interval incomplete")
    nanos = decimal_counter(interval["interval_ns"])
    require(nanos > 0, "OS accounting interval did not advance")
    a, b = process["before"], process["after"]
    require(a["status"] == b["status"] == "available", "OS accounting endpoint unavailable")
    a, b = a["sample"], b["sample"]
    require(a["source"] == b["source"] and a["source"] in
            ("linux_proc_self_io", "darwin_proc_pid_rusage_v2")
            and a["identity"] == b["identity"]
            and a["identity"]["pid"] == before["identity"]["pid"] == after["identity"]["pid"],
            "OS accounting process identity changed")
    require(set(a["identity"]) == {"pid", "start_token"}, "OS accounting process identity shape changed")
    decimal_counter(a["identity"]["start_token"])
    require(before["process_since_baseline"]["os_io"]["process_disk"]["after"]["sample"] == a
            and after["process_since_baseline"]["os_io"]["process_disk"]["after"]["sample"] == b,
            "OS accounting endpoints do not match active process boundaries")
    counters = process["counters"]
    require(set(counters) == {"read_bytes", "write_bytes", "linux_counters"}, "OS counter inventory changed")
    for key in ("read_bytes", "write_bytes"):
        aa, bb, dd = (decimal_counter(x[key]) for x in (a, b, counters))
        require(bb >= aa and dd == bb - aa, "OS byte counter reset or delta mismatch")
    linux = counters["linux_counters"]
    if a["source"] == "linux_proc_self_io":
        keys = {"rchar", "wchar", "syscr", "syscw", "read_bytes", "write_bytes", "cancelled_write_bytes"}
        require(isinstance(linux, dict) and set(linux) == keys
                and set(a["linux_counters"]) == set(b["linux_counters"]) == keys,
                "Linux OS accounting inventory incomplete")
        for key in keys:
            aa, bb, dd = (decimal_counter(x[key]) for x in (a["linux_counters"], b["linux_counters"], linux))
            require(bb >= aa and dd == bb - aa, "Linux OS counter reset or delta mismatch")
    else:
        require(a["linux_counters"] is b["linux_counters"] is linux is None,
                "Darwin OS accounting contains fabricated Linux counters")
    return {"available": True, "status": "measured", "source": a["source"],
            "pid": a["identity"]["pid"], "interval_ns": interval["interval_ns"],
            "counters": counters, "scope": scope}


def verify_oracles(root, terminal, drives):
    passes = terminal["fresh_oracle_passes"]
    require(len(passes) == 2 and [x["pass"] for x in passes] == ["initial", "final"], "fresh oracle pass inventory mismatch")
    bindings = []
    for p in passes:
        ref = p["receipt"]
        require(ref["file"] == "oracle-receipts/" + p["pass"] + ".json", "unexpected oracle receipt path")
        witness, sha = read_json(contained(root, ref["file"]), ref["sha256"])
        require(witness["complete"] is True and witness["settled"] is True and p["after_boundary_complete"] is True
                and witness["completed_drive_ids"] == list(range(drives))
                and witness["pass"] == p["pass"] and witness["live_slots"] == 0
                and witness["completed_drives"] == witness["expected_drives"] == witness["started_drives"] == drives
                and witness["completed_files"] == witness["expected_files"] == witness["checked_files"] == drives * 1000
                and witness["completed_bytes"] == witness["expected_bytes"] == witness["compared_bytes"],
                "fresh oracle corpus or roster incomplete")
        require(all(p[k] == v for k, v in witness.items() if k != "completed_drive_ids"), "oracle summary mismatch")
        bindings.append({"file": ref["file"], "sha256": sha, "pass": p["pass"],
                         "files": witness["checked_files"], "bytes": witness["compared_bytes"]})
    refs = terminal["expected_state_receipts"]
    require(len(refs) == drives and {r["drive"] for r in refs} == set(range(drives)), "expected Drive roster incomplete")
    for ref in refs:
        drive = integer(ref["drive"])
        require(ref["file"] == "expected/drive-" + str(drive) + ".json", "unexpected expected ledger path")
        witness, sha = read_json(contained(root, ref["file"]), ref["sha256"])
        require(witness["drive"] == drive and len(witness["files"]) == 1000, "expected ledger identity mismatch")
        bindings.append({"file": ref["file"], "sha256": sha, "drive": drive})
    batches = terminal["crossnode_route_batches"]
    require(len(batches) == 10 and {b["offset"] for b in batches} == set(range(10))
            and all(b["rpc_complete"] is True and b["validation_complete"] is True
                    and b["acknowledged_stats"] == b["expected_stats"] == drives
                    and b["verified_reads"] == b["expected_reads"] == drives
                    and b["verified_bytes"] == b["expected_bytes"] == drives * 4096 for b in batches),
            "cross-node payload roster incomplete")
    return bindings


def enrich_run(spec, expected, output):
    owner = Path(spec["owner_root"]).resolve()
    receipt, _ = read_json(owner / "receipt.json", expected["receipt"])
    terminal, _ = read_json(owner / "combined-target/terminal.json", expected["terminal"])
    require(terminal["phase"] == "terminal" and terminal["fresh_oracle_settled"] is True,
            "native run is not final and settled")
    for worker in terminal["workers"]:
        ready, final = worker["ready"], worker["terminal"]
        require(ready["pid"] == final["pid"] == worker["pid"]
                and ready["server"] == final["server"] == worker["server"]
                and ready["binary_digest"] == expected["binary"]
                and ready["source_digest"] == expected["fixture"]
                and ready["mode"] == "MRC5"
                and ready["planned_drives"] == ready["registered_drives"] == expected["drives"],
                "worker readiness/terminal identity mismatch")
    output["label"] = spec["label"]
    output["archive_bindings"] = verify_archive(spec, expected, terminal["source"])
    frames, frame_bindings = audit_inventory(owner / "combined-target", terminal, receipt)
    output["all_metric_frame_bindings"] = frame_bindings
    output["oracle_and_expected_file_bindings"] = verify_oracles(owner / "combined-target", terminal, expected["drives"])
    boundaries = {row["sequence"]: row for row in terminal["phase_metrics"]["boundaries"] if "sequence" in row}
    for stage, cell in zip(terminal["stages"], output["cells"]):
        seconds = stage["timing"]["active_elapsed_seconds"]
        for value in stage["timing"].values():
            finite_seconds(value)
        finite_seconds(stage["elapsed_seconds"], positive=True)
        require(stage["elapsed_seconds"] >= stage["timing"]["phase_elapsed_seconds"] >= seconds,
                "elapsed window nesting invalid")
        require(cell["mode"] == stage["mode"] and cell["pattern"] == stage["pattern"], "cell output ordering changed")
        hist = stage["rpc_latency_histogram_log2_microseconds"]
        require(sum(integer(x) for x in hist) == cell["acknowledged_requests"], "RPC histogram/acknowledgment mismatch")
        require(cell["acknowledged_requests"] == cell["cycles"] * (4 if cell["pattern"] == "churn" else 3),
                "complete cycle/RPC scope mismatch")
        cell["exact_active_elapsed_seconds"] = rational(finite_seconds(seconds, positive=True))
        cell["acknowledged_requests_per_second"] = rate(cell["acknowledged_requests"], seconds)
        cell["complete_cycles_per_second"] = rate(cell["cycles"], seconds)
        cell["rpc_latency_histogram_log2_us"] = hist
        cell["rpc_latency_bounds"] = histogram_bounds(hist, rpc=True)
        rows = [boundaries[seq] for seq in stage["metric_sequences"][:2]]
        pair = [frames[row["controller"]["file"]] for row in rows]
        before, after = pair
        read_bytes = delta_value(before["observer"]["expected_compare"]["bytes"], after["observer"]["expected_compare"]["bytes"])
        prepared_bytes = delta_value(before["observer"]["byte_preparation"]["bytes"], after["observer"]["byte_preparation"]["bytes"])
        for category in ("expected_compare", "byte_preparation"):
            a, b = before["observer"][category], after["observer"][category]
            require(a["in_flight"] == b["in_flight"] == 0 and delta_value(a["error"], b["error"]) == 0
                    and delta_value(a["cancelled"], b["cancelled"]) == 0
                    and delta_value(a["calls"], b["calls"]) == delta_value(a["success"], b["success"]),
                    "payload observer outcomes incomplete")
        require(prepared_bytes >= read_bytes and read_bytes % 4096 == prepared_bytes % 4096 == 0,
                "logical payload accounting invalid")
        if cell["pattern"] == "churn":
            require(prepared_bytes == read_bytes == 0, "churn has unexpected payload bytes")
        elif cell["pattern"] in ("sequential_read", "random_read"):
            require(prepared_bytes == read_bytes == cell["cycles"] * 4096, "read cycle payload mismatch")
        elif cell["pattern"] == "append_truncate":
            require(read_bytes == 0 and prepared_bytes <= cell["cycles"] * 4096, "append cycle payload mismatch")
        else:
            require(prepared_bytes == cell["cycles"] * 4096, "I/O cycle payload mismatch")
            if cell["pattern"] in ("sequential_overwrite", "random_overwrite"):
                require(read_bytes == 0, "overwrite has unexpected read payload")
        cell["logical_payload"] = {"read_bytes": read_bytes, "write_bytes": prepared_bytes - read_bytes,
                                   "read_bytes_per_second": rate(read_bytes, seconds),
                                   "write_bytes_per_second": rate(prepared_bytes - read_bytes, seconds),
                                   "payload_request_bytes": 4096}
        cdelta = checked_delta(before, after)
        cell["controller"]["process_detail"] = process_detail(before, after, cdelta)
        cell["controller"]["object_store_observation"] = http_delta(before, after)
        cell["workers"]["per_process"] = []
        selected_storage, selected_core = [], []
        for server in range(10):
            pair = []
            for row in rows:
                refs = [r for r in row["workers"] if r["server"] == server]
                require(len(refs) == 1, "worker active boundary missing")
                pair.append(frames[refs[0]["file"]])
            a, b = pair
            d = checked_delta(a, b)
            verify_runtime_delta(a, b, d["runtime_activation"])
            storage = selected_entries(d, "storage", STORAGE_PREFIXES)
            core = selected_entries(d, "core", CORE_PREFIXES)
            selected_storage.append(storage)
            selected_core.append(core)
            cell["workers"]["per_process"].append({"server": server, "process": process_detail(a, b, d),
                                                    "allocations": allocation_summary([d]),
                                                    "storage": [{k: r[k] for k in ("name",) + COUNTERS} for r in storage], "core": core,
                                                    "object_store_observation": http_delta(a, b),
                                                    "quic": {k: integer(d["server_transport"]["observed_total"][k]) for k in QUIC}})
        cell["workers"]["storage"] = aggregate_entries(selected_storage)
        cell["workers"]["per_process_storage_scope"] = "exact per-process counters; latency histograms/bounds retained in cluster storage and hash-bound raw frames"
        cell["workers"]["core"] = aggregate_entries(selected_core)
        sql = cell["workers"]["sql"]
        cell["workers"]["exact_sql_calls_per_cycle"] = rational(Fraction(sum(r["calls"] for r in sql), cell["cycles"]))
        cell["workers"]["exact_sql_rows_per_cycle"] = rational(Fraction(sum(r["returned_rows"] for r in sql), cell["cycles"]))
        # RPC and provider measurements never become physical IOPS.
    return output


DENOMINATORS = {
    "throughput": "all active clients together; complete cycles and acknowledged RPCs divided by drained active workload duration; open/close included in RPCs",
    "logical_payload": "4 KiB active read comparisons and read/write preparation; excludes truncation bytes and untimed setup/oracles",
    "counters": "before_active to after_active process-local observations; boundary observers/background included; not a global atomic cut",
    "sql": "classified client submissions; successful known-count SELECT returned rows only; no affected/scanned rows, server execution or TiKV attribution",
    "pool": "pool checkout await includes possible lazy connection/session setup; queue-only wait unavailable",
    "latency": "32 log2 microsecond buckets; percentile bounds only; final bucket censored; RPC sub-1us values land in bucket1; no complete-cycle latency histogram",
    "cpu": "process CPU divided by its own monotonic observation interval; observers/background included; excludes backend and collector processes",
    "rss": "nullable endpoints and process lifetime peaks; no per-cell or simultaneous fleet peak; live Rust bytes remain endpoint gauges",
    "allocations": "process-local System Rust allocator deltas; includes observers/background and excludes foreign C allocators; instrumented builds affect throughput, and filesystem omits object-store observer frames; allocations and reallocations are separate counters; live bytes are endpoint gauges; per-cycle quotients do not isolate metadata or allocation-free paths",
    "quic": "worker active-boundary UDP payload counters exclude headers; client retained boundary also includes idle liveness; frames/datagrams/API calls differ",
    "http": "filesystem blobs have no HTTP/object-store transport; not_configured with null counters rather than measured zeros",
    "cache": "distributed RAM/disk/peer cache and generic object-store adapter absent; kernel filesystem caching is present but not isolated or measured",
    "physical_io": "no new RustFS container/cgroup; no qualified host physical device or TiKV physical IOPS; own-process OS accounting is separate and cannot establish physical IOPS",
    "comparison": "one qualified filesystem run; optional 9d RustFS observations are historical and unpaired. HTTP/Docker VM traversal/RustFS processing/adapter cache change together; run order/cache/kernel/background state and source changes confound causal attribution; no confidence interval or isolated RustFS latency estimate",
    "production": "local loopback depth-one native clients; 10 Drives is not 10000-client production, OS mounting or cross-host/OIDC qualification",
}


def table_rows(runs):
    result = []
    for run in runs:
        for cell in run["cells"]:
            payload, workers = cell["logical_payload"], cell["workers"]
            sql_calls = sum(r["calls"] for r in workers["sql"])
            sql_rows = sum(r["returned_rows"] for r in workers["sql"])
            storage = {r["name"]: r for r in workers["storage"]}
            rpc = {"run": run["label"], "source_revision": run["source_revision"],
                           "mode": cell["mode"], "pattern": cell["pattern"], "active_clients": cell["active_clients"],
                           "complete_cycles": cell["cycles"], "acknowledged_requests": cell["acknowledged_requests"],
                           "complete_cycles_per_second": cell["complete_cycles_per_second"],
                           "acknowledged_requests_per_second": cell["acknowledged_requests_per_second"],
                           "read_bytes": payload["read_bytes"], "write_bytes": payload["write_bytes"],
                           "read_bytes_per_second": payload["read_bytes_per_second"],
                           "write_bytes_per_second": payload["write_bytes_per_second"],
                           "rpc_latency_bounds": cell["rpc_latency_bounds"], "worker_sql_calls": sql_calls,
                           "worker_sql_returned_rows": sql_rows,
                           "worker_sql_calls_per_cycle": workers["exact_sql_calls_per_cycle"],
                           "worker_sql_rows_per_cycle": workers["exact_sql_rows_per_cycle"],
                           "observed_worker_http_dispatches": None,
                           "worker_cpu_us": workers["process"]["cpu_user_us"] + workers["process"]["cpu_system_us"],
                           "controller_cpu_us": cell["controller"]["process"]["cpu_user_us"] + cell["controller"]["process"]["cpu_system_us"]}
            for role, allocation in (("worker", workers["allocations"]),
                                     ("controller", cell["controller"]["allocations"])):
                rpc[role + "_allocations"] = allocation
                rpc[role + "_allocation_counters_per_cycle"] = {
                    key: rational(Fraction(integer(allocation["counters"][key]), cell["cycles"]))
                    for key in ALLOCATION_COUNTERS}
                requests = allocation["counters"]["rust_allocations"] + allocation["counters"]["rust_reallocations"]
                rpc[role + "_allocation_requests_per_cycle"] = rational(Fraction(requests, cell["cycles"]))
            for suffix in ("checkout",):
                row = storage["tidb.pool." + suffix]
                rpc["pool_" + suffix] = {k: row[k] for k in ("calls", "elapsed_ns", "latency_bounds")}
            for suffix in ("begin.metadata", "begin.inode", "begin.compact_read", "commit", "rollback"):
                row = storage["tidb.tx." + suffix]
                rpc["transaction_" + suffix.replace(".", "_")] = {k: row[k] for k in ("calls", "elapsed_ns", "latency_bounds")}
            for suffix in ("get", "put", "flush", "verify_concurrent_backing"):
                row = storage["sdk.blocks." + suffix]
                rpc["sdk_blocks_" + suffix] = {k: row[k] for k in ("calls", "bytes", "elapsed_ns", "latency_bounds")}
            rpc["worker_quic_udp"] = workers["quic"]
            rpc["worker_http_by_method"] = None
            rpc["generic_adapter_cache_cluster_endpoints"] = None
            rpc["object_store_http_status"] = "not_configured"
            rpc["distributed_blob_cache_configured"] = False
            result.append(rpc)
    return result


def verify_filesystem_qualification(spec, expected):
    owner = Path(spec["owner_root"]).resolve()
    receipt, _ = read_json(owner / "receipt.json", expected["receipt"])
    terminal, _ = read_json(owner / "combined-target/terminal.json", expected["terminal"])
    preflight = terminal["filesystem_preflight"]
    require(terminal.get("rustfs_preflight") is None and preflight["provider"] == "filesystem"
            and preflight["qualified"] is preflight["complete"] is True
            and preflight["owner"] == receipt["run"]
            and all(preflight[k] is True for k in ("root_identity_verified", "root_identity_redacted",
                "no_symlink_components", "anchored_directory_descriptors", "durable"))
            and preflight["private_directory_mode"] == "0700" and preflight["owner_marker_mode"] == "0600",
            "filesystem native ownership preflight incomplete")
    fs = preflight["persistent_local_filesystem"]
    require(fs["local"] is True and fs["source"] in ("darwin_fstatfs", "linux_fstatfs")
            and fs["persistent_type"] in ("apfs", "hfs", "ext", "xfs", "btrfs", "f2fs", "zfs"),
            "filesystem persistent local type unqualified")
    identity = receipt["filesystem_identity_before"]
    require(identity == receipt["filesystem_identity_at_dispatch"]
            == receipt["filesystem_identity_after_benchmark"] == receipt["filesystem_identity_after_settlement"]
            and identity["owner"] == receipt["run"] and identity["retained"] is True
            and identity["mode"] == "0700" and identity["marker_mode"] == "0600",
            "filesystem owner retained identity changed")
    root = Path(receipt["filesystem_root"])
    require(root.is_absolute() and root.parent == owner.parent
            and root.name == receipt["run"] and re.fullmatch(r"mount-rs-filesystem-[0-9a-f]{24}", root.name),
            "filesystem retained root path identity invalid")
    require(set(identity["root"]) == {"dev", "ino", "uid"}, "filesystem root identity shape changed")
    numbers = {key: decimal_counter(value) for key, value in identity["root"].items()}
    require(numbers["dev"] > 0 and numbers["ino"] > 0 and numbers["uid"] == os.geteuid(),
            "filesystem root device/inode/owner invalid")
    info = root.lstat()
    require(stat.S_ISDIR(info.st_mode) and stat.S_IMODE(info.st_mode) == 0o700
            and (info.st_dev, info.st_ino, info.st_uid) == (numbers["dev"], numbers["ino"], numbers["uid"]),
            "filesystem retained root no longer matches receipt")
    marker, marker_sha = read_json(root / "owner.json", sha_shape(identity["marker_sha256"]))
    require(marker == {"schema": "mount-rs-filesystem-owner-v1", "owner": receipt["run"], "root": identity["root"]}
            and (root / "owner.json").lstat().st_mode & 0o7777 == 0o600,
            "filesystem retained marker differs from owner proof")
    for key in ("before", "after"):
        require(isinstance(receipt["original_health_" + key], str), "original fixture health locator unavailable")
    supervisor_path = Path(spec["supervisor_receipt_path"]).resolve()
    require(supervisor_path.name == "receipt.json", "unexpected supervisor receipt locator")
    supervisor, supervisor_sha = read_json(supervisor_path, expected["supervisor_receipt"])
    producer_sha, _ = hash_large(Path(spec["supervisor_producer_path"]).resolve(), LIMIT)
    require(producer_sha == supervisor["producer_sha256"] == expected["supervisor"]
            and supervisor["schema"] == "mount-rs.fixed-native-runtime-supervisor.v1"
            and supervisor["head"] == expected["source"] and supervisor["owner_sha256"] == expected["producer"]
            and supervisor["manifest_sha256"] == expected["manifest"]
            and supervisor["binary_sha256"] == expected["binary"]
            and supervisor["status"] == "qualified" and supervisor["performance_eligible"] is True
            and supervisor["exact_identity_reconciliation_required"] is False
            and supervisor["source_pins_before_after_equal"] is True
            and supervisor["automatic_retry"] is False and supervisor["failure_labels"] == []
            and supervisor["primary_failure_type"] is None and supervisor["signals"] == []
            and supervisor["abort_requested"] is supervisor["forced_owner_stop"] is False,
            "filesystem supervisor qualification incomplete")
    require(supervisor["whole_limit_seconds"] == 3300 and supervisor["cleanup_reserve_seconds"] == 120
            and supervisor["final_settlement_within_cleanup_seconds"] == 5
            and supervisor["capture_cap_per_stream_bytes"] == 64 * 1024**2
            and supervisor["expected_exit_code"] == supervisor["exit_code"] == 0
            and supervisor["direct_child_reaped"] is supervisor["direct_owner_group_absent"] is supervisor["pipes_eof"] is True
            and 0 < finite_seconds(supervisor["elapsed_seconds"]) <= 3300,
            "filesystem supervisor limits or settlement changed")
    guard = supervisor["resource_guard"]
    require(guard["host_free_floor_bytes"] == 64 * 1024**3 and guard["requested_interval_ms"] == 100
            and integer(guard["minimum_host_free_bytes"]) >= guard["host_free_floor_bytes"]
            and integer(guard["samples"]) > 0 and finite_seconds(guard["actual_max_gap_ms"], positive=True) > 0,
            "filesystem supervisor host floor qualification incomplete")
    proof = supervisor["owner_receipt"]
    require(Path(proof["path"]).resolve() == owner / "receipt.json" and proof["sha256"] == expected["receipt"]
            and Path(proof["terminal_path"]).resolve() == owner / "combined-target/terminal.json"
            and proof["terminal_sha256"] == expected["terminal"] and proof["run"] == receipt["run"]
            and proof["filesystem_data_retained"] is proof["root_identity_verified"] is proof["owner_reported_cleanup_complete"] is True,
            "filesystem supervisor does not bind owner/terminal/root settlement")
    require(receipt["physical_io"]["available"] is False,
            "filesystem owner fabricates qualified physical I/O")
    return {"supervisor_receipt_sha256": supervisor_sha, "supervisor_producer_sha256": producer_sha,
            "status": "qualified", "performance_eligible": True, "owner_and_terminal_bound": True,
            "root_identity_verified": True, "data_retained": True, "marker_sha256": marker_sha,
            "filesystem_type": fs["persistent_type"], "filesystem_type_source": fs["source"],
            "owner_and_supervisor_source_pins_unchanged": True,
            "whole_limit_seconds": 3300, "cleanup_reserve_seconds": 120,
            "host_free_floor_bytes": 64 * 1024**3,
            "minimum_host_free_bytes": guard["minimum_host_free_bytes"],
            "resource_samples": guard["samples"], "actual_max_sample_gap_ms": guard["actual_max_gap_ms"],
            "scope": "qualified local retained filesystem fixture; no cross-host availability or crash/power-loss proof"}


def historical_summary(spec):
    require(set(spec) == {"path", "sha256"} and spec["sha256"] == HISTORICAL_SHA,
            "historical reference must be exact qualified paired public artifact")
    historical, sha = read_json(Path(spec["path"]).resolve(), HISTORICAL_SHA)
    require(historical["schema"] == "mount-rs-native-public-observations-v2"
            and len(historical["runs"]) == 2, "historical RustFS public artifact schema mismatch")
    candidates = [run for run in historical["runs"] if run["source_revision"] == HISTORICAL_SOURCE]
    require(len(candidates) == 1, "historical 9d run missing or duplicate")
    run = candidates[0]
    require(run["binary_sha256"] == HISTORICAL_BINARY
            and run["geometry"] == {"servers": 10, "clients": 10, "drives": 10, "partitions": 5,
                "files_per_drive": 1000, "seconds_per_cell": 5, "configured_partitions_per_server": 5,
                "construction_mode": "lazy", "metadata_provider": "tidb", "block_provider": "rustfs"}
            and run["coverage"]["allocation_profiling"] is True
            and run["correctness"]["workers_clean_reaped"] is True
            and run["correctness"]["fresh_oracle_complete"] is True
            and run["correctness"]["crossnode_completed_pairs"] == 100,
            "historical 9d source/geometry/correctness mismatch")
    cells = run["cells"]
    require(len(cells) == 16 and {(c["mode"], c["pattern"]) for c in cells} == CELLS,
            "historical cell inventory incomplete")
    rows = [row for row in historical["tables"] if row["source_revision"] == HISTORICAL_SOURCE]
    require(len(rows) == 16 and {(r["mode"], r["pattern"]) for r in rows} == CELLS,
            "historical table inventory incomplete")
    return {"classification": "historical_unpaired", "public_artifact_sha256": sha,
            "source_revision": HISTORICAL_SOURCE, "binary_sha256": HISTORICAL_BINARY,
            "geometry": run["geometry"], "tables": rows,
            "scope": "historical qualified 9d RustFS run; not a fresh matched pair or isolated RustFS attribution; source, kernel/adapter caches, run order and background state differ"}


def matched_rustfs_summary(spec, filesystem):
    require(set(spec) == {"path", "sha256", "label"}
            and isinstance(spec["label"], str) and re.fullmatch(r"[a-z0-9_]{1,64}", spec["label"]),
            "matched RustFS reference requires explicit artifact pin and closed label")
    artifact, sha = read_json(Path(spec["path"]).resolve(), sha_shape(spec["sha256"]))
    require(artifact["schema"] == "mount-rs-native-public-observations-v2",
            "matched RustFS public artifact schema mismatch")
    candidates = [run for run in artifact["runs"] if run["label"] == spec["label"]]
    require(len(candidates) == 1, "matched RustFS run missing or duplicate")
    run = candidates[0]
    for key in ("source_revision", "binary_sha256", "archive_manifest_sha256", "protected_fixture_digest"):
        require(run[key] == filesystem[key], "matched RustFS source/build differs from filesystem arm")
    geometry = dict(filesystem["geometry"], block_provider="rustfs")
    require(run["geometry"] == geometry and run["guard_limits"] == filesystem["guard_limits"]
            and run["coverage"]["allocation_profiling"] is True
            and run["coverage"]["metrics_complete"] is True
            and run["coverage"]["coverage_complete"] is False
            and run["correctness"]["workers_clean_reaped"] is True
            and run["correctness"]["fresh_oracle_complete"] is True
            and run["correctness"]["verified_passes"] == 2
            and run["correctness"]["crossnode_completed_pairs"] == 100,
            "matched RustFS workload/profile/correctness differs from filesystem arm")
    require(len(run["cells"]) == 16 and {(c["mode"], c["pattern"]) for c in run["cells"]} == CELLS,
            "matched RustFS cell inventory incomplete")
    rows = [row for row in artifact["tables"] if row["run"] == spec["label"]]
    require(len(rows) == 16 and {(row["mode"], row["pattern"]) for row in rows} == CELLS
            and all(row["source_revision"] == filesystem["source_revision"] for row in rows),
            "matched RustFS table inventory incomplete")
    return {"classification": "same_source_local_pair", "public_artifact_sha256": sha,
            "source_revision": run["source_revision"], "binary_sha256": run["binary_sha256"],
            "owner_receipt_sha256": run["owner_receipt_sha256"], "terminal_sha256": run["terminal_sha256"],
            "geometry": geometry, "tables": rows,
            "scope": "one sequential local pair at the same source/binary/geometry; HTTP/Docker VM traversal/RustFS processing/adapter cache change together; cache evolution/background state/observer work differ; no confidence interval or isolated RustFS attribution"}


def main():
    require(len(sys.argv) == 3, "usage: extractor SPEC_JSON SPEC_SHA256")
    spec, spec_sha = read_json(Path(sys.argv[1]).resolve(), sha_shape(sys.argv[2]))
    require({"schema", "run"} <= set(spec) <= {"schema", "run", "historical_rustfs", "matched_rustfs"}
            and spec["schema"] == "mount-rs-native-filesystem-offline-input-v1",
            "invalid filesystem extraction input inventory")
    item = spec["run"]
    require(set(item) == {"label", "owner_root", "manifest_path", "owner_producer_path",
                          "supervisor_receipt_path", "supervisor_producer_path", "pins"}
            and item["label"] == "filesystem_tidb_d10", "unexpected private input fields")
    expected = item["pins"]
    validate_pins(expected)
    result = extract(Path(item["owner_root"]) / "receipt.json", expected)
    result = enrich_run(item, expected, result)
    result["filesystem_qualification"] = verify_filesystem_qualification(item, expected)
    public = {"schema": "mount-rs-native-filesystem-public-observations-v1", "input_spec_sha256": spec_sha,
              "extractor_sha256": digest(read_file(Path(__file__).resolve())),
              "denominators": dict(DENOMINATORS), "runs": [result], "tables": table_rows([result])}
    if "historical_rustfs" in spec:
        public["historical_comparison"] = historical_summary(spec["historical_rustfs"])
    if "matched_rustfs" in spec:
        public["matched_comparison"] = matched_rustfs_summary(spec["matched_rustfs"], result)
        public["denominators"]["comparison"] = public["matched_comparison"]["scope"]
    # No output is emitted before the run and every referenced artifact qualifies.
    encoded = json.dumps(public, separators=(",", ":"), sort_keys=True, allow_nan=False)
    require(len(encoded.encode("utf-8")) <= 32 * 1024**2, "public report exceeds bounded output")
    sys.stdout.write(encoded + "\n")


if __name__ == "__main__":
    try:
        main()
    except (ExtractionError, ValueError, KeyError, TypeError, StopIteration, OSError, EOFError, zlib.error) as error:
        # Raw input strings, paths and credentials never enter rejection output.
        reason = str(error) if isinstance(error, ExtractionError) else type(error).__name__
        sys.stderr.write("extractor rejected input: " + reason + "\n")
        sys.exit(1)
