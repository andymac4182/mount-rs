#!/usr/bin/env python3
"""Sample only this harness's owned TiDB/PD/TiKV counters around a stage."""

import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import time
import urllib.request
from concurrent.futures import ThreadPoolExecutor

LABEL = "mount-rs.tidb.run"
SAMPLE = re.compile(r"^([a-zA-Z_:][a-zA-Z0-9_:]*)(\{[^}]*\})?\s+([-+0-9.eE]+)(?:\s|$)")
SAFE = re.compile(r"^[a-zA-Z0-9._-]+$")


def run(*args):
    return subprocess.run(args, check=True, capture_output=True, text=True, timeout=15).stdout


def owned(name, run_id):
    info = json.loads(run("docker", "inspect", name))[0]
    if info["Config"]["Labels"].get(LABEL) != run_id or info["State"]["Status"] != "running":
        raise RuntimeError(f"container {name} is not a running member of this TiDB test run")
    return info


def metric_text(name, kind, info):
    if kind == "tidb":
        bindings = info["NetworkSettings"]["Ports"].get("10080/tcp") or []
        if len(bindings) != 1 or bindings[0]["HostIp"] not in ("127.0.0.1", "::1"):
            raise RuntimeError("owned TiDB status port is not bound once on loopback")
        port = int(bindings[0]["HostPort"])
        with urllib.request.urlopen(f"http://127.0.0.1:{port}/metrics", timeout=10) as response:
            return response.read().decode("utf-8")
    port = 20180 if kind == "tikv" else 2379
    return run("docker", "exec", name, "curl", "-fsS", "--max-time", "10", f"http://127.0.0.1:{port}/metrics")


def parse_metrics(raw):
    metric_types = {}
    for line in raw.splitlines():
        if line.startswith("# TYPE "):
            bits = line.split()
            if len(bits) == 4:
                metric_types[bits[2]] = bits[3]
    samples = {}
    families = set()
    available = set()
    for line in raw.splitlines():
        match = SAMPLE.match(line)
        if not match:
            continue
        name, labels, value = match.groups()
        available.add(name)
        family = name.removesuffix("_total").removesuffix("_count")
        measured_counter = metric_types.get(name) == "counter" or metric_types.get(family) == "counter"
        histogram_count = name.endswith("_count") and metric_types.get(family) in ("histogram", "summary")
        rocksdb_perf = name == "tikv_storage_rocksdb_perf" and metric_types.get(name) == "counter"
        if not (measured_counter or histogram_count or rocksdb_perf):
            continue
        try:
            number = float(value)
        except ValueError:
            continue
        if number != number or number in (float("inf"), float("-inf")):
            continue
        samples[f"{name}{labels or ''}"] = number
        families.add(name)
    return samples, sorted(families), sorted(available)


def parse_proc_io(raw):
    result = {}
    for line in raw.splitlines():
        if ":" not in line:
            continue
        key, value = line.split(":", 1)
        if key in {"rchar", "wchar", "syscr", "syscw", "read_bytes", "write_bytes", "cancelled_write_bytes"}:
            result[key] = int(value.strip())
    return result


def parse_cgroup_io(raw):
    result = {}
    for line in raw.splitlines():
        bits = line.split()
        if not bits:
            continue
        device = bits[0]
        for entry in bits[1:]:
            if "=" in entry:
                key, value = entry.split("=", 1)
                if key in {"rios", "wios", "dios", "rbytes", "wbytes", "dbytes"}:
                    result[f"{device}:{key}"] = int(value)
    return result


def parse_net_dev(raw):
    """Count traffic on the container's eth0, excluding loopback."""
    for line in raw.splitlines():
        if ":" not in line:
            continue
        interface, fields = line.split(":", 1)
        if interface.strip() != "eth0":
            continue
        values = fields.split()
        if len(values) != 16:
            raise ValueError("unexpected /proc/1/net/dev eth0 layout")
        return {
            "rx_bytes": int(values[0]),
            "rx_packets": int(values[1]),
            "tx_bytes": int(values[8]),
            "tx_packets": int(values[9]),
        }
    raise ValueError("owned container has no eth0 interface")


