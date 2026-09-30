#!/usr/bin/env python3
"""Offline synthetic controls for production-target receipt verification.

These temporary JSON/gzip fixtures are verifier test inputs, never live client,
storage, durability, throughput, or production-capacity evidence. No provider,
server, Cargo command, or retained historical capture is used.
"""

import argparse
import gzip
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


RUNNER = Path(__file__).with_name("bench-remote-production-target.sh")
SHARED_VERIFIER = Path(__file__).with_name("verify-production-target-evidence.py")
STORED_LIMIT = 16 * 1024 * 1024
DECODED_LIMIT = 64 * 1024 * 1024
SENTINEL = "FULL_TARGET_GEOMETRY_REGRESSION"

QUIC_SCHEMA = "mount-rs-controller-quic-v1"
QUIC_SCOPE = (
    "actual retained client connections; active workload plus idle liveness; "
    "snapshot observer outside active throughput interval; server transport retained "
    "separately in worker phase receipts"
)
PATTERNS = (
    "sequential_read", "random_read", "sequential_overwrite", "random_overwrite",
    "mixed", "hot_file", "append_truncate", "churn",
)
GEOMETRY_FAILURE = (
    SENTINEL + ": relabeled 10-Drive/F2/1-second control was accepted as full"
)


def current_inline_program():
    """Execute the current verifier verbatim, without invoking the shell runner."""
    source = RUNNER.read_text(encoding="utf-8")
    marker = "python3 - <<'PY'\n"
    if source.count(marker) != 1:
        raise RuntimeError("expected exactly one current inline verifier")
    program, end, suffix = source.split(marker, 1)[1].partition("\nPY\n")
    if not end or suffix.strip():
        raise RuntimeError("current inline verifier boundary changed")
    return program


def verify_fixture(root, mode, timeout=10):
    """Adapter for today's inline verifier and the later shared raising API."""
    if SHARED_VERIFIER.is_file():
        spec = importlib.util.spec_from_file_location(
            "production_target_evidence", SHARED_VERIFIER
        )
        if spec is None or spec.loader is None:
            raise RuntimeError("shared verifier import unavailable")
        verifier = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(verifier)
        try:
            verifier.verify_terminal(root, mode)
        except (OSError, ValueError, KeyError, TypeError, AssertionError) as error:
            return subprocess.CompletedProcess(
                [str(SHARED_VERIFIER)], 2, "", type(error).__name__
            )
        return subprocess.CompletedProcess([str(SHARED_VERIFIER)], 0, "", "")

    environment = os.environ.copy()
    environment["MOUNT_RS_TARGET_OUTPUT"] = str(root)
    environment["MOUNT_RS_TARGET_MODE"] = mode
    return subprocess.run(
        [sys.executable, "-c", current_inline_program()],
        env=environment,
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
    )


