#!/usr/bin/env python3
"""Independent bounded offline verification of one paired public report.

Usage: python3 -I -S -B THIS_FILE PUBLIC_JSON SHA256 [--negative-controls]
Only the supplied regular JSON file is read. No runtime/extractor import,
environment access, subprocess, network, fixture operation, or output file.
SQL profile expectations are diagnostics, not new runtime acceptance gates.
Root must review and execute this source; its author has not executed it.
"""
import copy
import hashlib
import json
import math
import os
import re
import stat
import sys
from decimal import Decimal
from fractions import Fraction
from pathlib import PurePosixPath

LIMIT = 32 * 1024 * 1024
U64 = (1 << 64) - 1
SHA = re.compile(r"[0-9a-f]{64}\Z")
REL = re.compile(r"[A-Za-z0-9_./-]+\Z")
NAME = re.compile(r"[A-Za-z0-9_][A-Za-z0-9_.]*\Z")
FRAME = re.compile(r"(?:worker-[0-9]+/)?metrics/(?:g[0-9]+-s[0-9]+|startup-g[0-9]+|closed-g[0-9]+|terminal)\.json\.gz\Z")
EXTRACTOR = "ff2eaee91ac008601a9a868f42a87d2e28d7a36401bbf7fdd4be7df046033524"
FIXTURE = "d6056bc54b40ed1da76e657dd0ea775c45bd187f93395a957a43f606effd8080"
PINS = {
    "paired_be510_d10": {
        "source_revision": "be510e4bd638504306eca37ec63f9379fdb6e8e9",
        "binary_sha256": "f7ab2865a872b577a68ffb31e42610d1e8a4acf67edc9c936eac1498ae4dee8e",
        "archive_manifest_sha256": "235ee8c4822b28918995374ca028f15426bddbefbab3ae2306647fb748272eab",
        "owner_producer_sha256": "7938c771876955fcdd8e5249b94006491c419d6371c430760c952e5c6987e8e2",
    },
    "paired_9d_d10": {
        "source_revision": "9d4d09d51f7e20ae1bf73c548d6d109a98d4e64b",
        "binary_sha256": "bd2ba529f302d47dfa4b7f00d57188ecfd889698eb37f098b35e58fc4cce872c",
        "archive_manifest_sha256": "aec45dc31eeb0d049212d375e1661fef74160ed2353c48e2c58c303214c03b70",
        "owner_producer_sha256": "433066dd9bf281ded72952e3293c7055228b3eba3183117f1f65d4fc508901e6",
    },
}
MODES = ("mostly_idle", "all_active")
PATTERNS = ("sequential_read", "random_read", "sequential_overwrite", "random_overwrite",
            "mixed", "hot_file", "append_truncate", "churn")
CELLS = {(mode, pattern) for mode in MODES for pattern in PATTERNS}
SQL_NAMES = tuple("tidb.sql." + suffix for suffix in (
    "session", "ddl", "metadata_read", "metadata_write", "inode_read", "inode_write",
    "block_read", "block_write", "flush_probe"))
WAIT_NAMES = ("tidb.pool.checkout", "tidb.tx.commit", "tidb.tx.rollback")
BLOCK_NAMES = tuple("sdk.blocks." + suffix for suffix in ("get", "put", "flush", "verify_concurrent_backing"))
COUNTERS = ("calls", "success", "error", "cancelled", "elapsed_ns", "bytes", "returned_rows", "returned_row_observations")
QUIC = ("udp_rx_bytes", "udp_tx_bytes", "udp_rx_datagrams", "udp_tx_datagrams", "udp_rx_ios",
        "udp_tx_ios", "sent_packets", "lost_bytes", "lost_packets", "congestion_events",
        "sent_plpmtud_probes", "lost_plpmtud_probes", "black_holes_detected")
ALLOC = ("rust_allocations", "rust_deallocations", "rust_reallocations", "rust_allocated_bytes", "rust_freed_bytes")
ALLOC_SCOPE = ("optional System Rust allocator atomics; excludes foreign C allocators; "
               "instrumentation affects throughput; live bytes are endpoint gauges")
GAUGES = ("rss_end_bytes", "lifetime_peak_rss_bytes", "sqlite_heap_end_bytes",
          "sqlite_heap_lifetime_peak_bytes", "rust_live_end_bytes")
ROLES = ("primary_data_mixed", "primary_probe_mixed", "qualification_data", "qualification_probe",
         "standalone_data", "standalone_probe")
METHODS = ("get", "head", "put", "delete", "post", "other")
HTTP_COUNTERS = ("attempts_started", "header_responses", "transport_errors", "cancelled_before_headers",
                 "offered_bytes", "offered_known", "offered_unknown", "known_extra_future_boxes",
                 "known_extra_response_body_boxes", "bodies_started", "body_eof", "body_errors",
                 "body_dropped", "body_bytes", "body_chunks", "dispatch_elapsed_ns", "body_elapsed_ns")
HTTP_GAUGES = ("attempts_inflight", "bodies_inflight", "dispatch_max_ns", "body_max_ns")
CACHE = ("created", "released", "live", "resident_entries", "payload_bytes", "unknown_live")
REQUIRED = ("core", "storage", "process", "service_quiescence", "server_quic", "runtime_activation")
UNAVAILABLE = ("sqlite_live_cache_sql", "direct_sdk_raw_object_store", "http_attempts", "physical_iops")
BUDGETS = {"host_free_floor": 64 * 1024**3, "rss_cap_per_owned_process": 24 * 1024**3,
           "phase_seconds": 600, "population_seconds": 600, "request_seconds": 30, "setup_seconds": 600,
           "work_seconds": 1800, "child_cleanup_seconds": 95, "client_cleanup_seconds": 30,
           "expected_receipt_seconds": 30, "oracle_cleanup_seconds": 30}