def sample_source(args):
    name, kind, run_id = args
    info = owned(name, run_id)
    raw = metric_text(name, kind, info)
    metrics, families, available = parse_metrics(raw)
    proc_io = parse_proc_io(run("docker", "exec", name, "cat", "/proc/1/io"))
    net_dev = parse_net_dev(run("docker", "exec", name, "cat", "/proc/1/net/dev"))
    try:
        cgroup_io = parse_cgroup_io(run("docker", "exec", name, "cat", "/sys/fs/cgroup/io.stat"))
        cgroup_error = None
    except (subprocess.CalledProcessError, FileNotFoundError) as error:
        cgroup_io = {}
        cgroup_error = type(error).__name__
    return name, {
        "kind": kind,
        "process_pid": info["State"]["Pid"],
        "process_started_at": info["State"]["StartedAt"],
        "captured_at_unix_ns": time.time_ns(),
        "captured_at_monotonic_ns": time.monotonic_ns(),
        "metrics": metrics,
        "metric_families": families,
        "available_metric_names": available,
        "proc_io": proc_io,
        "net_dev": net_dev,
        "cgroup_io": cgroup_io,
        "cgroup_io_error": cgroup_error,
    }


def snapshot(run_id, pd_count, tikv_count):
    names = [(f"mount-rs-tidb-{run_id}-tidb", "tidb", run_id)]
    names += [(f"mount-rs-tidb-{run_id}-pd{i}", "pd", run_id) for i in range(1, pd_count + 1)]
    names += [(f"mount-rs-tidb-{run_id}-tikv{i}", "tikv", run_id) for i in range(1, tikv_count + 1)]
    result = {
        "captured_at_unix_ns": time.time_ns(),
        "captured_at_monotonic_ns": time.monotonic_ns(),
        "sources": {},
    }
    with ThreadPoolExecutor(max_workers=len(names)) as workers:
        for name, data in workers.map(sample_source, names):
            result["sources"][name] = data
    result["completed_at_unix_ns"] = time.time_ns()
    result["completed_at_monotonic_ns"] = time.monotonic_ns()
    return result


def elapsed_seconds(before, after):
    # Older retained snapshots predate the monotonic field. They remain
    # readable for diagnostics, but all new measurements use monotonic time.
    key = "captured_at_monotonic_ns" if "captured_at_monotonic_ns" in before and "captured_at_monotonic_ns" in after else "captured_at_unix_ns"
    delta_ns = after[key] - before[key]
    if delta_ns <= 0:
        raise RuntimeError("nonpositive observer interval")
    return delta_ns / 1e9


def diff(before, after):
    deltas = {}
    resets = []
    for source, prior in before["sources"].items():
        current = after["sources"].get(source)
        if current is None:
            raise RuntimeError(f"owned source disappeared: {source}")
        if (prior["process_pid"], prior["process_started_at"]) != (current["process_pid"], current["process_started_at"]):
            raise RuntimeError(f"owned source restarted during stage: {source}")
        source_delta = {}
        resets_before_source = len(resets)
        for section in ("metrics", "proc_io", "cgroup_io", "net_dev"):
            values = {}
            for key, old in prior.get(section, {}).items():
                if key not in current.get(section, {}):
                    continue
                change = current[section][key] - old
                if change < 0:
                    resets.append(f"{source}:{section}:{key}")
                else:
                    values[key] = change
            source_delta[section] = values
        source_delta["metric_new_series"] = sorted(set(current["metrics"]) - set(prior["metrics"]))
        source_delta["metric_missing_series"] = sorted(set(prior["metrics"]) - set(current["metrics"]))
        # Preserve the reset diagnosis even when a synthetic or restarted
        # counter also has an invalid timestamp; both invalidate the stage.
        if len(resets) > resets_before_source:
            try:
                source_delta["sample_elapsed_seconds"] = elapsed_seconds(prior, current)
            except RuntimeError:
                source_delta["sample_elapsed_seconds"] = None
        else:
            source_delta["sample_elapsed_seconds"] = elapsed_seconds(prior, current)
        deltas[source] = source_delta
    return deltas, resets


def totals_by_kind(deltas, after):
    totals = {}
    for source, sections in deltas.items():
        kind = after["sources"][source]["kind"]
        grouped = totals.setdefault(kind, {"metrics": {}, "proc_io": {}, "cgroup_io": {}, "net_dev": {}})
        for metric, change in sections["metrics"].items():
            family = metric.split("{", 1)[0]
            if family == "tikv_storage_rocksdb_perf":
                match = re.search(r'(?:\{|,)metric="([^"]+)"', metric)
                family = f"{family}[metric={match.group(1) if match else 'unknown'}]"
            grouped["metrics"][family] = grouped["metrics"].get(family, 0) + change
        for section in ("proc_io", "cgroup_io", "net_dev"):
            for key, change in sections[section].items():
                aggregate = key.rsplit(":", 1)[-1] if section == "cgroup_io" else key
                grouped[section][aggregate] = grouped[section].get(aggregate, 0) + change
    return totals


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", dir=path.parent, delete=False) as file:
        json.dump(value, file, sort_keys=True, indent=2)
        file.write("\n")
        temporary = Path(file.name)
    os.replace(temporary, path)


