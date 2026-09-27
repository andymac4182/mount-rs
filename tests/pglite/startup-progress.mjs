import { writeSync } from "node:fs";

const STAGES = new Set([
  "module_body_entered", "pglite_import_started", "pglite_import_completed",
  "socket_import_started", "socket_import_completed", "configuration_ready",
  "cleanup_install_started", "cleanup_install_completed", "engine_create_started",
  "engine_create_completed", "socket_construct_started", "socket_construct_completed",
  "socket_start_started", "socket_start_completed", "ready_published",
]);
const MAX_RECORDS = 16;
const MAX_BYTES = 512;

function monotonicMilliseconds() {
  return Number(process.hrtime.bigint() / 1_000_000n);
}

function writeStderr(line) {
  // One synchronous write bounds bytes, not sink latency. A short or failed
  // write disables reporting without retries or a process-level listener.
  return writeSync(2, line) === Buffer.byteLength(line, "utf8");
}

export function createPgliteStartupProgress({
  enabled = false,
  clock = monotonicMilliseconds,
  write = writeStderr,
} = {}) {
  const reportingEnabled = enabled === true;
  let active = reportingEnabled;
  let availability = active ? "observed" : "unavailable";
  let reason = null;
  let origin;
  let previous;
  let elapsed = null;
  let attemptedRecords = 0;
  let successfulWrites = 0;
  let lastStage = null;
  const seen = new Set();

  function publish(stage) {
    if (attemptedRecords >= MAX_RECORDS) return;
    const line = `${JSON.stringify({
      schema: "mount-rs.pglite-startup.v1",
      pid: process.pid,
      scope: "node_module_body",
      stage,
      sequence: attemptedRecords + 1,
      elapsed_ms: elapsed,
      availability,
      reason,
    })}\n`;
    if (Buffer.byteLength(line, "utf8") > MAX_BYTES) {
      active = false;
      availability = "unavailable";
      reason = "publication_cap";
      return;
    }
    attemptedRecords++;
    lastStage = stage;
    try {
      if (write(line) !== true) {
        active = false;
        availability = "unavailable";
        reason = "sink_short_write";
        return;
      }
      successfulWrites++;
    } catch {
      active = false;
      availability = "unavailable";
      reason = "sink_failed";
    }
  }

  function unavailable(fixedReason) {
    active = false;
    availability = "unavailable";
    reason = fixedReason;
    publish("observer_unavailable");
  }

  function observe(stage) {
    if (!active) return;
    if (!STAGES.has(stage)) {
      unavailable("invalid_stage");
      return;
    }
    if (seen.has(stage)) return;
    // Fifteen ordinary records leave one slot for observer unavailability.
    if (attemptedRecords >= MAX_RECORDS - 1) {
      unavailable("publication_cap");
      return;
    }
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
    if (previous !== undefined && now < previous) {
      unavailable("clock_regressed");
      return;
    }
    origin ??= now;
    previous = now;
    elapsed = now - origin;
    seen.add(stage);
    publish(stage);
  }

  return Object.freeze({
    observe,
    receipt: () => Object.freeze({
      enabled: reportingEnabled,
      availability,
      reason,
      attempted_records: attemptedRecords,
      successful_writes: successfulWrites,
      last_stage: lastStage,
    }),
  });
}

export async function loadPgliteStartupImports(progress, importModule = (specifier) => import(specifier)) {
  progress.observe("pglite_import_started");
  const { PGlite } = await importModule("@electric-sql/pglite");
  progress.observe("pglite_import_completed");
  progress.observe("socket_import_started");
  const { PGLiteSocketHandler, PGLiteSocketServer } = await importModule("@electric-sql/pglite-socket");
  progress.observe("socket_import_completed");
  return { PGlite, PGLiteSocketHandler, PGLiteSocketServer };
}