GEOMETRY = {"servers": 10, "clients": 10, "drives": 10, "partitions": 5, "files_per_drive": 1000,
            "seconds_per_cell": 5, "configured_partitions_per_server": 5, "construction_mode": "lazy",
            "metadata_provider": "tidb", "block_provider": "rustfs"}
DENOMINATOR_KEYS = {"throughput", "logical_payload", "counters", "sql", "pool", "latency", "cpu", "rss",
                    "allocations", "quic", "http", "cache", "physical_io", "comparison", "production"}


class Rejected(Exception):
    """Only fixed source messages are exposed; no raw data or paths."""


def need(condition, message):
    if not condition:
        raise Rejected(message)


def shape(value, keys, message="object shape"):
    need(type(value) is dict and set(value) == set(keys), message)
    return value


def uint(value):
    need(type(value) is int and 0 <= value <= U64, "unsigned counter")
    return value


def sha(value):
    need(type(value) is str and SHA.fullmatch(value) is not None, "digest shape")
    return value


def relative(value):
    need(type(value) is str and REL.fullmatch(value) is not None, "public relative reference")
    path = PurePosixPath(value)
    need(not path.is_absolute() and ".." not in path.parts and str(path) == value, "public relative reference")
    return value


def unique(pairs):
    result = {}
    for key, value in pairs:
        need(key not in result, "duplicate JSON key")
        result[key] = value
    return result


def parse_int(token):
    need(len(token) <= 128, "excessive integer token")
    return int(token)


def parse_decimal(token):
    need(len(token) <= 128, "excessive decimal token")
    value = Decimal(token)
    need(value.is_finite(), "nonfinite JSON number")
    return value


def reject_constant(_token):
    raise Rejected("nonfinite JSON constant")


def read_public(path, expected):
    sha(expected)
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    try:
        before = os.fstat(descriptor)
        need(stat.S_ISREG(before.st_mode) and 0 < before.st_size <= LIMIT, "bounded regular input")
        with os.fdopen(descriptor, "rb", closefd=False) as stream:
            raw = stream.read(LIMIT + 1)
        after = os.fstat(descriptor)
        need(len(raw) == before.st_size <= LIMIT and
             (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns, before.st_ctime_ns) ==
             (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns, after.st_ctime_ns),
             "input changed during bounded read")
    finally:
        os.close(descriptor)
    need(hashlib.sha256(raw).hexdigest() == expected, "public input digest")
    return json.loads(raw, object_pairs_hook=unique, parse_float=parse_decimal,
                      parse_int=parse_int, parse_constant=reject_constant), len(raw)


def number(value, positive=False):
    need(type(value) in (int, Decimal) and (not isinstance(value, Decimal) or value.is_finite()), "finite number")
    result = Fraction(value)
    need(result > 0 if positive else result >= 0, "elapsed or ratio sign")
    return result


def rational(value, expected=None, positive=False):
    shape(value, ("numerator", "denominator", "value"), "rational shape")
    numerator, denominator = value["numerator"], value["denominator"]
    need(type(numerator) is int and 0 <= numerator < 2**256 and
         type(denominator) is int and 0 < denominator < 2**256, "rational integer bounds")
    result = Fraction(numerator, denominator)
    need(result.numerator == numerator and result.denominator == denominator, "rational not reduced")
    approximation = number(value["value"])
    need(math.isfinite(float(result)) and float(approximation) == float(result), "rational display value")
    need(not positive or result > 0, "nonpositive rational")
    if expected is not None:
        need(result == expected, "exact rational arithmetic")
    return result


def endpoints(value, nullable=False):
    shape(value, ("before", "after"), "endpoint shape")
    for endpoint in value.values():
        if endpoint is None:
            need(nullable, "unexpected null endpoint")
        else:
            uint(endpoint)


def allocations(value):
    shape(value, ("available", "status", "counters", "scope"), "allocation shape")
    need(value["available"] is True and value["status"] == "measured" and value["scope"] == ALLOC_SCOPE,
         "allocation profile unavailable or changed")
    shape(value["counters"], ALLOC, "allocation counter inventory")
    return {key: uint(value["counters"][key]) for key in ALLOC}


def histogram(value, calls, bounds=None, rpc=False):
    need(type(value) is list and len(value) == 32, "histogram inventory")
    need(sum(uint(count) for count in value) == calls, "histogram samples")
    if bounds is None:
        return
    shape(bounds, ("samples", "kind", "percentiles"), "latency bounds shape")
    need(uint(bounds["samples"]) == calls and bounds["kind"] == "log2_microsecond_bucket_bounds", "latency bounds scope")
    shape(bounds["percentiles"], ("p50", "p95", "p99"), "latency percentile inventory")
    for label, percentile in (("p50", 50), ("p95", 95), ("p99", 99)):
        reported = bounds["percentiles"][label]
        if calls == 0:
            need(reported is None, "empty histogram percentile")
            continue
        rank = (calls * percentile + 99) // 100
        cumulative, bucket = 0, None
        for index, count in enumerate(value):
            cumulative += count
            if cumulative >= rank:
                bucket = index
                break
        shape(reported, ("rank", "bucket", "lower_us", "upper_us_exclusive", "censored"), "percentile bound shape")
        need(uint(reported["rank"]) == rank and uint(reported["bucket"]) == bucket and
             uint(reported["lower_us"]) == (0 if bucket == 0 or rpc and bucket == 1 else 2**(bucket - 1)),
             "percentile bucket lower bound")
        upper = reported["upper_us_exclusive"]
        need(upper is None if bucket == 31 else type(upper) is int and upper == 2**bucket,
             "percentile bucket upper bound")
        need(reported["censored"] is (bucket == 31), "percentile censor flag")


