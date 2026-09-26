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


def parse_line(raw):
    if raw.startswith(STARTUP_PREFIX):
        prefix, limit, validator = STARTUP_PREFIX, 16 * 1024, validate_startup
    elif raw.startswith(TARGET_PREFIX):
        prefix, limit, validator = TARGET_PREFIX, 4 * 1024, validate_target
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


def summary():
    return dict(schema="mount-rs.startup-log-filter.v1", status="ok", first_issue=None,
                input_bytes=0, private_log_bytes=0, startup_records=0, target_records=0,
                ignored_lines=0, identity_verified=False)


def filter_stream(source, private, public, *, expected_target=None,
                  max_line_bytes=MAX_LINE_BYTES, max_log_bytes=MAX_LOG_BYTES):
    result = summary()
    pending = bytearray()
    discarding = False
    private_failed = public_failed = False
    identity = TargetIdentity(expected_target) if expected_target is not None else None
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
            if not record["accounting_complete"]:
                issue("diagnostic_accounting_incomplete")
            if public_failed:
                return
            output = prefix + json.dumps(record, separators=(",", ":"), ensure_ascii=True) + "\n"
            written = public.write(output)
            if written is not None and written != len(output):
                raise OSError()
            public.flush()
            field = "startup_records" if prefix == STARTUP_PREFIX.decode("ascii") else "target_records"
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
