#!/usr/bin/env python3
"""Reduce exported FDB/VM counters; neither scope is physical host SSD IOPS."""
import argparse
import json
from pathlib import Path


def counters(value, path=""):
    result = {}
    if isinstance(value, dict):
        for key, child in value.items():
            name = f"{path}.{key}" if path else key
            if key == "counter" and isinstance(child, (int, float)):
                result[path] = child
            else:
                result.update(counters(child, name))
    return result


def diskstats(text):
    result = {}
    for line in text.splitlines():
        fields = line.split()
        if len(fields) < 14:
            continue
        # Linux block accounting uses 512-byte sectors, regardless of device size.
        result[fields[2]] = dict(zip(
            ("reads", "read_sectors", "read_ms", "writes", "write_sectors", "write_ms", "io_ms"),
            (int(fields[index]) for index in (3, 5, 6, 7, 9, 10, 12)),
        ))
    return result


def deltas(before, after, seconds):
    output = {}
    for key in sorted(before.keys() & after.keys()):
        delta = after[key] - before[key]
        output[key] = {"delta": delta if delta >= 0 else None,
                       "per_second": delta / seconds if delta >= 0 else None,
                       "counter_reset": delta < 0}
    return output


def reduce_stage(directory, stage):
    begin = directory / f"{stage}.begin"
    end = directory / f"{stage}.end"
    before = json.loads(Path(f"{begin}.status.json").read_text())["cluster"]
    after = json.loads(Path(f"{end}.status.json").read_text())["cluster"]
    start = int(Path(f"{begin}.finish-ns").read_text())
    finish = int(Path(f"{end}.start-ns").read_text())
    seconds = (finish - start) / 1e9
    if seconds <= 0:
        raise ValueError("nonpositive observation interval")
    logical_before = counters(before["workload"], "cluster.workload")
    logical_after = counters(after["workload"], "cluster.workload")
    disks_before = diskstats(Path(f"{begin}.diskstats").read_text())
    disks_after = diskstats(Path(f"{end}.diskstats").read_text())
    process_delta = {}
    for process in sorted(before.get("processes", {}).keys() & after.get("processes", {}).keys()):
        process_delta[process] = deltas(
            counters(before["processes"][process].get("disk", {})),
            counters(after["processes"][process].get("disk", {})), seconds)
    mode, queue, successes, failures = Path(f"{end}.stage-info").read_text().split()
    client_io = {"available": False}
    io_begin, io_end = Path(f"{begin}.client-io"), Path(f"{end}.client-io")
    if io_begin.exists() and io_end.exists():
        def parse_io(path):
            return {key.rstrip(":"): int(value) for key, value in
                    (line.split() for line in path.read_text().splitlines())}
        client_io = {"available": True, "scope": "owned native integration-test Linux process",
                     "counters": deltas(parse_io(io_begin), parse_io(io_end), seconds)}
        if int(successes) > 0:
            client_io["syscw_per_successful_application_op"] = client_io["counters"]["syscw"]["delta"] / int(successes)
    disk_delta = {device: deltas(disks_before[device], disks_after[device], seconds)
                  for device in sorted(disks_before.keys() & disks_after.keys())}
    return {"schema": "mount-rs-foundationdb-counter-deltas-v1", "stage": stage,
            "observation_seconds": seconds,
            "application": {"mode": mode, "total_queue_depth": int(queue), "successes": int(successes), "failures": int(failures)},
            "fdb_process_device_counters": process_delta,
            "logical_datastore_counters": deltas(logical_before, logical_after, seconds),
            "latency_probe_seconds_begin": before.get("latency_probe"),
            "latency_probe_seconds_end": after.get("latency_probe"),
            "linux_vm_block_devices": disk_delta,
            "client_process_io": client_io,
            "limitations": [
                "FDB status counters are cluster-wide exported snapshots and include probes/background traffic.",
                "Status export lag means interval counts are diagnostic, not exact application attribution.",
                "Latency probes sample internal transactions; they are not application latency percentiles.",
                "Linux /proc/diskstats covers shared Docker VM block devices, not physical macOS SSD or one container.",
                "Do not sum parent devices and partitions; device mapping may overlap.",
                "Logical operations, commits, and block-device IOPS are different measurement units.",
            ]}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("directory", type=Path)
    parser.add_argument("stage")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    result = json.dumps(reduce_stage(args.directory, args.stage), indent=2) + "\n"
    if args.output:
        args.output.write_text(result)
    else:
        print(result, end="")


if __name__ == "__main__":
    main()