def metrics(rows, core=False, hist=False, bounds=False, names=None):
    need(type(rows) is list, "metric array")
    result = {}
    keys = {"name", "calls", "elapsed_ns", "units"} if core else {"name", *COUNTERS}
    if hist:
        keys.add("latency_log2_us")
    if bounds:
        keys.add("latency_bounds")
    for row in rows:
        shape(row, keys, "metric record shape")
        name = row["name"]
        need(type(name) is str and NAME.fullmatch(name) is not None and name not in result, "unique metric label")
        for key in (("calls", "elapsed_ns", "units") if core else COUNTERS):
            uint(row[key])
        if not core:
            need(row["calls"] == row["success"] + row["error"] + row["cancelled"], "metric outcomes")
            if hist:
                histogram(row["latency_log2_us"], row["calls"], row.get("latency_bounds"))
        result[name] = row
    if names is not None:
        need(tuple(result) == tuple(names), "metric named inventory")
    return result


def process_detail(value):
    shape(value, ("cpu_user_us", "cpu_system_us", "observation_elapsed_seconds", "percent_of_one_core",
                  "rss_endpoints_bytes", "lifetime_peak_rss_bytes", "process_gauges", "allocations"), "process detail shape")
    cpu = uint(value["cpu_user_us"]) + uint(value["cpu_system_us"])
    elapsed = rational(value["observation_elapsed_seconds"], positive=True)
    rational(value["percent_of_one_core"], Fraction(cpu, 10000) / elapsed)
    endpoints(value["rss_endpoints_bytes"], nullable=True)
    peak = value["lifetime_peak_rss_bytes"]
    if peak is not None:
        uint(peak)
    shape(value["process_gauges"], GAUGES, "process gauge inventory")
    for key in GAUGES:
        endpoints(value["process_gauges"][key], nullable=key != "rust_live_end_bytes")
    need(value["rss_endpoints_bytes"] == value["process_gauges"]["rss_end_bytes"] and
         peak == value["process_gauges"]["lifetime_peak_rss_bytes"]["after"], "process gauge aliases")
    return allocations(value["allocations"])


def process_totals(summary, details):
    shape(summary, ("cpu_user_us", "cpu_system_us", "rss_endpoints_bytes", "lifetime_peak_rss_bytes", "process_gauges"), "process total shape")
    for key in ("cpu_user_us", "cpu_system_us"):
        need(uint(summary[key]) == sum(detail[key] for detail in details), "CPU process aggregate")
    for key in ("rss_endpoints_bytes", "lifetime_peak_rss_bytes", "process_gauges"):
        need(type(summary[key]) is list and summary[key] == [detail[key] for detail in details], "ordered process gauge aggregate")


def object_store(value):
    shape(value, ("http", "generic_adapter_cache", "generic_adapter_occupancy_complete"), "object-store summary shape")
    need(type(value["http"]) is list and len(value["http"]) == 36, "HTTP inventory")
    rows = {}
    for row in value["http"]:
        shape(row, ("role", "method", "counters", "status", "gauges"), "HTTP row shape")
        identity = row["role"], row["method"]
        need(identity in {(role, method) for role in ROLES for method in METHODS} and identity not in rows, "HTTP identity inventory")
        shape(row["counters"], HTTP_COUNTERS, "HTTP counter inventory")
        for counter in row["counters"].values():
            uint(counter)
        shape(row["gauges"], HTTP_GAUGES, "HTTP gauge inventory")
        for key, endpoint in row["gauges"].items():
            endpoints(endpoint)
            if key in ("attempts_inflight", "bodies_inflight"):
                need(endpoint["before"] == endpoint["after"] == 0, "HTTP boundary in flight")
        counters = row["counters"]
        need(type(row["status"]) is list and len(row["status"]) == 6 and
             sum(uint(count) for count in row["status"]) == counters["header_responses"], "HTTP status aggregate")
        need(counters["attempts_started"] == counters["header_responses"] + counters["transport_errors"] + counters["cancelled_before_headers"] and
             counters["bodies_started"] == counters["header_responses"] == counters["body_eof"] + counters["body_errors"] + counters["body_dropped"],
             "HTTP outcome aggregate")
        rows[identity] = row
    shape(value["generic_adapter_cache"], CACHE, "generic cache inventory")
    for endpoint in value["generic_adapter_cache"].values():
        endpoints(endpoint)
    complete = all(value["generic_adapter_cache"]["unknown_live"][endpoint] == 0 for endpoint in ("before", "after"))
    need(value["generic_adapter_occupancy_complete"] is complete, "generic cache occupancy coverage")
    return rows


