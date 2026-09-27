#!/usr/bin/env python3
"""Retain a bounded private stream and forward only fixed safe diagnostics."""

import json
import os
import re
import stat
import sys

CHUNK_BYTES = 8192
MAX_LINE_BYTES = 1024 * 1024
MAX_LOG_BYTES = 64 * 1024 * 1024
U64_MAX = (1 << 64) - 1
STARTUP_PREFIX = b"startup_diagnostics "
TARGET_PREFIX = b"target_progress "
RESOURCE_PREFIX = b"resource_progress "
STAGES = (
    "configuration", "catalog_open", "catalog_load", "catalog_validate",
    "tls_material", "cache_start", "drive_config", "drive_open", "backing_receipt",
    "drive_register", "listener_bind", "ready", "cleanup",
)
PATTERNS = (
    "sequential_read", "random_read", "sequential_overwrite", "random_overwrite",
    "mixed", "hot_file", "append_truncate", "churn",
)
TARGET_PHASES = frozenset((
    "preflight", "empty_drive_initialization", "worker_setup", "injected_timeout",
    "signed_connections", "online_namespace", "online_payload", "initial_fresh_oracle",
    "refresh_replicas", "routes_and_scope", "final_fresh_oracle", "revocation", "terminal",
)) | frozenset(f"{mode}/{pattern}" for mode in ("mostly_idle", "all_active") for pattern in PATTERNS)
STARTUP_FIELDS = frozenset((
    "schema", "pid", "worker", "generation", "observed_unix_ms", "current_stage",
    "terminal_outcome", "cleanup_outcome", "configured_partitions", "planned_drives",
    "open_started", "open_success", "open_error", "open_cancelled", "open_in_flight",
    "registered_drives", "elapsed_ns", "current_stage_elapsed_ns", "accounting_complete",
    "banks_captured", "stages",
))
STAGE_FIELDS = frozenset((
    "stage", "started", "success", "error", "cancelled", "in_flight", "elapsed_ns", "max_ns",
))
TARGET_FIELDS = frozenset((
    "schema", "event", "pid", "elapsed_ns", "mode", "provider", "servers", "clients", "drives",
    "partitions", "files_per_drive", "phase_seconds", "phase", "source_revision", "source_verified",
    "host_free_bytes", "initialized_drives", "connected_clients", "outcome", "accounting_complete",
))
IDENTITY_FIELDS = (
    "mode", "provider", "servers", "clients", "drives", "partitions", "files_per_drive", "phase_seconds",
)
REVISION = re.compile(r"[0-9a-f]{40}\Z")
DIGEST = re.compile(r"[0-9a-f]{64}\Z")
RESOURCE_FIELDS = frozenset((
    "schema", "controller_pid", "source_revision", "source_digest", "binary_sha256", "role",
    "pid", "worker", "generation_context", "phase", "observed_unix_ms", "published_unix_ms",
    "samples", "sample_interval_ms", "terminal_sample", "available", "reason", "counter_scope",
    "cpu_user_us", "cpu_system_us", "rss_current_bytes", "rss_lifetime_peak_bytes", "rss_peak_bytes",
    "minimum_host_free_bytes", "block_inputs", "block_outputs", "process_disk_read_bytes",
    "process_disk_write_bytes", "process_disk_bytes_reason",
))
RESOURCE_MEASUREMENTS = (
    "cpu_user_us", "cpu_system_us", "rss_current_bytes", "rss_lifetime_peak_bytes", "rss_peak_bytes",
    "minimum_host_free_bytes", "block_inputs", "block_outputs",
)
RESOURCE_SAMPLE_FIELDS = ("observed_unix_ms", "samples", "terminal_sample", *RESOURCE_MEASUREMENTS)
RESOURCE_REASONS = frozenset((
    "missing_sample", "invalid_sample", "stale_sample", "foreign_pid", "resource_validation_failed",
))