class SyntheticControl:
    """Synthetic packed Drive ledgers joined to two passes and sixteen QUIC cells."""

    DRIVES = 10
    FILES = 2
    SERVERS = 10
    CONTROLLER_PID = 2000

    def __init__(self, root):
        self.root = root
        self.receipt_names = []
        self.worker_metric_names = []
        self.total_stored_bytes = 0
        sizes=[4096 if identity<990 else 131072 if identity<999 else 1048576
               for identity in range(self.FILES)]
        profile_bytes=sum(sizes)
        slots=profile_bytes//4096
        workers = [
            dict(server=server, pid=1000 + server, reap_confirmed=True, exit_code=0)
            for server in range(self.SERVERS)
        ]
        self.terminal = dict(
            schema="mount-rs-production-target-v1",
            scope="synthetic offline verifier fixture; no live capacity proof",
            phase="terminal",
            outcome="success",
            full_target=False,
            configuration=dict(
                full_target=False,
                drives=self.DRIVES,
                files=self.FILES,
                seconds=1,
                population_seconds=600,
                provider="sqlite",
            ),
            workers=workers,
            cleanup_errors=[],
            verified_passes=2,
            fresh_oracle_complete=True,
            fresh_oracle_settled=True,
            expected_state_observation=dict(complete=True),
            expected_state_receipts=dict(schema="mount-rs-expected-ledger-inventory-v1",
                                         drive_count=self.DRIVES,pack_size=32,packs=[]),
            verified_files=self.DRIVES * self.FILES,
            verified_bytes=self.DRIVES * profile_bytes,
            namespace_files=self.DRIVES * self.FILES,
            population_bytes=self.DRIVES * profile_bytes,
            fresh_oracle_passes=[],
            phase_metrics=dict(boundaries=[]),
            metrics_required_for_outcome=True,
            controller_resources=dict(pid=self.CONTROLLER_PID),
            source=dict(digest="a" * 64, binary_sha256="b" * 64),
        )
        for pack,first in enumerate(range(0,self.DRIVES,32)):
            ledgers=[]
            for drive in range(first,min(first+32,self.DRIVES)):
                ledgers.append(dict(
                    drive=drive,
                    files={f"mixed-{identity}":dict(identity=identity,length=size,changed={})
                           for identity,size in enumerate(sizes)},
                    generations=[0] * slots,
                    oracle="tuple-seeded4096-byte blocks; initial generation0"))
            receipt=self.receipt(f"expected/pack-{pack:05}.json.gz",
                                 dict(schema="mount-rs-expected-ledger-pack-v1",pack=pack,
                                      first_drive=first,ledgers=ledgers))
            receipt.update(pack=pack,first_drive=first,count=len(ledgers))
            self.terminal["expected_state_receipts"]["packs"].append(receipt)
            del ledgers

        for sequence, label in enumerate(("initial", "final")):
            summary = dict(
                slot_limit=8,
                expected_drives=self.DRIVES,
                started_drives=self.DRIVES,
                completed_drives=self.DRIVES,
                live_slots=0,
                expected_files=self.DRIVES * self.FILES,
                completed_files=self.DRIVES * self.FILES,
                checked_files=self.DRIVES * self.FILES,
                expected_bytes=self.DRIVES * profile_bytes,
                completed_bytes=self.DRIVES * profile_bytes,
                compared_bytes=self.DRIVES * profile_bytes,
                complete=True,
                settled=True,
            )
            summary["pass"] = label
            raw = dict(summary, completed_drive_ids=list(range(self.DRIVES)))
            receipt = self.receipt(f"oracle-receipts/{label}.json", raw)
            summary.update(after_boundary_complete=True, receipt=receipt)
            self.terminal["fresh_oracle_passes"].append(summary)

            common = dict(
                controller_pid=self.CONTROLLER_PID,
                generation=0,
                sequence=sequence,
                phase=f"{label}_fresh_oracle",
                boundary="after",
                source_digest=self.terminal["source"]["digest"],
                binary_digest=self.terminal["source"]["binary_sha256"],
                catalog_digest="c" * 64,
                backend_prefix="synthetic-control-only",
            )
            frame = dict(
                identity=dict(common, pid=self.CONTROLLER_PID, server=None, role="controller"),
                capture_complete=True,
                metrics_complete=True,
            )
            controller = self.receipt(f"metrics/g0-s{sequence}.json.gz", frame)
            measured = []
            for worker in workers:
                server, pid = worker["server"], worker["pid"]
                frame = dict(
                    identity=dict(common, pid=pid, server=server, role="worker"),
                    capture_complete=True,
                    metrics_complete=True,
                )
                name = f"worker-{server}/metrics/g0-s{sequence}.json.gz"
                self.worker_metric_names.append(name)
                receipt = self.receipt(name, frame)
                receipt.update(server=server, pid=pid)
                measured.append(receipt)
            self.terminal["phase_metrics"]["boundaries"].append(dict(
                phase=f"{label}_fresh_oracle",
                boundary="after",
                generation=0,
                sequence=sequence,
                complete=True,
                metrics_complete=True,
                controller=controller,
                workers=measured,
            ))
        self.terminal["stages"] = []
        for cell, (mode, pattern) in enumerate(
            (mode, pattern)
            for mode in ("mostly_idle", "all_active")
            for pattern in PATTERNS
        ):
            phase = f"{mode}/{pattern}"
            sequences = [2 + 3 * cell + index for index in range(3)]
            after_identity = None
            for boundary, sequence in zip(
                ("before_active", "after_active", "after_idle"), sequences
            ):
                identity = dict(
                    common, sequence=sequence, phase=phase, boundary=boundary,
                    pid=self.CONTROLLER_PID, server=None, role="controller",
                )
                frame = dict(
                    identity=identity, capture_complete=True, metrics_complete=True,
                )
                controller = self.receipt(f"metrics/g0-s{sequence}.json.gz", frame)
                self.terminal["phase_metrics"]["boundaries"].append(dict(
                    phase=phase, boundary=boundary, generation=0, sequence=sequence,
                    complete=True, metrics_complete=True, controller=controller,
                ))
                if boundary == "after_idle":
                    after_identity = dict(identity, mode=mode, pattern=pattern)
            rows = [
                dict(lane=lane, quic=dict(
                    tx_bytes=2**53 + 1,
                    rx_bytes=2**64 - 1,
                    tx_datagrams=1000 + lane + cell,
                    rx_datagrams=2000 + lane + cell,
                    tx_ios=900 + lane + cell,
                    rx_ios=1800 + lane + cell,
                    lost_packets=(lane + cell) % 3,
                    lost_bytes=((lane + cell) % 3) * 1200,
                    sent_packets=2**64 - 2,
                    congestion_events=cell % 4,
                ))
                for lane in range(self.DRIVES)
            ]
            document = dict(
                schema=QUIC_SCHEMA, identity=after_identity, scope=QUIC_SCOPE,
                connections=rows,
            )
            receipt = self.receipt(f"quic/g0-s{sequences[2]}.json.gz", document)
            artifact = dict(
                receipt, schema=QUIC_SCHEMA, identity=after_identity, lanes=self.DRIVES,
            )
            active = min(max(self.DRIVES // 100, 1), 100) if mode == "mostly_idle" else self.DRIVES
            self.terminal["stages"].append(dict(
                mode=mode, pattern=pattern, metric_sequences=sequences,
                connected_clients=self.DRIVES, configured_active_clients=active,
                controller_quic_boundary=dict(scope=QUIC_SCOPE, artifact=artifact),
            ))
        self.publish_terminal()

    def write(self, name, value):
        plain = json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8")
        if len(plain) > DECODED_LIMIT:
            raise ValueError("synthetic fixture exceeds inherited decoded limit")
        data = gzip.compress(plain, mtime=0) if name.endswith(".gz") else plain
        if len(data) > STORED_LIMIT:
            raise ValueError("synthetic fixture exceeds inherited stored limit")
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        previous = path.stat().st_size if path.exists() else 0
        path.write_bytes(data)
        self.total_stored_bytes += len(data) - previous
        return data

    def receipt(self, name, value):
        data = self.write(name, value)
        if name not in self.receipt_names:
            self.receipt_names.append(name)
        return dict(file=name, sha256=hashlib.sha256(data).hexdigest())

    def shard(self, stage=0):
        artifact = self.terminal["stages"][stage]["controller_quic_boundary"]["artifact"]
        return json.loads(gzip.decompress((self.root / artifact["file"]).read_bytes()))

    def replace_shard(self, document, stage=0):
        artifact = self.terminal["stages"][stage]["controller_quic_boundary"]["artifact"]
        artifact["sha256"] = self.receipt(artifact["file"], document)["sha256"]

    def replace_shard_bytes(self, data, stage=0):
        artifact = self.terminal["stages"][stage]["controller_quic_boundary"]["artifact"]
        (self.root / artifact["file"]).write_bytes(data)
        artifact["sha256"] = hashlib.sha256(data).hexdigest()

    def publish_terminal(self):
        self.write("terminal.json", self.terminal)


class FullTargetGeometryRegression(AssertionError):
    """The one intended RED: reduced geometry accepted under the full label."""


class ProductionTargetEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="mount-rs-evidence-test-")
        self.addCleanup(self.directory.cleanup)
        self.fixture = SyntheticControl(Path(self.directory.name))

    def assert_rejected(self, mode="control"):
        self.fixture.publish_terminal()
        result = verify_fixture(self.fixture.root, mode)
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)

    def test_valid_synthetic_control_is_accepted(self):
        self.assertEqual(len(self.fixture.terminal["expected_state_receipts"]["packs"]), 1)
        self.assertEqual(self.fixture.terminal["expected_state_receipts"]["drive_count"], 10)
        self.assertEqual(len(self.fixture.terminal["fresh_oracle_passes"]), 2)
        self.assertEqual(len(self.fixture.worker_metric_names), 20)
        self.assertEqual(len(self.fixture.receipt_names), 25 + 4 * 2 * len(PATTERNS))
        self.assertEqual(len(self.fixture.terminal["stages"]), 16)
        first = self.fixture.shard()["connections"][0]["quic"]
        self.assertEqual(first["tx_bytes"], 2**53 + 1)
        self.assertEqual(first["rx_bytes"], 2**64 - 1)
        result = verify_fixture(self.fixture.root, "control")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_control_relabelled_full_is_rejected(self):
        self.fixture.terminal["full_target"] = True
        self.fixture.terminal["configuration"]["full_target"] = True
        self.fixture.publish_terminal()
        result = verify_fixture(self.fixture.root, "full")
        if result.returncode == 0:
            raise FullTargetGeometryRegression(GEOMETRY_FAILURE)
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)

    def test_changed_receipt_digest_is_rejected(self):
        self.fixture.terminal["expected_state_receipts"]["packs"][0]["sha256"] = "0" * 64
        self.assert_rejected()

    def test_missing_oracle_drive_roster_is_rejected(self):
        summary = self.fixture.terminal["fresh_oracle_passes"][0]
        path = self.fixture.root / summary["receipt"]["file"]
        raw = json.loads(path.read_bytes())
        raw["completed_drive_ids"].pop()
        summary["receipt"] = self.fixture.receipt(summary["receipt"]["file"], raw)
        self.assert_rejected()

    def test_unclean_worker_is_rejected(self):
        self.fixture.terminal["workers"][0]["reap_confirmed"] = False
        self.assert_rejected()

    def test_missing_quic_shard_is_rejected(self):
        artifact = self.fixture.terminal["stages"][0]["controller_quic_boundary"]["artifact"]
        (self.fixture.root / artifact["file"]).unlink()
        self.assert_rejected()

    def test_missing_quic_lane_is_rejected(self):
        document = self.fixture.shard()
        document["connections"].pop()
        self.fixture.replace_shard(document)
        self.assert_rejected()

    def test_duplicate_quic_lane_is_rejected(self):
        document = self.fixture.shard()
        document["connections"][-1]["lane"] = 0
        self.fixture.replace_shard(document)
        self.assert_rejected()

    def test_wrong_quic_lane_is_rejected(self):
        document = self.fixture.shard()
        document["connections"][-1]["lane"] = self.fixture.DRIVES
        self.fixture.replace_shard(document)
        self.assert_rejected()

    def test_changed_quic_shard_digest_is_rejected(self):
        self.fixture.terminal["stages"][0]["controller_quic_boundary"]["artifact"]["sha256"] = "0" * 64
        self.assert_rejected()

    def test_inline_quic_connections_are_rejected(self):
        boundary = self.fixture.terminal["stages"][0]["controller_quic_boundary"]
        boundary["connections"] = self.fixture.shard()["connections"]
        self.assert_rejected()

    def test_quic_envelope_extra_field_is_rejected(self):
        document = self.fixture.shard()
        document["unexpected"] = True
        self.fixture.replace_shard(document)
        self.assert_rejected()

    def test_quic_counter_shape_is_rejected(self):
        document = self.fixture.shard()
        document["connections"][0]["quic"]["unexpected"] = 0
        self.fixture.replace_shard(document)
        self.assert_rejected()

    def test_non_u64_quic_counters_are_rejected(self):
        for number in (True, 1.0, "9007199254740993", -1, 2**64):
            with self.subTest(number=number):
                document = self.fixture.shard()
                document["connections"][0]["quic"]["tx_bytes"] = number
                self.fixture.replace_shard(document)
                self.assert_rejected()

    def test_quic_stage_relabel_is_rejected(self):
        document = self.fixture.shard()
        document["identity"]["mode"] = "all_active"
        artifact = self.fixture.terminal["stages"][0]["controller_quic_boundary"]["artifact"]
        artifact["identity"]["mode"] = "all_active"
        self.fixture.replace_shard(document)
        self.assert_rejected()

    def test_quic_generation_relabel_is_rejected(self):
        document = self.fixture.shard()
        document["identity"]["generation"] = 1
        artifact = self.fixture.terminal["stages"][0]["controller_quic_boundary"]["artifact"]
        artifact["identity"]["generation"] = 1
        self.fixture.replace_shard(document)
        self.assert_rejected()

    def test_quic_source_relabel_is_rejected(self):
        document = self.fixture.shard()
        document["identity"]["source_digest"] = "0" * 64
        artifact = self.fixture.terminal["stages"][0]["controller_quic_boundary"]["artifact"]
        artifact["identity"]["source_digest"] = "0" * 64
        self.fixture.replace_shard(document)
        self.assert_rejected()

    def test_quic_nonconsecutive_sequences_are_rejected(self):
        self.fixture.terminal["stages"][0]["metric_sequences"][1] += 1
        self.assert_rejected()

    def test_quic_boundary_sequence_binding_is_rejected(self):
        self.fixture.terminal["phase_metrics"]["boundaries"][2]["sequence"] += 1
        self.assert_rejected()

    def test_quic_missing_controller_boundary_is_rejected(self):
        self.fixture.terminal["phase_metrics"]["boundaries"].pop(2)
        self.assert_rejected()

    def test_quic_duplicate_controller_boundary_is_rejected(self):
        boundaries = self.fixture.terminal["phase_metrics"]["boundaries"]
        boundaries.append(dict(boundaries[2]))
        self.assert_rejected()

    def test_quic_client_roster_is_rejected(self):
        self.fixture.terminal["stages"][0]["connected_clients"] -= 1
        self.assert_rejected()

    def test_quic_gzip_trailing_data_is_rejected(self):
        artifact = self.fixture.terminal["stages"][0]["controller_quic_boundary"]["artifact"]
        data = (self.fixture.root / artifact["file"]).read_bytes()
        self.fixture.replace_shard_bytes(data + b"trailing")
        self.assert_rejected()

    def test_quic_gzip_second_member_is_rejected(self):
        artifact = self.fixture.terminal["stages"][0]["controller_quic_boundary"]["artifact"]
        data = (self.fixture.root / artifact["file"]).read_bytes()
        self.fixture.replace_shard_bytes(data + gzip.compress(b"{}", mtime=0))
        self.assert_rejected()

    def test_quic_duplicate_json_key_is_rejected(self):
        document = self.fixture.shard()
        plain = json.dumps(document, sort_keys=True, separators=(",", ":")).encode()
        duplicated = plain.replace(b'"schema":', b'"schema":"duplicate","schema":', 1)
        self.fixture.replace_shard_bytes(gzip.compress(duplicated, mtime=0))
        self.assert_rejected()

    def test_missing_final_oracle_drive_roster_is_rejected(self):
        summary = self.fixture.terminal["fresh_oracle_passes"][1]
        path = self.fixture.root / summary["receipt"]["file"]
        raw = json.loads(path.read_bytes())
        raw["completed_drive_ids"].pop()
        summary["receipt"] = self.fixture.receipt(summary["receipt"]["file"], raw)
        self.assert_rejected()

    def test_both_fresh_oracle_worker_frames_remain_bound(self):
        for index in (0, 1):
            with self.subTest(pass_index=index):
                receipt = self.fixture.terminal["phase_metrics"]["boundaries"][index]["workers"][0]
                path = self.fixture.root / receipt["file"]
                frame = json.loads(gzip.decompress(path.read_bytes()))
                original = dict(frame["identity"])
                frame["identity"]["source_digest"] = "0" * 64
                receipt["sha256"] = self.fixture.receipt(receipt["file"], frame)["sha256"]
                self.assert_rejected()
                frame["identity"] = original
                receipt["sha256"] = self.fixture.receipt(receipt["file"], frame)["sha256"]

    def test_fresh_oracle_catalog_disagreement_is_rejected(self):
        boundary = self.fixture.terminal["phase_metrics"]["boundaries"][1]
        for receipt in [boundary["controller"]] + boundary["workers"]:
            path = self.fixture.root / receipt["file"]
            frame = json.loads(gzip.decompress(path.read_bytes()))
            frame["identity"]["catalog_digest"] = "d" * 64
            receipt["sha256"] = self.fixture.receipt(receipt["file"], frame)["sha256"]
        self.assert_rejected()

    def test_boolean_quic_lane_is_rejected(self):
        document = self.fixture.shard()
        document["connections"][0]["lane"] = False
        self.fixture.replace_shard(document)
        self.assert_rejected()

    def test_quic_stored_limit_is_enforced(self):
        document = self.fixture.shard()
        plain = json.dumps(document, sort_keys=True, separators=(",", ":")).encode()
        padded = b" " * (STORED_LIMIT + 1) + plain
        self.fixture.replace_shard_bytes(gzip.compress(padded, compresslevel=0, mtime=0))
        self.assert_rejected()

    def test_quic_decoded_limit_is_enforced(self):
        document = self.fixture.shard()
        plain = json.dumps(document, sort_keys=True, separators=(",", ":")).encode()
        padded = b" " * (DECODED_LIMIT + 1 - len(plain)) + plain
        self.fixture.replace_shard_bytes(gzip.compress(padded, mtime=0))
        self.assert_rejected()

    def test_missing_quic_cell_is_rejected(self):
        self.fixture.terminal["stages"].pop()
        self.assert_rejected()

    def test_reordered_quic_cells_are_rejected(self):
        stages = self.fixture.terminal["stages"]
        stages[0], stages[1] = stages[1], stages[0]
        self.assert_rejected()

    def test_duplicate_quic_cell_is_rejected(self):
        stages = self.fixture.terminal["stages"]
        stages[1] = dict(stages[0])
        self.assert_rejected()