def verify_cell(cell):
    shape(cell, ("mode", "pattern", "active_clients", "cycles", "acknowledged_requests", "cycles_per_second",
                 "active_elapsed_seconds", "phase_elapsed_seconds", "overall_phase_elapsed_seconds",
                 "metrics_observer_elapsed_seconds", "idle_liveness_acknowledgments", "metric_sequences",
                 "workers", "controller", "exact_active_elapsed_seconds", "acknowledged_requests_per_second",
                 "complete_cycles_per_second", "rpc_latency_histogram_log2_us", "rpc_latency_bounds", "logical_payload"), "cell shape")
    identity = cell["mode"], cell["pattern"]
    need(identity in CELLS, "cell identity")
    cycles, acks = uint(cell["cycles"]), uint(cell["acknowledged_requests"])
    need(cycles > 0 and acks == cycles * (4 if cell["pattern"] == "churn" else 3), "cycle RPC inventory")
    active = 1 if cell["mode"] == "mostly_idle" else 10
    need(uint(cell["active_clients"]) == active and uint(cell["idle_liveness_acknowledgments"]) == 10 - active, "active idle geometry")
    elapsed = number(cell["active_elapsed_seconds"], positive=True)
    rational(cell["exact_active_elapsed_seconds"], elapsed, positive=True)
    rational(cell["complete_cycles_per_second"], Fraction(cycles) / elapsed)
    rational(cell["acknowledged_requests_per_second"], Fraction(acks) / elapsed)
    need(math.isclose(float(number(cell["cycles_per_second"])), float(Fraction(cycles) / elapsed), rel_tol=1e-12), "legacy displayed cycle rate")
    need(number(cell["overall_phase_elapsed_seconds"], positive=True) >= number(cell["phase_elapsed_seconds"], positive=True) >= elapsed,
         "cell elapsed window nesting")
    number(cell["metrics_observer_elapsed_seconds"])
    sequences = cell["metric_sequences"]
    need(type(sequences) is list and len(sequences) == 3 and all(type(item) is int for item in sequences) and
         sequences == list(range(uint(sequences[0]), sequences[0] + 3)), "cell sequence triple")
    histogram(cell["rpc_latency_histogram_log2_us"], acks, cell["rpc_latency_bounds"], rpc=True)
    payload = shape(cell["logical_payload"], ("read_bytes", "write_bytes", "read_bytes_per_second", "write_bytes_per_second", "payload_request_bytes"), "logical payload shape")
    read, write = uint(payload["read_bytes"]), uint(payload["write_bytes"])
    need(type(payload["payload_request_bytes"]) is int and payload["payload_request_bytes"] == 4096 and read % 4096 == write % 4096 == 0, "logical request bytes")
    rational(payload["read_bytes_per_second"], Fraction(read) / elapsed)
    rational(payload["write_bytes_per_second"], Fraction(write) / elapsed)
    pattern = cell["pattern"]
    if pattern == "churn":
        need(read == write == 0, "churn payload")
    elif pattern in ("sequential_read", "random_read"):
        need(read == cycles * 4096 and write == 0, "read cycle payload")
    elif pattern == "append_truncate":
        need(read == 0 and write <= cycles * 4096, "append cycle payload")
    else:
        need(read + write == cycles * 4096, "I/O cycle payload")
        if pattern in ("sequential_overwrite", "random_overwrite"):
            need(read == 0, "overwrite read bytes")
    worker = shape(cell["workers"], ("process_server_order", "process", "allocations", "sql", "sql_calls_per_cycle",
                                    "sql_rows_per_cycle", "waits", "logical_blocks", "quic", "per_process", "storage",
                                    "per_process_storage_scope", "core", "exact_sql_calls_per_cycle", "exact_sql_rows_per_cycle"), "worker cluster shape")
    need(worker["process_server_order"] == list(range(10)) and all(type(item) is int for item in worker["process_server_order"]), "worker process order")
    need(worker["per_process_storage_scope"] == "exact per-process counters; latency histograms/bounds retained in cluster storage and hash-bound raw frames", "storage scope")
    need(type(worker["per_process"]) is list and len(worker["per_process"]) == 10, "ten worker processes")
    processes, allocated, storages, cores = [], [], [], []
    for server, detail in enumerate(worker["per_process"]):
        shape(detail, ("server", "process", "allocations", "storage", "core", "object_store_observation", "quic"), "worker process shape")
        need(type(detail["server"]) is int and detail["server"] == server, "worker process identity")
        counters = process_detail(detail["process"])
        need(allocations(detail["allocations"]) == counters, "duplicate process allocation join")
        processes.append(detail["process"])
        allocated.append(counters)
        storages.append(metrics(detail["storage"]))
        cores.append(metrics(detail["core"], core=True))
        object_store(detail["object_store_observation"])
        shape(detail["quic"], QUIC, "per-process QUIC inventory")
        for counter in detail["quic"].values():
            uint(counter)
    process_totals(worker["process"], processes)
    aggregate_alloc = allocations(worker["allocations"])
    need(aggregate_alloc == {key: sum(value[key] for value in allocated) for key in ALLOC}, "worker allocation sum")
    storage = metrics(worker["storage"], hist=True, bounds=True)
    core = metrics(worker["core"], core=True)
    for rows, aggregate, keys in ((storages, storage, COUNTERS), (cores, core, ("calls", "elapsed_ns", "units"))):
        need(all(tuple(row) == tuple(aggregate) for row in rows), "per-process metric inventory join")
        for name in aggregate:
            need(all(aggregate[name][key] == sum(row[name][key] for row in rows) for key in keys), "per-process metric scalar sum")
    sql = metrics(worker["sql"], hist=True, names=SQL_NAMES)
    waits = metrics(worker["waits"], hist=True, names=WAIT_NAMES)
    blocks = metrics(worker["logical_blocks"], hist=True, names=BLOCK_NAMES)
    for group in (sql, waits, blocks):
        for name, row in group.items():
            need(name in storage and all(row[key] == storage[name][key] for key in COUNTERS + ("latency_log2_us",)), "classified cluster storage join")
    calls, returned = sum(row["calls"] for row in sql.values()), sum(row["returned_rows"] for row in sql.values())
    rational(worker["exact_sql_calls_per_cycle"], Fraction(calls, cycles))
    rational(worker["exact_sql_rows_per_cycle"], Fraction(returned, cycles))
    for key, total in (("sql_calls_per_cycle", calls), ("sql_rows_per_cycle", returned)):
        need(math.isclose(float(number(worker[key])), float(Fraction(total, cycles)), rel_tol=1e-12), "legacy SQL quotient")
    shape(worker["quic"], QUIC, "cluster QUIC inventory")
    need(all(uint(worker["quic"][key]) == sum(detail["quic"][key] for detail in worker["per_process"]) for key in QUIC), "QUIC process aggregate")
    controller = shape(cell["controller"], ("process", "allocations", "sql", "waits", "logical_blocks", "process_detail", "object_store_observation"), "controller shape")
    controller_alloc = process_detail(controller["process_detail"])
    process_totals(controller["process"], [controller["process_detail"]])
    need(allocations(controller["allocations"]) == controller_alloc, "controller allocation join")
    metrics(controller["sql"], hist=True, names=SQL_NAMES)
    metrics(controller["waits"], hist=True, names=WAIT_NAMES)
    metrics(controller["logical_blocks"], hist=True, names=BLOCK_NAMES)
    object_store(controller["object_store_observation"])
    return sql, storage