class Rejected(Exception):
    """Fixed public reason only; never attach input or exception text."""


def require(condition, reason="invalid_record"):
    if not condition:
        raise Rejected(reason)


def exact_fields(value, names):
    require(type(value) is dict and value.keys() == names)


def unsigned(value, maximum=U64_MAX, minimum=0):
    require(type(value) is int and minimum <= value <= maximum)


def nullable_unsigned(value):
    if value is not None:
        unsigned(value)


def boolean(value):
    require(type(value) is bool)


def enumeration(value, choices):
    require(type(value) is str and value in choices)


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, "duplicate_key")
        result[key] = value
    return result


def reject_constant(unused):
    raise Rejected("invalid_json")


def validate_startup(record):
    exact_fields(record, STARTUP_FIELDS)
    require(record["schema"] == "mount-rs.startup.v1")
    unsigned(record["pid"], (1 << 32) - 1, 1)
    if record["worker"] is not None:
        unsigned(record["worker"], 9)
    for field in ("generation", "observed_unix_ms", "open_started", "open_success", "open_error",
                  "open_cancelled", "open_in_flight", "registered_drives", "elapsed_ns",
                  "current_stage_elapsed_ns"):
        unsigned(record[field])
    for field in ("configured_partitions", "planned_drives"):
        nullable_unsigned(record[field])
    enumeration(record["current_stage"], STAGES)
    enumeration(record["terminal_outcome"], ("running", "ready", "error", "cancelled"))
    if record["cleanup_outcome"] is not None:
        enumeration(record["cleanup_outcome"], ("success", "error", "cancelled"))
    boolean(record["accounting_complete"])
    require(record["banks_captured"] is False)
    rows = record["stages"]
    require(type(rows) is list and len(rows) == len(STAGES))
    for row, stage in zip(rows, STAGES):
        exact_fields(row, STAGE_FIELDS)
        require(row["stage"] == stage)
        for field in STAGE_FIELDS - {"stage"}:
            unsigned(row[field])
        if record["accounting_complete"]:
            require(row["started"] == sum(row[field] for field in
                                          ("success", "error", "cancelled", "in_flight")),
                    "invalid_accounting")
            require(row["max_ns"] <= row["elapsed_ns"], "invalid_accounting")
    open_row = rows[STAGES.index("drive_open")]
    require(all(record["open_" + field] == open_row[field] for field in
                ("started", "success", "error", "cancelled", "in_flight")), "invalid_accounting")
    require(record["registered_drives"] <= record["open_success"], "invalid_accounting")
    if record["accounting_complete"]:
        require(record["open_started"] == sum(record[field] for field in
                ("open_success", "open_error", "open_cancelled", "open_in_flight")), "invalid_accounting")
    if record["terminal_outcome"] == "ready" and record["accounting_complete"]:
        require(record["planned_drives"] is not None
                and record["registered_drives"] == record["open_success"] == record["open_started"]
                == record["planned_drives"]
                and record["open_error"] == record["open_cancelled"] == record["open_in_flight"] == 0,
                "invalid_accounting")