class LedgerPackEvidenceTests(unittest.TestCase):
    def test_pack_boundaries_and_lossless_u64_sparse_state_join_both_oracles(self):
        for drives in (31,32,33):
            with self.subTest(drives=drives), tempfile.TemporaryDirectory() as directory:
                fixture=type("PackBoundary",(SyntheticControl,),dict(DRIVES=drives,FILES=1000))(Path(directory))
                receipt=fixture.terminal["expected_state_receipts"]["packs"][0]
                decoded=json.loads(gzip.decompress((fixture.root/receipt['file']).read_bytes()))
                ledger=decoded['ledgers'][0]
                ledger['generations']=[2**64-1 if index%2 else 2**53+7 for index in range(1534)]
                ledger['files']['renamed-é']=ledger['files'].pop('mixed-1')
                ledger['files']['mixed-0']['length']=8192
                ledger['files']['mixed-0']['changed']={'1':2**64-1}
                receipt.update(fixture.receipt(receipt['file'],decoded))
                # Update only final checked-byte totals; initial profile stays exact.
                fixture.terminal['verified_bytes']+=4096
                summary=fixture.terminal['fresh_oracle_passes'][1]
                for field in ('expected_bytes','completed_bytes','compared_bytes'): summary[field]+=4096
                raw=json.loads((fixture.root/summary['receipt']['file']).read_bytes())
                for field in ('expected_bytes','completed_bytes','compared_bytes'): raw[field]=summary[field]
                summary['receipt']=fixture.receipt(summary['receipt']['file'],raw)
                fixture.publish_terminal()
                result=verify_fixture(fixture.root,'control')
                self.assertEqual(result.returncode,0,result.stderr)
                restored=json.loads(gzip.decompress((fixture.root/receipt['file']).read_bytes()))
                self.assertEqual(restored,decoded)

    def test_rehashed_pack_corruption_cannot_replace_corpus_authority(self):
        faults=('empty','overfull','duplicate_drive','wrong_pack','wrong_first','missing_dense_slot',
                'float_generation','bool_generation','overflow_generation','wrong_oracle',
                'wrong_file_identity','noncanonical_sparse_key','dense_key_in_sparse_map',
                'sparse_past_length','missing_pack','duplicate_pack','wrong_receipt_range')
        for fault in faults:
            with self.subTest(fault=fault), tempfile.TemporaryDirectory() as directory:
                fixture=type("TwoPacks",(SyntheticControl,),dict(DRIVES=33))(Path(directory))
                inventory=fixture.terminal['expected_state_receipts']
                receipt=inventory['packs'][0]
                decoded=json.loads(gzip.decompress((fixture.root/receipt['file']).read_bytes()))
                ledger=decoded['ledgers'][0]
                if fault=='empty': decoded['ledgers']=[]
                elif fault=='overfull': decoded['ledgers'].append(decoded['ledgers'][0])
                elif fault=='duplicate_drive': decoded['ledgers'][1]['drive']=0
                elif fault=='wrong_pack': decoded['pack']=1
                elif fault=='wrong_first': decoded['first_drive']=32
                elif fault=='missing_dense_slot': ledger['generations'].pop()
                elif fault=='float_generation': ledger['generations'][0]=float(2**53)
                elif fault=='bool_generation': ledger['generations'][0]=True
                elif fault=='overflow_generation': ledger['generations'][0]=2**64
                elif fault=='wrong_oracle': ledger['oracle']='different oracle'
                elif fault=='wrong_file_identity': ledger['files']['mixed-0']['identity']=2
                elif fault=='noncanonical_sparse_key': ledger['files']['mixed-0']['changed']={'01':1}
                elif fault=='dense_key_in_sparse_map': ledger['files']['mixed-0']['changed']={'0':1}
                elif fault=='sparse_past_length': ledger['files']['mixed-0']['changed']={'1':1}
                elif fault=='missing_pack': inventory['packs'].pop()
                elif fault=='duplicate_pack': inventory['packs'][1]=dict(receipt)
                elif fault=='wrong_receipt_range': receipt['first_drive']=1
                receipt.update(fixture.receipt(receipt['file'],decoded))
                fixture.publish_terminal()
                self.assertEqual(verify_fixture(fixture.root,'control').returncode,2)

    def test_duplicate_json_keys_and_raw_hash_damage_are_rejected(self):
        for duplicate in (False,True):
            with self.subTest(duplicate=duplicate), tempfile.TemporaryDirectory() as directory:
                fixture=SyntheticControl(Path(directory))
                receipt=fixture.terminal['expected_state_receipts']['packs'][0]
                path=fixture.root/receipt['file']
                plain=gzip.decompress(path.read_bytes())
                if duplicate:
                    plain=plain.replace(b'"drive":0',b'"drive":0,"drive":0',1)
                    data=gzip.compress(plain,mtime=0)
                    receipt['sha256']=hashlib.sha256(data).hexdigest()
                else: data=path.read_bytes()+b'corrupt'
                path.write_bytes(data)
                fixture.publish_terminal()
                self.assertEqual(verify_fixture(fixture.root,'control').returncode,2)

    def test_each_fresh_oracle_roster_still_joins_complete_packed_corpus(self):
        for index in (0,1):
            with self.subTest(index=index), tempfile.TemporaryDirectory() as directory:
                fixture=SyntheticControl(Path(directory))
                summary=fixture.terminal['fresh_oracle_passes'][index]
                raw=json.loads((fixture.root/summary['receipt']['file']).read_bytes())
                raw['completed_drive_ids'][-1]=10000
                summary['receipt']=fixture.receipt(summary['receipt']['file'],raw)
                fixture.publish_terminal()
                self.assertEqual(verify_fixture(fixture.root,'control').returncode,2)