def verify_bindings(run):
    archive = shape(run["archive_bindings"], ("manifest_sha256", "source_closure_sha256", "source_count", "source_files",
                                              "fixture_source_bindings", "binary_sha256", "binary_bytes", "native_inventory"), "archive bindings shape")
    need(archive["manifest_sha256"] == run["archive_manifest_sha256"] and archive["binary_sha256"] == run["binary_sha256"] and
         uint(archive["source_count"]) == 637 and 0 < uint(archive["binary_bytes"]) <= 512 * 1024**2, "archive identity join")
    sha(archive["source_closure_sha256"])
    inventory = shape(archive["native_inventory"], ("tests", "benchmarks", "controller_filtered", "list_sha256"), "archive native inventory")
    need(uint(inventory["benchmarks"]) == 0 and uint(inventory["controller_filtered"]) == 177 and uint(inventory["tests"]) > 177, "archive test inventory")
    sha(inventory["list_sha256"])
    need(type(archive["source_files"]) is list and len(archive["source_files"]) == 637, "source file roster")
    sources = {}
    for row in archive["source_files"]:
        shape(row, ("path", "sha256", "bytes"), "source binding shape")
        name = relative(row["path"])
        need(name not in sources and uint(row["bytes"]) <= LIMIT, "source file identity or bounds")
        sources[name] = sha(row["sha256"])
    need(type(archive["fixture_source_bindings"]) is list and archive["fixture_source_bindings"], "fixture binding roster")
    fixture_names = set()
    for row in archive["fixture_source_bindings"]:
        shape(row, ("path", "sha256"), "fixture binding shape")
        name = relative(row["path"])
        need(name not in fixture_names and sources.get(name) == sha(row["sha256"]), "fixture archive source join")
        fixture_names.add(name)
    refs = run["oracle_and_expected_file_bindings"]
    need(type(refs) is list and len(refs) == 12, "oracle and Drive binding roster")
    expected_names = {"expected/drive-" + str(drive) + ".json" for drive in range(10)}
    seen = set()
    corpus = run["correctness"]["fresh_corpus"]
    for ref in refs:
        name = relative(ref["file"])
        need(name not in seen, "duplicate correctness binding")
        seen.add(name)
        sha(ref["sha256"])
        if name in expected_names:
            shape(ref, ("file", "sha256", "drive"), "Drive binding shape")
            need(name == "expected/drive-" + str(uint(ref["drive"])) + ".json" and ref["drive"] < 10, "Drive binding identity")
        else:
            shape(ref, ("file", "sha256", "pass", "files", "bytes"), "oracle binding shape")
            label = ref["pass"]
            need(label in ("initial", "final") and name == "oracle-receipts/" + label + ".json" and
                 uint(ref["files"]) == 10000 and uint(ref["bytes"]) == corpus[label + "_bytes"] and
                 ref["sha256"] == corpus[label + "_receipt_sha256"], "oracle corpus binding join")
    need(seen == expected_names | {"oracle-receipts/initial.json", "oracle-receipts/final.json"}, "complete correctness roster")
    frames = run["all_metric_frame_bindings"]
    need(type(frames) is list and len(frames) >= 352, "full metric frame binding roster")
    by_identity, seen_files = {}, set()
    for frame in frames:
        shape(frame, ("file", "sha256", "role", "server", "generation", "sequence", "phase", "boundary"), "metric frame binding shape")
        name = relative(frame["file"])
        need(FRAME.fullmatch(name) is not None and name not in seen_files, "metric frame path inventory")
        seen_files.add(name)
        role, server = frame["role"], frame["server"]
        need(role in ("controller", "worker") and (server is None if role == "controller" else type(server) is int and 0 <= server < 10), "metric frame role identity")
        need(name.startswith("metrics/") if role == "controller" else name.startswith("worker-" + str(server) + "/"), "metric frame role path join")
        sha(frame["sha256"])
        uint(frame["generation"])
        uint(frame["sequence"])
        identity = frame["sequence"], role, server
        by_identity.setdefault(identity, []).append(frame)
    active = run["active_boundary_sha256"]
    need(type(active) is list and len(active) == 352, "active 16 by 2 by 11 frame roster")
    expected = {(sequence, role, server): cell for cell in run["cells"] for sequence in cell["metric_sequences"][:2]
                for role, servers in (("controller", (None,)), ("worker", range(10))) for server in servers}
    need(len(expected) == 352, "distinct active sequence roster")
    seen_active = set()
    for ref in active:
        shape(ref, ("sequence", "role", "server", "sha256"), "active frame reference shape")
        identity = ref["sequence"], ref["role"], ref["server"]
        need(identity in expected and identity not in seen_active, "active frame identity roster")
        seen_active.add(identity)
        matches = by_identity.get(identity, [])
        need(len(matches) == 1 and matches[0]["sha256"] == sha(ref["sha256"]), "active full frame digest join")
        cell, frame = expected[identity], matches[0]
        need(frame["phase"] == cell["mode"] + "/" + cell["pattern"] and frame["boundary"] ==
             ("before_active" if identity[0] == cell["metric_sequences"][0] else "after_active"), "active cell frame join")
    need(seen_active == set(expected), "complete active frame roster")