def validate_target(record):
    exact_fields(record, TARGET_FIELDS)
    require(record["schema"] == "mount-rs.target-progress.v1")
    enumeration(record["event"], ("controller_start", "source_verified", "capacity", "phase", "progress", "terminal"))
    unsigned(record["pid"], (1 << 32) - 1, 1)
    for field in ("elapsed_ns", "servers", "clients", "drives", "partitions", "files_per_drive",
                  "phase_seconds", "initialized_drives", "connected_clients"):
        unsigned(record[field])
    enumeration(record["mode"], ("full", "control"))
    enumeration(record["provider"], ("sqlite", "tidb"))
    enumeration(record["phase"], TARGET_PHASES)
    revision = record["source_revision"]
    require(revision is None or (type(revision) is str and REVISION.fullmatch(revision)))
    boolean(record["source_verified"])
    require(record["source_verified"] == (revision is not None))
    nullable_unsigned(record["host_free_bytes"])
    enumeration(record["outcome"], ("running", "success", "error", "cancelled"))
    boolean(record["accounting_complete"])
    require(record["servers"] == 10 and 10 <= record["drives"] <= 10000 and record["drives"] % 2 == 0
            and record["clients"] == record["drives"] and record["partitions"] == record["drives"] // 2
            and 1 <= record["files_per_drive"] <= 1000 and 1 <= record["phase_seconds"] <= 30)
    if record["mode"] == "full":
        require(record["drives"] == 10000 and record["files_per_drive"] == 1000 and record["phase_seconds"] == 30)
    require(record["initialized_drives"] <= record["drives"] and record["connected_clients"] <= record["clients"])
    if record["event"] == "controller_start":
        require(record["phase"] == "preflight" and not record["source_verified"]
                and record["outcome"] == "running")
    if record["event"] == "source_verified":
        require(record["source_verified"])
    if record["event"] == "terminal":
        require(record["outcome"] != "running")
        require(record["outcome"] != "success" or record["accounting_complete"], "invalid_accounting")


def validate_resource(record):
    exact_fields(record, RESOURCE_FIELDS)
    require(record["schema"] == "mount-rs.resource-progress.v1")
    for field in ("controller_pid", "pid"):
        unsigned(record[field], (1 << 32) - 1, 1)
    for field, pattern in (("source_revision", REVISION), ("source_digest", DIGEST),
                           ("binary_sha256", DIGEST)):
        require(type(record[field]) is str and pattern.fullmatch(record[field]))
    enumeration(record["role"], ("controller", "worker"))
    if record["role"] == "controller":
        require(record["worker"] is None and record["generation_context"] is None)
        require(record["pid"] == record["controller_pid"], "resource_identity_mismatch")
    else:
        unsigned(record["worker"], 9)
        nullable_unsigned(record["generation_context"])
        require(record["pid"] != record["controller_pid"], "resource_identity_mismatch")
    enumeration(record["phase"], TARGET_PHASES)
    unsigned(record["published_unix_ms"], minimum=1)
    unsigned(record["sample_interval_ms"], 100, 100)
    require(record["counter_scope"] == "sampler_baseline_cumulative_process")
    require(record["process_disk_read_bytes"] is None and record["process_disk_write_bytes"] is None
            and record["process_disk_bytes_reason"] == "not_captured_by_sampler")
    boolean(record["available"])
    if not record["available"]:
        enumeration(record["reason"], RESOURCE_REASONS)
        require(all(record[field] is None for field in RESOURCE_SAMPLE_FIELDS))
        return
    require(record["reason"] is None)
    unsigned(record["samples"], minimum=1)
    if record["terminal_sample"] is None:
        require(record["samples"] == 1)
    else:
        boolean(record["terminal_sample"])
    unsigned(record["observed_unix_ms"], minimum=1)
    require(record["observed_unix_ms"] <= record["published_unix_ms"]
            and record["published_unix_ms"] - record["observed_unix_ms"] <= 10000)
    for field in RESOURCE_MEASUREMENTS:
        unsigned(record[field])
    require(record["rss_peak_bytes"] >= max(record["rss_current_bytes"], record["rss_lifetime_peak_bytes"]))


def parse_line(raw):
    if raw.startswith(STARTUP_PREFIX):
        prefix, limit, validator = STARTUP_PREFIX, 16 * 1024, validate_startup
    elif raw.startswith(TARGET_PREFIX):
        prefix, limit, validator = TARGET_PREFIX, 4 * 1024, validate_target
    elif raw.startswith(RESOURCE_PREFIX):
        # Include the newline stripped by the framing loop in this contract.
        prefix, limit, validator = RESOURCE_PREFIX, 2047, validate_resource
    else:
        return None
    require(len(raw) <= limit, "record_limit_exceeded")
    try:
        record = json.loads(raw[len(prefix):].decode("utf-8"), object_pairs_hook=unique_object,
                            parse_constant=reject_constant)
    except (ValueError, UnicodeError, RecursionError):
        raise Rejected("invalid_json") from None
    validator(record)
    return prefix.decode("ascii"), record


