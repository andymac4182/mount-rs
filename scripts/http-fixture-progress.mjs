import { writeSync } from "node:fs";

const MAX_RECORDS = 16;
const MAX_RECORD_BYTES = 512;
const EVENTS = new Set([
  "launch_requested", "child_spawned", "stdout_seen", "stderr_seen",
  "ready_received", "watchdog_expired", "child_exited",
]);
const TERMINAL_EVENTS = new Set(["watchdog_expired", "child_exited", "observer_unavailable"]);

// A single bounded write catches descriptor errors locally; no asynchronous error
// listener, retry, timer or child ownership is added by this diagnostic sink.
function defaultWrite(line) {
  return writeSync(2, line) === Buffer.byteLength(line, "utf8");
}

export function createHttpFixtureProgress({
  clock = () => Number(process.hrtime.bigint() / 1_000_000n),
  write = defaultWrite,
} = {}) {
  const seen = new Set();
  let origin = null;
  let previous = null;
  let elapsed = null;
  let available = true;
  let reason = null;
  let attempted = 0;
  let successful = 0;
  let childState = "not_started";
  let stdoutSeen = false;
  let stderrSeen = false;
  let exitKind = null;
  let exitCode = null;

  function publish(event, failureReason = null) {
    // The current one-shot vocabulary has at most eight records. The hard cap
    // also reserves a slot for a terminal boundary if the vocabulary grows.
    const limit = TERMINAL_EVENTS.has(event) ? MAX_RECORDS : MAX_RECORDS - 1;
    if (attempted >= limit) return false;
    const record = {
      schema: "mount-rs.http-fixture-progress.v1",
      event,
      sequence: attempted + 1,
      elapsed_ms: elapsed,
      child_state: childState,
      stdout_seen: stdoutSeen,
      stderr_seen: stderrSeen,
      compilation_observation: "unobserved",
      runtime_entry_observation: "unobserved",
      exit_kind: exitKind,
      exit_code: exitCode,
      reason: failureReason,
    };
    const line = `${JSON.stringify(record)}\n`;
    if (Buffer.byteLength(line, "utf8") > MAX_RECORD_BYTES) return false;
    attempted++;
    try {
      if (write(line) === false) {
        unavailable("sink_backpressure", false);
        return false;
      }
      successful++;
      return true;
    } catch {
      unavailable("sink_failed", false);
      return false;
    }
  }

  function unavailable(failureReason, emit = true) {
    if (!available) return;
    available = false;
    reason = failureReason;
    if (emit) publish("observer_unavailable", failureReason);
  }

  function observe(event, code = null, signal = null) {
    if (!EVENTS.has(event)) {
      unavailable("invalid_event");
      return;
    }
    if (seen.has(event)) return;
    seen.add(event);
    if (event === "child_spawned" && childState !== "exited") childState = "spawned";
    if (event === "stdout_seen") stdoutSeen = true;
    if (event === "stderr_seen") stderrSeen = true;
    if (event === "child_exited") {
      childState = "exited";
      const validCode = Number.isSafeInteger(code) && code >= 0 && code <= 0xffff_ffff;
      if (typeof signal === "string" && signal.length > 0) {
        exitKind = "signal";
      } else if (validCode) {
        exitKind = code === 0 ? "success" : "nonzero";
        exitCode = code;
      }
    }
    if (!available) return;
    let now;
    try {
      now = clock();
    } catch {
      unavailable("clock_failed");
      return;
    }
    if (!Number.isSafeInteger(now) || now < 0) {
      unavailable("clock_invalid");
      return;
    }
    if (previous !== null && now < previous) {
      unavailable("clock_regressed");
      return;
    }
    origin ??= now;
    previous = now;
    elapsed = now - origin;
    if (!publish(event)) unavailable("publication_cap");
  }

  return {
    observe,
    receipt: () => Object.freeze({
      available, reason, attempted_records: attempted, successful_writes: successful,
      child_state: childState, stdout_seen: stdoutSeen, stderr_seen: stderrSeen,
      exit_kind: exitKind, exit_code: exitCode,
    }),
  };
}

export function attachHttpFixtureProgress(child, lines, progress) {
  child.once("spawn", () => progress.observe("child_spawned"));
  // Independent of the removable pre-readiness rejection listener, so normal
  // shutdown after readiness remains observable without owning the child.
  child.once("exit", (code, signal) => progress.observe("child_exited", code, signal));
  lines.once("line", () => progress.observe("stdout_seen"));
  child.stderr.once("data", () => progress.observe("stderr_seen"));
}