def verify_run(run):
    shape(run, ("source_revision", "binary_sha256", "archive_manifest_sha256", "archive_source_count", "owner_producer_sha256",
                "owner_receipt_sha256", "terminal_sha256", "protected_fixture_digest", "protected_fixture_scope", "owner_source_pin_count",
                "owner_source_pins_unchanged", "geometry", "guard_limits", "resources", "correctness", "coverage", "cells",
                "active_boundary_sha256", "rustfs_guest_cgroup", "label", "archive_bindings", "all_metric_frame_bindings",
                "oracle_and_expected_file_bindings"), "run shape")
    label = run["label"]
    need(label in PINS and all(run[key] == value for key, value in PINS[label].items()), "paired build pins")
    for key in ("owner_receipt_sha256", "terminal_sha256"):
        sha(run[key])
    need(run["protected_fixture_digest"] == FIXTURE and run["protected_fixture_scope"] == "benchmark seams, not complete native archive source closure" and
         uint(run["archive_source_count"]) == 637 and uint(run["owner_source_pin_count"]) == 16 and run["owner_source_pins_unchanged"] is True,
         "source qualification scope")
    shape(run["geometry"], GEOMETRY, "geometry field inventory")
    for key, expected in GEOMETRY.items():
        need(type(run["geometry"][key]) is type(expected) and run["geometry"][key] == expected, "fixed paired geometry")
    shape(run["guard_limits"], BUDGETS, "guard inventory")
    need(all(uint(run["guard_limits"][key]) == expected for key, expected in BUDGETS.items()), "unchanged guard budgets")
    resources = shape(run["resources"], ("minimum_observed_host_free_bytes", "controller_peak_rss_bytes", "worker_peak_rss_bytes"), "resource summary shape")
    need(uint(resources["minimum_observed_host_free_bytes"]) >= BUDGETS["host_free_floor"] and
         uint(resources["controller_peak_rss_bytes"]) <= BUDGETS["rss_cap_per_owned_process"] and
         type(resources["worker_peak_rss_bytes"]) is list and len(resources["worker_peak_rss_bytes"]) == 10 and
         all(uint(value) <= BUDGETS["rss_cap_per_owned_process"] for value in resources["worker_peak_rss_bytes"]), "resource guard scope")
    coverage = shape(run["coverage"], ("metrics_complete", "coverage_complete", "required_families", "known_unavailable_families", "allocation_profiling",
                                       "rustfs_server_otlp_extracted", "tikv_cgroup_io_extracted"), "coverage shape")
    need(coverage["metrics_complete"] is True and coverage["coverage_complete"] is False and coverage["allocation_profiling"] is True and
         coverage["rustfs_server_otlp_extracted"] is False and coverage["tikv_cgroup_io_extracted"] is False and
         coverage["required_families"] == list(REQUIRED) and coverage["known_unavailable_families"] == list(UNAVAILABLE), "profile and partial physical coverage")
    correctness = shape(run["correctness"], ("owner_benchmarked", "workload_complete", "fresh_oracle_complete", "workers_clean_reaped", "scope_denials",
                                             "revocation_denials", "verified_passes", "crossnode_completed_pairs", "crossnode_verified_bytes", "fresh_corpus",
                                             "cumulative_requests"), "correctness shape")
    need(all(correctness[key] is True for key in ("owner_benchmarked", "workload_complete", "fresh_oracle_complete", "workers_clean_reaped")), "retained correctness qualification")
    shape(correctness["scope_denials"], ("partition", "sibling"), "scope denial inventory")
    need(all(uint(value) == 10 for value in correctness["scope_denials"].values()) and uint(correctness["revocation_denials"]) == 10 and
         uint(correctness["verified_passes"]) == 2 and uint(correctness["crossnode_completed_pairs"]) == 100 and
         uint(correctness["crossnode_verified_bytes"]) == 100 * 4096, "correctness geometry witnesses")
    corpus = shape(correctness["fresh_corpus"], ("initial_files", "final_files", "initial_bytes", "final_bytes", "initial_receipt_sha256", "final_receipt_sha256"), "fresh corpus shape")
    need(uint(corpus["initial_files"]) == uint(corpus["final_files"]) == 10000, "full paired file corpus")
    uint(corpus["initial_bytes"])
    uint(corpus["final_bytes"])
    sha(corpus["initial_receipt_sha256"])
    sha(corpus["final_receipt_sha256"])
    requests = shape(correctness["cumulative_requests"], ("attempts", "acknowledged", "failed", "uncertain"), "request settlement shape")
    need(uint(requests["attempts"]) == uint(requests["acknowledged"]) and uint(requests["failed"]) == uint(requests["uncertain"]) == 0, "acknowledged request settlement")
    cells = run["cells"]
    need(type(cells) is list and len(cells) == 16 and {(cell["mode"], cell["pattern"]) for cell in cells} == CELLS, "sixteen distinct cells")
    seen_sequences = set()
    for cell in cells:
        verify_cell(cell)
        need(not seen_sequences.intersection(cell["metric_sequences"]), "distinct cell sequence triples")
        seen_sequences.update(cell["metric_sequences"])
    need(requests["acknowledged"] >= sum(cell["acknowledged_requests"] for cell in cells), "timed cumulative request join")
    guest = shape(run["rustfs_guest_cgroup"], ("scope", "interval_ns", "before_sha256", "after_sha256", "devices"), "guest cgroup shape")
    need(guest["scope"] == "new RustFS container guest cgroup; whole native benchmark window includes setup, oracle, background and writeback; not physical IOPS or TiKV" and
         uint(guest["interval_ns"]) > 0, "whole-run guest physical coverage scope")
    sha(guest["before_sha256"])
    sha(guest["after_sha256"])
    need(type(guest["devices"]) is list and guest["devices"], "guest device roster")
    devices = set()
    for row in guest["devices"]:
        shape(row, ("device", "delta"), "guest device shape")
        need(type(row["device"]) is str and re.fullmatch(r"[0-9]+:[0-9]+", row["device"]) is not None and row["device"] not in devices, "guest device identity")
        devices.add(row["device"])
        shape(row["delta"], ("rbytes", "wbytes", "rios", "wios", "dbytes", "dios"), "guest counter inventory")
        for value in row["delta"].values():
            uint(value)
    verify_bindings(run)