class FullLedgerCorpusTests(unittest.TestCase):
    def test_complete_10000_by_1000_corpus_and_both_fresh_oracle_joins(self):
        with tempfile.TemporaryDirectory(prefix='mount-rs-full-packed-ledger-') as directory:
            fixture=type('FullCorpus',(SyntheticControl,),dict(DRIVES=10000,FILES=1000))(Path(directory))
            fixture.terminal['full_target']=True
            fixture.terminal['configuration'].update(full_target=True,seconds=30)
            fixture.publish_terminal()
            self.assertEqual(len(fixture.terminal['expected_state_receipts']['packs']),313)
            self.assertEqual(fixture.terminal['namespace_files'],10000000)
            self.assertEqual(fixture.terminal['population_bytes'],62832640000)
            # This bounds a separate offline verifier subprocess, not the producer's
            # unchanged single 30-second terminal observation allowance.
            result=verify_fixture(fixture.root,'full',timeout=120)
            self.assertEqual(result.returncode,0,result.stderr)


class RecordingResult(unittest.TextTestResult):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.successes = set()

    def addSuccess(self, test):
        self.successes.add(test.id())
        super().addSuccess(test)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--expect-geometry-red", action="store_true",
        help="succeed only for the exact intended geometry failure and all other controls passing",
    )
    parser.add_argument('--full-ledger-corpus',action='store_true',
                        help='explicit offline 10000-Drive/F1000 joined artifact fixture')
    arguments = parser.parse_args()
    loader = unittest.TestLoader()
    suite = loader.loadTestsFromTestCase(ProductionTargetEvidenceTests)
    if arguments.full_ledger_corpus:
        suite=loader.loadTestsFromTestCase(FullLedgerCorpusTests)
    elif not arguments.expect_geometry_red:
        suite.addTests(loader.loadTestsFromTestCase(LedgerPackEvidenceTests))
    required = {
        ProductionTargetEvidenceTests(name).id()
        for name in loader.getTestCaseNames(ProductionTargetEvidenceTests)
    }
    geometry = ProductionTargetEvidenceTests("test_control_relabelled_full_is_rejected").id()
    result = unittest.TextTestRunner(verbosity=2, resultclass=RecordingResult).run(suite)
    if not arguments.expect_geometry_red:
        return 0 if result.wasSuccessful() else 1
    intended_failure = (
        len(result.failures) == 1
        and result.failures[0][0].id() == geometry
        and result.failures[0][1].rstrip().endswith(
            "FullTargetGeometryRegression: " + GEOMETRY_FAILURE
        )
    )
    qualified_red = (
        result.testsRun == len(required)
        and result.successes == required - {geometry}
        and intended_failure
        and not result.errors
        and not result.skipped
        and not result.expectedFailures
        and not result.unexpectedSuccesses
    )
    if qualified_red:
        print(SENTINEL + f": exact geometry RED with {len(required) - 1} passing synthetic controls")
        return 0
    print("Expected exactly the geometry RED and every other synthetic control passing", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