const CHILD_STAGES = new Set([
  "main_body_entered", "s3_create_started", "s3_create_completed",
  "webdav_create_started", "webdav_create_completed", "webdav_listen_started",
  "webdav_listen_completed", "ready_published", "observer_unavailable",
]);
const CHILD_KEYS = new Set([
  "schema", "pid", "scope", "stage", "sequence", "elapsed_ms",
  "availability", "reason", "compiler_observation",
]);
const CHILD_REASONS = new Set(["clock_invalid", "clock_regressed", "publication_cap"]);

// These are closed child self-reports, not an OS verification of the child PID.
// Keep raw stderr in the existing launcher collector; relay only fixed records.
export function createHttpCheckpointRelay({ write = defaultWrite } = {}) {
  const buffer = Buffer.alloc(MAX_RECORD_BYTES - 1);
  const seen = new Set();
  let length = 0;
  let oversized = false;
  let available = true;
  let reason = null;
  let attempted = 0;
  let successful = 0;
  let refused = 0;
  let selfReportedPid = null;
  let previousSequence = 0;
  let previousElapsed = null;
  let childUnavailable = false;

  function refuse() {
    refused++;
  }

  function acceptLine() {
    let record;
    try {
      record = JSON.parse(buffer.toString("utf8", 0, length));
    } catch {
      refuse();
      return;
    }
    const validObject = record !== null && typeof record === "object" && !Array.isArray(record);
    if (!validObject || Object.keys(record).length !== CHILD_KEYS.size || Object.keys(record).some((key) => !CHILD_KEYS.has(key))) {
      refuse();
      return;
    }
    const validElapsed = Number.isSafeInteger(record.elapsed_ms) && record.elapsed_ms >= 0;
    const unavailableRecord = record.stage === "observer_unavailable" && record.availability === "unavailable" && CHILD_REASONS.has(record.reason);
    const observedRecord = record.stage !== "observer_unavailable" && record.availability === "observed" && record.reason === null && validElapsed;
    if (record.schema !== "mount-rs.http-oracle-startup.v1" || record.scope !== "application_async_main_body" || record.compiler_observation !== "unobserved" ||
        !Number.isSafeInteger(record.pid) || record.pid <= 0 || record.pid > 0xffff_ffff ||
        !Number.isSafeInteger(record.sequence) || record.sequence <= previousSequence || record.sequence > MAX_RECORDS ||
        !CHILD_STAGES.has(record.stage) || seen.has(record.stage) || childUnavailable ||
        (selfReportedPid !== null && record.pid !== selfReportedPid) ||
        (!validElapsed && !(unavailableRecord && record.elapsed_ms === null)) ||
        (previousElapsed !== null && (record.elapsed_ms === null || record.elapsed_ms < previousElapsed)) ||
        (!unavailableRecord && !observedRecord)) {
      refuse();
      return;
    }
    const line = `${JSON.stringify(record)}\n`;
    if (Buffer.byteLength(line, "utf8") > MAX_RECORD_BYTES) {
      refuse();
      return;
    }
    if (attempted >= MAX_RECORDS) {
      available = false;
      reason = "publication_cap";
      return;
    }
    attempted++;
    try {
      if (write(line) === false) {
        available = false;
        reason = "sink_short_write";
        return;
      }
    } catch {
      available = false;
      reason = "sink_failed";
      return;
    }
    successful++;
    selfReportedPid ??= record.pid;
    previousSequence = record.sequence;
    previousElapsed = record.elapsed_ms;
    childUnavailable = unavailableRecord;
    seen.add(record.stage);
  }

  return {
    push(chunk) {
      if (!available) return;
      if (!(chunk instanceof Uint8Array)) {
        refuse();
        return;
      }
      for (const byte of chunk) {
        if (byte === 10) {
          if (oversized) refuse();
          else if (length > 0) acceptLine();
          length = 0;
          oversized = false;
          if (!available) return;
        } else if (!oversized) {
          if (length === buffer.length) {
            oversized = true;
            length = 0;
          } else {
            buffer[length++] = byte;
          }
        }
      }
    },
    receipt: () => Object.freeze({
      available, reason, attempted_records: attempted, successful_writes: successful,
      refused_records: refused, self_reported_pid: selfReportedPid,
    }),
  };
}
