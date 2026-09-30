#!/usr/bin/env python3
"""Independent read-only grant benchmark arithmetic audit; root executes it.

Usage: python3 -B verify-summary.py OBS SUMMARY OBS_SHA256 SUMMARY_SHA256
No source import, network, subprocess, benchmark execution, or file mutation.
This audits published projections, not the private process/binary/source receipt.
"""
import hashlib
import json
import math
import os
import re
import stat
import sys
from fractions import Fraction

MAX_FILE_BYTES = 2 * 1024 * 1024
EXPECTED_GRANTS = (10, 100, 1000, 10000)
MODES = ("full", "indexed")
CHECKS = 0


def require(condition, label):
    global CHECKS
    CHECKS += 1
    if not condition:
        raise ValueError(label)


def closed(record, keys, label):
    require(type(record) is dict and set(record) == set(keys), label + " keys")


def integer(value, label, minimum=0):
    require(type(value) is int and minimum <= value <= (1 << 64) - 1, label)
    return value


def exact(value, expected, label):
    require(type(value) is type(expected) and value == expected, label)


def digest(value, label):
    require(type(value) is str and re.fullmatch(r"[0-9a-f]{64}", value) is not None, label)


def no_duplicates(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, "duplicate JSON key")
        result[key] = value
    return result


def read_json(path, expected_sha):
    digest(expected_sha, "expected digest")
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    try:
        before = os.fstat(fd)
        require(stat.S_ISREG(before.st_mode), "regular input required")
        require(0 < before.st_size <= MAX_FILE_BYTES, "input bound")
        chunks = []
        total = 0
        while True:
            block = os.read(fd, min(65536, MAX_FILE_BYTES + 1 - total))
            if not block:
                break
            chunks.append(block)
            total += len(block)
            require(total <= MAX_FILE_BYTES, "input grew beyond bound")
        after = os.fstat(fd)
        require((before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns) ==
                (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns), "input changed")
        raw = b"".join(chunks)
        require(len(raw) == before.st_size, "input length")
        require(hashlib.sha256(raw).hexdigest() == expected_sha, "input digest mismatch")
        return json.loads(raw.decode("utf-8"), object_pairs_hook=no_duplicates,
                          parse_constant=lambda _: (_ for _ in ()).throw(ValueError("nonfinite JSON")))
    finally:
        os.close(fd)


def median(values):
    ordered = sorted(values)
    require(len(ordered) == 8, "eight-pair statistic")
    return (ordered[3] + ordered[4]) / 2


def close_numeric(value, expected, label):
    require(type(value) in (int, float) and math.isfinite(value), label + " finite number")
    wanted = float(expected)
    # Exact rational recomputation may round at a different stage than the
    # producer's floating division/median; permit at most two binary64 ULPs.
    require(abs(float(value) - wanted) <= max(2 * math.ulp(wanted), 1e-12), label)


def audit_length(index):
    # Independent record construction fixes complete field order and LF, then
    # derives expected aggregate bytes; it does not use the measured counter.
    record = {"event": "remote_access", "partition_id": f"p{index // 2:05}",
              "drive_id": f"d{index:05}", "grant_ids": [f"g{index:05}"],
              "operation": "handle_write", "request_id": 42, "outcome": "ok"}
    return len((json.dumps(record, separators=(",", ":"), ensure_ascii=False) + "\n").encode("utf-8"))