def main():
    if len(sys.argv) != 7:
        raise ValueError("usage: observer begin|end read|write|mixed depth stage_id successes failures")
    action, mode, depth, stage_id, successes, failures = sys.argv[1:]
    if action not in ("begin", "end") or mode not in ("read", "write", "mixed"):
        raise ValueError("invalid observer action or mode")
    if not SAFE.fullmatch(stage_id):
        raise ValueError("stage_id must be a filename-safe identifier")
    depth, successes, failures = int(depth), int(successes), int(failures)
    if depth < 1 or successes < 0 or failures < 0:
        raise ValueError("invalid depth or operation count")
    run_id = os.environ["MOUNT_RS_TIDB_RUN_ID"]
    if not SAFE.fullmatch(run_id):
        raise ValueError("invalid owned TiDB run id")
    pd_count = int(os.environ["MOUNT_RS_TIDB_PD_COUNT"])
    tikv_count = int(os.environ["MOUNT_RS_TIDB_TIKV_COUNT"])
    if pd_count not in (1, 3) or tikv_count not in (1, 3):
        raise ValueError("unexpected owned topology")
    directory = Path(os.environ.get("MOUNT_RS_DATASTORE_METRICS_OUTPUT_DIR", Path(tempfile.gettempdir()) / "mount-rs-tidb-metrics"))
    stem = f"{run_id}-{stage_id}"
    before_path = directory / f"{stem}.begin.json"
    if action == "begin":
        if successes or failures or before_path.exists():
            raise ValueError("begin requires zero outcomes and a fresh stage id")
        write_json(before_path, snapshot(run_id, pd_count, tikv_count))
        print(f"TIDB_STAGE_OBSERVER_BEGIN stage={stage_id} snapshot={before_path}", flush=True)
        return
    if not before_path.is_file():
        raise ValueError("missing begin snapshot")
    before = json.loads(before_path.read_text(encoding="utf-8"))
    after = snapshot(run_id, pd_count, tikv_count)
    after_path = directory / f"{stem}.end.json"
    write_json(after_path, after)
    deltas, resets = diff(before, after)
    totals = totals_by_kind(deltas, after)
    selected = {}
    for kind, sections in totals.items():
        selected[kind] = {
            family: value
            for family, value in sections["metrics"].items()
            if family.startswith((
                "tidb_executor_statement_", "tidb_server_query_", "tidb_tikvclient_request_",
                "tikv_grpc_msg_", "tikv_storage_command_", "tikv_storage_rocksdb_perf",
                "tikv_engine_rocksdb_", "pd_client_",
            ))
        }
    summary = {
        "run_id": run_id,
        "stage_id": stage_id,
        "mode": mode,
        "total_queue_depth": depth,
        "logical_successes": successes,
        "logical_failures": failures,
        "sample_elapsed_seconds": elapsed_seconds(before, after),
        "begin_snapshot": str(before_path),
        "end_snapshot": str(after_path),
        "counter_deltas": deltas,
        "totals_by_component_kind": totals,
        "selected_sql_kv_rocksdb_counters": selected,
        "selected_per_logical_success": {
            kind: {family: value / successes for family, value in metrics.items()}
            for kind, metrics in selected.items()
        } if successes else None,
        "vm_io_per_logical_success": {
            kind: {
                section: {name: value / successes for name, value in sections[section].items()}
                for section in ("proc_io", "cgroup_io", "net_dev")
            }
            for kind, sections in totals.items()
        } if successes else None,
        "counter_resets": resets,
        "series_coverage_incomplete": any(
            data["metric_new_series"] or data["metric_missing_series"] for data in deltas.values()
        ),
        "boundaries": {
            "prometheus": "TiDB SQL, PD, and TiKV process counters; cluster/background traffic included",
            "proc_io": "Linux VM process read/write bytes and syscall counts; not physical Mac SSD IOPS",
            "cgroup_io": "Linux VM cgroup block I/O counts and bytes if available; not physical Mac SSD IOPS",
            "net_dev": "Container eth0 receive/transmit bytes and packets; includes SQL, TiKV, metrics, and background traffic, not physical NIC usage",
        },
    }
    summary_path = directory / f"{stem}.metrics.json"
    write_json(summary_path, summary)
    if resets:
        raise RuntimeError(f"counter reset detected; artifact={summary_path} count={len(resets)}")
    print(f"TIDB_STAGE_OBSERVER_END stage={stage_id} successes={successes} failures={failures} metrics={summary_path}", flush=True)


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"TIDB_STAGE_OBSERVER_ERROR {type(error).__name__}: {error}", file=sys.stderr)
        sys.exit(1)
