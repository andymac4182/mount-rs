#!/usr/bin/env python3
"""Reduce successful canonical scaling artifacts without counting retained copies."""
import argparse
import json
from pathlib import Path


def summarize(path, metrics_root):
    artifact = json.loads(path.read_text())
    if artifact["verification_status"] != "passed" or any(
        artifact[key] is not None
        for key in ("work_error", "verification_error", "cleanup_error")
    ):
        raise ValueError(f"unqualified workload: {path}")
    rows = []
    for stage_index, stage in enumerate(artifact["stages"]):
        operations = stage["read"]["completed"] + stage["write"]["completed"]
        if stage["failures"] or not operations:
            raise ValueError(f"unqualified stage: {path}")
        resources = stage["resources"]
        profile = {entry["name"]: entry for entry in stage["io_profile"]["entries"]}
        row = {
            "provider": artifact["provider"],
            "inode_updates": artifact["inode_updates"],
            "separate_drives": artifact.get("separate_drives", False),
            "drive_count": artifact.get("drive_count", 1),
            "driver_replicas": artifact.get("driver_replicas", artifact["servers"]),
            "driver_setup_seconds": artifact.get("driver_setup_seconds"),
            "provisioning_seconds": artifact.get("provisioning_seconds"),
            "parallel_server_startup": artifact.get("parallel_server_startup", False),
            "drives_provisioned_before_startup": artifact.get("drives_provisioned_before_startup", False),
            "verification_method": artifact.get("verification_method"),
            "servers": stage["servers"],
            "clients": stage["clients"],
            "active_clients": stage.get("active_clients", stage["clients"]),
            "setup_seconds": artifact.get("setup_seconds"),
            "depth": stage["per_client_depth"],
            "mode": stage["mode"],
            "operations": operations,
            "operations_per_second": stage["total_iops"],
            "p99_us_upper": max(stage["read"]["p99_us_upper"], stage["write"]["p99_us_upper"]),
            "cpu_us_per_operation": (resources["cpu_user_us"] + resources["cpu_system_us"]) / operations,
            "cpu_cores": (resources["cpu_user_us"] + resources["cpu_system_us"]) / 1e6 / stage["elapsed_seconds_including_drain"],
            "rss_end_bytes": resources["rss_end_bytes"],
            "allocations_per_operation": resources["rust_allocations"] / operations if resources["rust_allocator_instrumented"] else None,
            "inode_cas_conflicts": profile.get("provider.inode.cas_conflict", {}).get("calls", 0),
            "namespace_cas_conflicts": profile.get("provider.metadata.cas_conflict", {}).get("calls", 0),
            "verified_files": artifact["verified_files"],
            "artifact": str(path),
        }
        row["profile_us_per_operation"] = {
            name: profile.get(name, {}).get("elapsed_ns", 0) / 1000 / operations
            for name in ("filesystem.gate_wait", "service.dispatch", "service.authorization",
                         "catalog.load", "catalog.pool_wait", "catalog.decode_validate_bytes",
                         "provider.inode.load_if_changed", "provider.inode.publish_cas")
        }
        row["catalog_document_bytes_per_operation"] = profile.get("catalog.query_document_bytes", {}).get("units", 0) / operations
        row["quic_network"] = resources.get("quic_client_side")
        if "sqlite_io_end" in stage:
            connections = stage["sqlite_io_end"]["connections"]
            for key in ("sql_statements", "pager_read_bytes_estimate", "pager_write_bytes_estimate"):
                row[key + "_per_operation"] = sum(c[key] for c in connections) / operations
        stage_id = artifact["volume_key"] + "-" + str(2 * (stage_index + 1))
        metrics_paths = list(metrics_root.rglob("*" + stage_id + ".metrics.json"))
        if len(metrics_paths) > 1:
            raise ValueError("ambiguous datastore metrics")
        if metrics_paths:
            metrics = json.loads(metrics_paths[0].read_text())
            row["datastore_sessions"] = metrics.get("sessions_by_source")
            row["datastore_window_seconds"] = metrics["sample_elapsed_seconds"]
            row["datastore_counter_resets"] = metrics["counter_resets"]
            row["datastore_series_coverage_incomplete"] = metrics["series_coverage_incomplete"]
            row["datastore_cpu_seconds_per_operation"] = {
                component: values.get("process_cpu_seconds_total")
                for component, values in metrics["selected_per_logical_success"].items()
            }
            row["datastore_vm_io_per_operation"] = metrics["vm_io_per_logical_success"]
            row["datastore_sql_per_operation"] = metrics["selected_per_logical_success"].get("tidb", {}).get("tidb_executor_statement_total")
        rows.append(row)
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    rows = []
    failures = []
    for provider in ("sqlite", "tidb", "foundationdb"):
        for path in args.directory.rglob(provider + "-*.json"):
            if all(part.isdigit() for part in path.stem.removeprefix(provider + "-").split("-")):
                try:
                    rows.extend(summarize(path, args.directory))
                except ValueError as error:
                    artifact = json.loads(path.read_text())
                    failures.append({"artifact": str(path), "reason": str(error),
                                     "work_error": artifact.get("work_error"),
                                     "verification_error": artifact.get("verification_error"),
                                     "cleanup_error": artifact.get("cleanup_error")})
    if not rows:
        raise ValueError("no canonical scaling artifacts")
    rows.sort(key=lambda row: (row["provider"], row["servers"], row["clients"], row["separate_drives"], row["active_clients"], row["depth"], row["mode"], row["artifact"]))
    args.output.write_text(json.dumps({"schema": "mount-rs-horizontal-scaling-v1", "stages": rows, "unqualified_workloads": failures}, indent=2) + "\n")
    for row in rows:
        print(f'{row["provider"]:12} {row["servers"]:3} servers {row["clients"]:5} clients active={row["active_clients"]:5} depth={row["depth"]} {row["mode"]:5} '
              f'{row["operations_per_second"]:8.0f}/s p99<={row["p99_us_upper"] / 1000:7.1f}ms '
              f'CPU={row["cpu_cores"]:5.2f} cores conflicts={row["inode_cas_conflicts"]}')


if __name__ == "__main__":
    main()