class TargetIdentity:
    def __init__(self, expected):
        self.expected = expected
        self.pid = None
        self.verified = False
        self.terminal = False

    def accept(self, record):
        require(not self.terminal, "target_identity_mismatch")
        require(all(record[field] == self.expected[field] for field in IDENTITY_FIELDS),
                "target_identity_mismatch")
        event = record["event"]
        if self.pid is None:
            require(event == "controller_start", "target_identity_mismatch")
            self.pid = record["pid"]
            return
        require(record["pid"] == self.pid and event != "controller_start", "target_identity_mismatch")
        if event == "source_verified":
            require(not self.verified, "target_identity_mismatch")
            require(record["source_revision"] == self.expected["source_revision"], "target_identity_mismatch")
            self.verified = True
        if self.verified:
            require(record["source_verified"] and record["source_revision"] == self.expected["source_revision"],
                    "target_identity_mismatch")
        else:
            require(not record["source_verified"], "target_identity_mismatch")
        if event == "terminal":
            self.terminal = True

    def complete(self):
        return self.pid is not None and self.verified and self.terminal


class ResourceIdentity:
    """Bounded in-stream associations; source artifacts still need a separate digest join."""

    def __init__(self):
        self.controller_pid = None
        self.revision = None
        self.phase = None
        self.terminal = False
        self.digests = None
        self.workers = {}
        self.startups = {}
        self.previous = {}

    def target(self, record):
        if record["event"] == "controller_start":
            require(self.controller_pid is None, "resource_identity_mismatch")
            self.controller_pid = record["pid"]
        if self.controller_pid is not None:
            require(record["pid"] == self.controller_pid, "resource_identity_mismatch")
            self.phase = record["phase"]
            if record["event"] == "source_verified":
                require(self.revision is None, "resource_identity_mismatch")
                self.revision = record["source_revision"]
            if self.revision is not None:
                require(record["source_revision"] == self.revision, "resource_identity_mismatch")
            self.terminal |= record["event"] == "terminal"

    def startup(self, record):
        worker = record["worker"]
        if worker is None:
            return
        previous = self.startups.get(worker)
        if previous is not None:
            require(record["pid"] == previous[0] and record["generation"] >= previous[1],
                    "resource_identity_mismatch")
        if worker in self.workers:
            require(record["pid"] == self.workers[worker], "resource_identity_mismatch")
        self.startups[worker] = (record["pid"], record["generation"])

    def accept(self, record):
        require(self.revision is not None and not self.terminal
                and record["controller_pid"] == self.controller_pid
                and record["source_revision"] == self.revision
                and record["phase"] == self.phase, "resource_identity_mismatch")
        digests = (record["source_digest"], record["binary_sha256"])
        require(self.digests is None or self.digests == digests, "resource_identity_mismatch")
        worker, pid = record["worker"], record["pid"]
        if worker is not None:
            require(worker not in self.workers or self.workers[worker] == pid, "resource_identity_mismatch")
            require(all(index == worker or other != pid for index, other in self.workers.items()),
                    "resource_identity_mismatch")
            startup = self.startups.get(worker)
            require(startup is None or startup[0] == pid, "resource_identity_mismatch")
            context = record["generation_context"]
            if context is not None:
                require(self.startups.get(worker) == (pid, context), "resource_identity_mismatch")
        previous = self.previous.get(pid)
        if record["available"] and previous is not None:
            for field in ("samples", "cpu_user_us", "cpu_system_us", "block_inputs", "block_outputs",
                          "rss_peak_bytes", "rss_lifetime_peak_bytes"):
                require(record[field] >= previous[field], "resource_counter_regressed")
            require(record["minimum_host_free_bytes"] <= previous["minimum_host_free_bytes"],
                    "resource_counter_regressed")
            require(previous["terminal_sample"] is not True or record["terminal_sample"] is True,
                    "resource_counter_regressed")
        self.digests = digests
        if worker is not None:
            self.workers[worker] = pid
        if record["available"]:
            self.previous[pid] = record