def expected_bytes(cycles, positions):
    require(cycles % 3 == 0, "balanced query cycles")
    return (cycles // 3) * sum(audit_length(index) for index in positions)


def native(record, cycles, positions, label):
    closed(record, ("cycles", "audit_bytes", "elapsed_ns", "process_user_us", "process_system_us"), label)
    for key, value in record.items():
        integer(value, label + "/" + key)
    exact(record["cycles"], cycles, label + " cycles")
    exact(record["audit_bytes"], expected_bytes(cycles, positions), label + " independently derived audit bytes")
    require(record["elapsed_ns"] > 0, label + " positive elapsed")
    return (Fraction(record["elapsed_ns"], cycles),
            Fraction(1000 * (record["process_user_us"] + record["process_system_us"]), cycles))


def allocation(record, cycles, positions, label):
    closed(record, ("cycles", "audit_bytes", "rust_alloc_calls", "rust_alloc_requested_bytes"), label)
    for key, value in record.items():
        integer(value, label + "/" + key)
    exact(record["cycles"], cycles, label + " cycles")
    exact(record["audit_bytes"], expected_bytes(cycles, positions), label + " audit bytes")
    exact(record["rust_alloc_calls"], 0, label + " zero allocation calls")
    exact(record["rust_alloc_requested_bytes"], 0, label + " zero requested bytes")


def cold(record, label):
    closed(record, ("preparations", "elapsed_ns", "process_user_us", "process_system_us",
                    "rust_alloc_calls", "rust_alloc_requested_bytes", "scope"), label)
    for key in set(record) - {"scope"}:
        integer(record[key], label + "/" + key)
    exact(record["preparations"], 1, label + " one cold preparation")
    require(record["elapsed_ns"] > 0, label + " elapsed")
    exact(record["scope"], "cold prepare includes current-thread allocation-meter overhead; authority construction/validation and later index drop excluded", label + " scope")


def main():
    require(len(sys.argv) == 5, "expected OBS SUMMARY OBS_SHA SUMMARY_SHA")
    observation_sha, summary_sha = sys.argv[3:]
    observations = read_json(sys.argv[1], observation_sha)
    summary = read_json(sys.argv[2], summary_sha)
    closed(observations, ("schema", "complete", "elapsed_ns", "release", "pairs_per_scale",
                         "timed_operations", "cpu_scope", "allocation_scope", "cfg_test_scope",
                         "exclusions", "oracles", "scales"), "observations")
    exact(observations["schema"], "mount-rs.grant-index-paired.v1", "observation schema")
    exact(observations["complete"], True, "complete")
    exact(observations["release"], True, "release")
    exact(observations["pairs_per_scale"], 8, "pairs per scale")
    require(0 < integer(observations["elapsed_ns"], "benchmark elapsed") < 55_000_000_000, "internal cooperative budget at final emission")
    exact(observations["cpu_scope"], "getrusage RUSAGE_SELF deltas; whole process, synchronous single-test execution", "CPU scope")
    exact(observations["allocation_scope"], "existing current-thread System allocation/reallocation calls and requested bytes; separate synchronous windows; no deallocation/live/RSS or foreign allocator coverage", "allocation scope")
    exact(observations["cfg_test_scope"], "indexed audit retains two TLS candidate-visit observations/cycle; full reference has none, avoiding artificial full-map TLS cost; cold preparation includes test hook and allocation-meter overhead", "test instrumentation scope")
    exact(observations["timed_operations"], "policy matching, source/cache Arc selection, public or prepared authorization, writer mutex, buffered audit, exact permission/bytes checks, periodic budget checks", "timed operations")
    exact(observations["exclusions"], ["catalog load and authority construction/validation", "JWT verification", "dispatcher freshness/wait paths", "stderr OS audit I/O", "transport/storage/provider/SQLite C", "cluster capacity or full-production qualification"], "exclusions")
    exact(observations["oracles"], "validated target geometry, two candidates, exact authority Arc/cache reuse, full permission equivalence, LF-terminated audit bytes/grant order, reused-index current claims and empty-condition distinction", "source oracle declaration")
    summary_keys = ("schema", "base_revision", "benchmark_elapsed_ns", "cfg_test_scope", "exclusions",
                     "public_observations_sha256", "receipt_sha256", "release_features", "rows", "scope",
                     "source_before_after_match", "source_input_count", "source_input_sha256", "stderr_sha256",
                     "stdout_sha256", "working_candidate")
    final_receipt_keys = ("binary_before_after_match", "binary_bytes", "binary_sha256",
                          "inner_budget_seconds", "outer_deadline_seconds",
                          "all_shipping_strict_clippy_receipt_sha256",
                          "final_release_regressions_receipt_sha256")
    has_final_receipt = type(summary) is dict and "binary_sha256" in summary
    closed(summary, summary_keys + (final_receipt_keys if has_final_receipt else ()), "summary")
    exact(summary["schema"], "mount-rs.grant-index-paired-summary.v1", "summary schema")
    exact(summary["public_observations_sha256"], observation_sha, "observation binding")
    exact(summary["benchmark_elapsed_ns"], observations["elapsed_ns"], "elapsed projection")
    exact(summary["cfg_test_scope"], observations["cfg_test_scope"], "instrumentation projection")
    exact(summary["exclusions"], observations["exclusions"], "exclusions projection")
    exact(summary["scope"], observations["timed_operations"], "scope projection")
    exact(summary["source_before_after_match"], True, "source equality declaration")
    exact(summary["working_candidate"], True, "working candidate declaration")
    require(type(summary["base_revision"]) is str and re.fullmatch(r"[0-9a-f]{40}", summary["base_revision"]) is not None, "base revision shape")
    integer(summary["source_input_count"], "source input count", 1)
    require(type(summary["release_features"]) is str and bool(summary["release_features"]), "release features")
    for key in ("receipt_sha256", "source_input_sha256", "stderr_sha256", "stdout_sha256"):
        digest(summary[key], key)
    if has_final_receipt:
        exact(summary["binary_before_after_match"], True, "binary equality declaration")
        integer(summary["binary_bytes"], "binary byte count", 1)
        exact(summary["inner_budget_seconds"], 55, "internal cooperative budget declaration")
        exact(summary["outer_deadline_seconds"], 55, "external deadline declaration")
        for key in ("binary_sha256", "all_shipping_strict_clippy_receipt_sha256",
                    "final_release_regressions_receipt_sha256"):
            digest(summary[key], key)
    require(type(observations["scales"]) is list and len(observations["scales"]) == 4, "four scales")
    require(type(summary["rows"]) is list and len(summary["rows"]) == 4, "four summary rows")
    native_windows = allocation_windows = pairs_total = cold_records = 0
    for index, grants in enumerate(EXPECTED_GRANTS):
        scale, row = observations["scales"][index], summary["rows"][index]
        closed(scale, ("grants", "partitions", "scoped_candidates", "query_positions", "cycles_per_window", "pairs", "calibration"), "scale")
        exact(scale["grants"], grants, "grants geometry")
        exact(scale["partitions"], grants // 2, "partition geometry")
        exact(scale["scoped_candidates"], 2, "two scoped candidates")
        positions = [0, grants // 2, grants - 1]
        exact(scale["query_positions"], positions, "query positions")
        for position in scale["query_positions"]:
            integer(position, "query position integer")
        cycles = integer(scale["cycles_per_window"], "window cycles", 24)
        require(cycles <= 3072 and cycles % 3 == 0, "window cycle bounds")
        calibration = scale["calibration"]
        closed(calibration, ("cycles_per_mode", "full", "indexed", "cold"), "calibration")
        exact(calibration["cycles_per_mode"], 48, "pilot cycles")
        for mode in MODES:
            native(calibration[mode], 48, positions, "pilot/" + mode)
        cold(calibration["cold"], "pilot cold")
        cold_records += 1
        slowest = max(calibration[mode]["elapsed_ns"] for mode in MODES)
        estimate = min(3072, max(24, 25_000_000 * 48 // max(slowest, 1)))
        exact(cycles, estimate // 3 * 3, "independent pilot calibration")
        require(type(scale["pairs"]) is list and len(scale["pairs"]) == 8, "eight pairs")
        wall = {mode: [] for mode in MODES}
        cpu = {mode: [] for mode in MODES}
        paired_speedup = []
        cold_elapsed, cold_calls, cold_bytes = [], [], []
        for pair_index, pair in enumerate(scale["pairs"]):
            closed(pair, ("pair", "first", "cold", "native", "allocation_only"), "pair")
            exact(pair["pair"], pair_index, "pair identity/order")
            exact(pair["first"], "full" if pair_index % 2 == 0 else "indexed", "alternating order")
            cold(pair["cold"], "pair cold")
            cold_records += 1
            cold_elapsed.append(Fraction(pair["cold"]["elapsed_ns"]))
            cold_calls.append(pair["cold"]["rust_alloc_calls"])
            cold_bytes.append(pair["cold"]["rust_alloc_requested_bytes"])
            closed(pair["native"], MODES, "native modes")
            closed(pair["allocation_only"], MODES, "allocation modes")
            for mode in MODES:
                wall_ns, cpu_ns = native(pair["native"][mode], cycles, positions, "native/" + mode)
                wall[mode].append(wall_ns)
                cpu[mode].append(cpu_ns)
                native_windows += 1
                allocation(pair["allocation_only"][mode], cycles, positions, "allocation/" + mode)
                allocation_windows += 1
            paired_speedup.append(wall["full"][-1] / wall["indexed"][-1])
            pairs_total += 1
        closed(row, ("grants", "partitions", "pairs", "cycles_per_window", "full_median_wall_ns_per_cycle",
                     "indexed_median_wall_ns_per_cycle", "full_median_process_cpu_ns_per_cycle",
                     "indexed_median_process_cpu_ns_per_cycle", "paired_median_wall_speedup", "wall_ns_ranges",
                     "cold_median_wall_ns", "cold_alloc_call_values", "cold_alloc_requested_byte_values",
                     "hot_alloc_requests_and_bytes_both_modes"), "summary row")
        exact(row["grants"], grants, "row grants")
        exact(row["partitions"], grants // 2, "row partitions")
        exact(row["pairs"], 8, "row pairs")
        exact(row["cycles_per_window"], cycles, "row cycles")
        closed(row["wall_ns_ranges"], MODES, "wall ranges")
        for mode in MODES:
            close_numeric(row[mode + "_median_wall_ns_per_cycle"], median(wall[mode]), mode + " wall median")
            close_numeric(row[mode + "_median_process_cpu_ns_per_cycle"], median(cpu[mode]), mode + " CPU median")
            require(type(row["wall_ns_ranges"][mode]) is list and len(row["wall_ns_ranges"][mode]) == 2, "range length")
            close_numeric(row["wall_ns_ranges"][mode][0], min(wall[mode]), mode + " minimum")
            close_numeric(row["wall_ns_ranges"][mode][1], max(wall[mode]), mode + " maximum")
        close_numeric(row["paired_median_wall_speedup"], median(paired_speedup), "median of matched per-pair speedups")
        close_numeric(row["cold_median_wall_ns"], median(cold_elapsed), "cold elapsed median")
        exact(row["cold_alloc_call_values"], sorted(set(cold_calls)), "cold call values")
        exact(row["cold_alloc_requested_byte_values"], sorted(set(cold_bytes)), "cold cumulative requested-byte values")
        exact(row["hot_alloc_requests_and_bytes_both_modes"], [0, 0], "hot allocation projection")
    exact(pairs_total, 32, "32 paired observations")
    exact(native_windows, 64, "64 paired native windows")
    exact(allocation_windows, 64, "64 paired allocation windows")
    exact(cold_records, 36, "36 recorded cold preparations including four pilots")
    print(json.dumps({"status": "passed", "checks": CHECKS, "scales": 4, "pairs": pairs_total,
                      "native_windows": native_windows, "allocation_windows": allocation_windows,
                      "pilot_native_windows": 8, "recorded_cold_preparations": cold_records,
                      "observations_sha256": observation_sha, "summary_sha256": summary_sha,
                      "audit_scope": "public JSON arithmetic/geometry/projections only; private runtime/source/binary evidence requires root verification",
                      "cold_allocation_scope": "cumulative allocation/reallocation requested bytes, not retained/live heap or RSS"}, sort_keys=True))


if __name__ == "__main__":
    try:
        main()
    except (OSError, UnicodeError, ValueError, TypeError, KeyError, IndexError, ZeroDivisionError) as error:
        print("grant benchmark summary rejected: " + str(error), file=sys.stderr)
        raise SystemExit(1)
