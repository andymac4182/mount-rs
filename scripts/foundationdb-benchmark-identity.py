#!/usr/bin/env python3
"""Verify the owned SSD benchmark cluster and retain only redacted identity."""

import json
import sys
from pathlib import Path


def verified_identity(status, topology, image):
    cluster = status["cluster"]
    configuration = cluster["configuration"]
    engine = configuration["storage_engine"]
    redundancy = configuration["redundancy_mode"]
    expected_redundancy = {"single": "single", "durable": "double"}[topology]
    if engine != "ssd-2" or redundancy != expected_redundancy:
        raise ValueError(f"expected SSD storage/{expected_redundancy}, observed {engine}/{redundancy}")
    versions = sorted({process["version"] for process in cluster["processes"].values()})
    if versions != ["7.4.7"]:
        raise ValueError(f"expected native FoundationDB 7.4.7, observed {versions}")
    if not cluster.get("database_available") or not cluster.get("full_replication"):
        raise ValueError("owned benchmark database must be available and fully replicated")
    return {
        "schema": "mount-rs-foundationdb-benchmark-identity-v1",
        "image": image,
        "topology": topology,
        "storage_engine": engine,
        "redundancy_mode": redundancy,
        "server_versions": versions,
        "readiness": "committed-set-get-clear",
        "data_storage": "owned-run-labeled-docker-volume",
    }


def main():
    status_path, topology, image, output_path = sys.argv[1:]
    identity = verified_identity(json.loads(Path(status_path).read_text()), topology, image)
    Path(output_path).write_text(json.dumps(identity, indent=2) + "\n")
    print(json.dumps(identity, sort_keys=True, separators=(",", ":")))


if __name__ == "__main__":
    main()