def summary():
    return dict(schema="mount-rs.startup-log-filter.v2", status="ok", first_issue=None,
                input_bytes=0, private_log_bytes=0, startup_records=0, target_records=0,
                resource_records=0, ignored_lines=0, identity_verified=False)


def filter_stream(source, private, public, *, expected_target=None,
                  max_line_bytes=MAX_LINE_BYTES, max_log_bytes=MAX_LOG_BYTES):
    result = summary()
    pending = bytearray()
    discarding = False
    private_failed = public_failed = False
    identity = TargetIdentity(expected_target) if expected_target is not None else None
    resource_identity = ResourceIdentity()
    read = source.read1 if hasattr(source, "read1") else source.read

    def issue(reason):
        result["status"] = "failed"
        if result["first_issue"] is None:
            result["first_issue"] = reason

    def forward(raw):
        nonlocal public_failed
        try:
            parsed = parse_line(raw)
            if parsed is None:
                result["ignored_lines"] += 1
                return
            prefix, record = parsed
            if identity is not None and prefix == TARGET_PREFIX.decode("ascii"):
                identity.accept(record)
            if prefix == TARGET_PREFIX.decode("ascii"):
                resource_identity.target(record)
            elif prefix == STARTUP_PREFIX.decode("ascii"):
                resource_identity.startup(record)
            else:
                resource_identity.accept(record)
            if prefix == RESOURCE_PREFIX.decode("ascii") and not record["available"]:
                issue("resource_observation_unavailable")
            elif prefix != RESOURCE_PREFIX.decode("ascii") and not record["accounting_complete"]:
                issue("diagnostic_accounting_incomplete")
            if public_failed:
                return
            output = prefix + json.dumps(record, separators=(",", ":"), ensure_ascii=True) + "\n"
            written = public.write(output)
            if written is not None and written != len(output):
                raise OSError()
            public.flush()
            field = {STARTUP_PREFIX.decode("ascii"): "startup_records",
                     TARGET_PREFIX.decode("ascii"): "target_records",
                     RESOURCE_PREFIX.decode("ascii"): "resource_records"}[prefix]
            result[field] += 1
        except Rejected as error:
            issue(error.args[0])
        except (OSError, ValueError):
            public_failed = True
            issue("public_output_failed")

    while True:
        try:
            # BufferedReader.read may wait for the entire requested size. read1
            # exposes an available flushed pipe frame while its producer lives.
            chunk = read(CHUNK_BYTES)
        except (OSError, ValueError):
            issue("input_read_failed")
            break
        if not chunk:
            break
        before = result["input_bytes"]
        result["input_bytes"] = min(U64_MAX, before + len(chunk))
        permitted = chunk[:max(0, max_log_bytes - before)]
        if len(permitted) < len(chunk):
            issue("log_limit_exceeded")
        if not private_failed and permitted:
            try:
                written = private.write(permitted)
                if written is None:
                    written = len(permitted)
                if type(written) is not int or not 0 <= written <= len(permitted):
                    raise OSError()
                result["private_log_bytes"] += written
                if written != len(permitted):
                    raise OSError()
            except (OSError, ValueError):
                private_failed = True
                issue("private_log_write_failed")
        pieces = permitted.split(b"\n")
        for index, piece in enumerate(pieces):
            ended = index != len(pieces) - 1
            if not discarding:
                if len(pending) + len(piece) > max_line_bytes:
                    issue("line_limit_exceeded")
                    pending.clear()
                    discarding = True
                else:
                    pending.extend(piece)
            if ended:
                if not discarding:
                    forward(bytes(pending))
                pending.clear()
                discarding = False
    if pending or discarding:
        issue("truncated_line")
    for destination, reason in ((private, "private_log_write_failed"), (public, "public_output_failed")):
        try:
            destination.flush()
        except (OSError, ValueError):
            issue(reason)
    if identity is not None:
        if not identity.complete():
            issue("target_identity_incomplete")
        result["identity_verified"] = identity.complete() and result["status"] == "ok"
    return result