def table_join(table, run, cell):
    worker, controller, payload = cell["workers"], cell["controller"], cell["logical_payload"]
    cycles = cell["cycles"]
    storage = {row["name"]: row for row in worker["storage"]}
    expected = {
        "run": run["label"], "source_revision": run["source_revision"], "mode": cell["mode"], "pattern": cell["pattern"],
        "active_clients": cell["active_clients"], "complete_cycles": cycles, "acknowledged_requests": cell["acknowledged_requests"],
        "complete_cycles_per_second": cell["complete_cycles_per_second"], "acknowledged_requests_per_second": cell["acknowledged_requests_per_second"],
        "read_bytes": payload["read_bytes"], "write_bytes": payload["write_bytes"], "read_bytes_per_second": payload["read_bytes_per_second"],
        "write_bytes_per_second": payload["write_bytes_per_second"], "rpc_latency_bounds": cell["rpc_latency_bounds"],
        "worker_sql_calls": sum(row["calls"] for row in worker["sql"]), "worker_sql_returned_rows": sum(row["returned_rows"] for row in worker["sql"]),
        "worker_sql_calls_per_cycle": worker["exact_sql_calls_per_cycle"], "worker_sql_rows_per_cycle": worker["exact_sql_rows_per_cycle"],
        "observed_worker_http_dispatches": sum(row["counters"]["attempts_started"] for process in worker["per_process"] for row in process["object_store_observation"]["http"]),
        "worker_cpu_us": worker["process"]["cpu_user_us"] + worker["process"]["cpu_system_us"],
        "controller_cpu_us": controller["process"]["cpu_user_us"] + controller["process"]["cpu_system_us"],
        "worker_allocations": worker["allocations"], "controller_allocations": controller["allocations"],
        "worker_quic_udp": worker["quic"], "distributed_blob_cache_configured": False,
    }
    for role in ("worker", "controller"):
        alloc = allocations(table[role + "_allocations"])
        expected[role + "_allocation_counters_per_cycle"] = table[role + "_allocation_counters_per_cycle"]
        shape(expected[role + "_allocation_counters_per_cycle"], ALLOC, "table allocation quotient inventory")
        for key in ALLOC:
            rational(expected[role + "_allocation_counters_per_cycle"][key], Fraction(alloc[key], cycles))
        expected[role + "_allocation_requests_per_cycle"] = table[role + "_allocation_requests_per_cycle"]
        rational(expected[role + "_allocation_requests_per_cycle"], Fraction(alloc["rust_allocations"] + alloc["rust_reallocations"], cycles))
    for output, source, keys in (
        ("pool_checkout", "tidb.pool.checkout", ("calls", "elapsed_ns", "latency_bounds")),
        *(("transaction_" + suffix.replace(".", "_"), "tidb.tx." + suffix, ("calls", "elapsed_ns", "latency_bounds"))
          for suffix in ("begin.metadata", "begin.inode", "begin.compact_read", "commit", "rollback")),
        *(("sdk_blocks_" + suffix, "sdk.blocks." + suffix, ("calls", "bytes", "elapsed_ns", "latency_bounds"))
          for suffix in ("get", "put", "flush", "verify_concurrent_backing")),
    ):
        need(source in storage, "table storage metric join inventory")
        expected[output] = {key: storage[source][key] for key in keys}
    selected_http = ("attempts_started", "header_responses", "offered_bytes", "body_bytes", "transport_errors",
                     "cancelled_before_headers", "body_eof", "body_errors", "body_dropped")
    expected["worker_http_by_method"] = []
    for method in METHODS:
        rows = [row for process in worker["per_process"] for row in process["object_store_observation"]["http"] if row["method"] == method]
        expected["worker_http_by_method"].append({"method": method, **{key: sum(row["counters"][key] for row in rows) for key in selected_http}})
    expected["generic_adapter_cache_cluster_endpoints"] = {
        key: {endpoint: sum(process["object_store_observation"]["generic_adapter_cache"][key][endpoint]
                            for process in worker["per_process"]) for endpoint in ("before", "after")}
        for key in ("live", "resident_entries", "payload_bytes", "unknown_live")}
    shape(table, expected, "table field inventory")
    need(table["distributed_blob_cache_configured"] is False, "distributed cache control profile")
    need(table == expected, "table observation join")


