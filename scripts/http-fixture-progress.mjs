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