def arguments(argv):
    values = {}
    required = False
    options = {"--private-log": "private_log", "--expected-source-revision": "source_revision"}
    options.update({f"--expected-{field.replace('_', '-')}": field for field in IDENTITY_FIELDS})
    index = 0
    while index < len(argv):
        option = argv[index]
        if option == "--require-target-identity" and not required:
            required = True
            index += 1
            continue
        require(option in options and index + 1 < len(argv), "invalid_arguments")
        key = options[option]
        require(key not in values, "invalid_arguments")
        values[key] = argv[index + 1]
        index += 2
    require(bool(values.get("private_log")), "invalid_arguments")
    if not required:
        require(values.keys() == {"private_log"}, "invalid_arguments")
        return values["private_log"], None
    require(values.keys() == {"private_log", "source_revision", *IDENTITY_FIELDS}, "invalid_arguments")
    require(REVISION.fullmatch(values["source_revision"]), "invalid_arguments")
    require(values["mode"] in ("full", "control") and values["provider"] in ("sqlite", "tidb"), "invalid_arguments")
    for field in IDENTITY_FIELDS[2:]:
        value = values[field]
        require(value.isascii() and value.isdecimal(), "invalid_arguments")
        number = int(value)
        require(0 < number <= U64_MAX, "invalid_arguments")
        values[field] = number
    return values.pop("private_log"), values


def failed_drain(source, reason):
    result = summary()
    result.update(status="failed", first_issue=reason)
    read = source.read1 if hasattr(source, "read1") else source.read
    try:
        while chunk := read(CHUNK_BYTES):
            result["input_bytes"] = min(U64_MAX, result["input_bytes"] + len(chunk))
    except (OSError, ValueError):
        pass
    return result


def main(argv=None):
    try:
        path, expected = arguments(sys.argv[1:] if argv is None else argv)
    except (Rejected, ValueError, OverflowError):
        result = failed_drain(sys.stdin.buffer, "invalid_arguments")
    else:
        try:
            descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL
                                 | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0), 0o600)
            try:
                require(stat.S_ISREG(os.fstat(descriptor).st_mode), "private_log_unavailable")
                os.fchmod(descriptor, 0o600)
                private = os.fdopen(descriptor, "wb", buffering=0)
            except BaseException:
                os.close(descriptor)
                raise
        except (OSError, Rejected, ValueError):
            result = failed_drain(sys.stdin.buffer, "private_log_unavailable")
        else:
            try:
                result = filter_stream(sys.stdin.buffer, private, sys.stdout, expected_target=expected)
            except Exception:
                # A last-resort guard prevents any library exception from
                # exposing the private path or stream in a public traceback.
                result = failed_drain(sys.stdin.buffer, "internal_filter_failed")
            finally:
                try:
                    private.close()
                except (OSError, ValueError):
                    result["status"] = "failed"
                    if result["first_issue"] is None:
                        result["first_issue"] = "private_log_write_failed"
    try:
        sys.stderr.write(json.dumps(result, separators=(",", ":")) + "\n")
        sys.stderr.flush()
    except (OSError, ValueError):
        return 1
    return 0 if result["status"] == "ok" else 1


if __name__ == "__main__":
    raise SystemExit(main())