def diagnostics(public):
    """Observed expected profiles are informational; arithmetic is enforced above."""
    result = []
    for run in public["runs"]:
        for cell in run["cells"]:
            if cell["pattern"] not in ("sequential_read", "random_read", "sequential_overwrite", "random_overwrite"):
                continue
            sql = {row["name"]: row for row in cell["workers"]["sql"]}
            cycles = cell["cycles"]
            observed = (Fraction(sum(row["calls"] for row in sql.values()), cycles),
                        Fraction(sum(row["returned_rows"] for row in sql.values()), cycles),
                        Fraction(sql["tidb.sql.session"]["calls"], cycles),
                        Fraction(next(row["calls"] for row in cell["workers"]["logical_blocks"] if row["name"] == "sdk.blocks.put"), cycles))
            if cell["pattern"] in ("sequential_read", "random_read"):
                expected = (Fraction(3), Fraction(3), Fraction(0), Fraction(0))
            elif run["label"] == "paired_be510_d10":
                expected = (Fraction(6), Fraction(5), Fraction(1), Fraction(1))
            else:
                expected = (Fraction(5), Fraction(4), Fraction(0), Fraction(1))
            if observed != expected:
                result.append({"run": run["label"], "mode": cell["mode"], "pattern": cell["pattern"],
                               "observed_sql_calls_rows_session_put_per_cycle": [str(value) for value in observed]})
    return result


def verify(public):
    shape(public, ("schema", "input_spec_sha256", "extractor_sha256", "denominators", "runs", "tables"), "public report shape")
    need(public["schema"] == "mount-rs-native-public-observations-v2" and public["extractor_sha256"] == EXTRACTOR, "public producer identity")
    sha(public["input_spec_sha256"])
    shape(public["denominators"], DENOMINATOR_KEYS, "denominator inventory")
    need(all(type(value) is str and 0 < len(value) < 2048 for value in public["denominators"].values()), "bounded denominator text")
    for key, phrases in {
        "allocations": ("both builds instrumented", "excludes foreign C allocators", "per-cycle quotients do not isolate metadata"),
        "cache": ("distributed RAM/disk/peer cache absent", "unknown occupancy stays incomplete"),
        "physical_io": ("whole-run accounting", "no contemporaneous TiKV or host physical flash IOPS"),
        "comparison": ("sequential run order", "one pair has no confidence intervals"),
        "production": ("10 Drives is not 10000-client production", "local loopback depth-one native clients"),
    }.items():
        need(all(phrase in public["denominators"][key] for phrase in phrases), "retained measurement limitations")
    need(type(public["runs"]) is list and len(public["runs"]) == 2 and
         {run["label"] for run in public["runs"]} == set(PINS), "exact paired run labels")
    cells = {}
    for run in public["runs"]:
        verify_run(run)
        for cell in run["cells"]:
            identity = run["label"], cell["mode"], cell["pattern"]
            need(identity not in cells, "distinct paired cells")
            cells[identity] = run, cell
    need(len(cells) == 32 and type(public["tables"]) is list and len(public["tables"]) == 32, "32-cell public table inventory")
    seen = set()
    for row in public["tables"]:
        identity = row["run"], row["mode"], row["pattern"]
        need(identity in cells and identity not in seen, "distinct table cell identity")
        seen.add(identity)
        table_join(row, *cells[identity])
    need(seen == set(cells), "complete table joins")
    return diagnostics(public)


def negative_controls(public):
    """Three tiny data-only corruptions; no import of the benchmark runtime."""
    controls = (
        ("missing_cell", lambda value: value["runs"][0]["cells"].pop()),
        ("table_allocation_tamper", lambda value: value["tables"][0]["worker_allocations"]["counters"].__setitem__(
            "rust_allocations", value["tables"][0]["worker_allocations"]["counters"]["rust_allocations"] + 1)),
        ("allocation_profile_flag", lambda value: value["runs"][0]["coverage"].__setitem__("allocation_profiling", False)),
    )
    passed = []
    for name, mutate in controls:
        damaged = copy.deepcopy(public)
        mutate(damaged)
        try:
            verify(damaged)
        except Rejected:
            passed.append(name)
        else:
            raise Rejected("negative control accepted corrupted report")
        del damaged
    return passed


def main():
    need(len(sys.argv) in (3, 4) and (len(sys.argv) == 3 or sys.argv[3] == "--negative-controls"), "usage argument inventory")
    public, size = read_public(sys.argv[1], sys.argv[2])
    unexpected = verify(public)
    controls = negative_controls(public) if len(sys.argv) == 4 else []
    metadata = {"status": "PASS", "schema": "mount-rs-paired-public-verification-v1", "bytes": size,
                "public_sha256": sys.argv[2], "runs": 2, "cells": 32, "worker_processes_per_cell": 10,
                "allocation_counters_per_process": 5, "histogram_percentiles_checked": [50, 95, 99],
                "negative_controls_rejected": controls, "unexpected_sql_profiles": unexpected,
                "physical_io_coverage": "partial_whole_run_RustFS_guest_only", "distributed_cache_configured": False,
                "allocation_profiling": True, "runtime_or_extractor_imported": False,
                "runtime_execution_performed": False, "guard_update_unit_attribution": "not_exported_as_guard_specific_SQL_unit"}
    sys.stdout.write(json.dumps(metadata, sort_keys=True, separators=(",", ":"), allow_nan=False) + "\n")


if __name__ == "__main__":
    try:
        main()
    except (Rejected, ValueError, TypeError, KeyError, IndexError, OverflowError, OSError, RecursionError) as error:
        reason = str(error) if isinstance(error, Rejected) else type(error).__name__
        sys.stderr.write("public verifier rejected input: " + reason + "\n")
        sys.exit(1)
