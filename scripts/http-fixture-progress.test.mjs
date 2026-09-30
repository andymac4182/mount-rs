import assert from "node:assert/strict";
import { test } from "node:test";
import { EventEmitter } from "node:events";
import { createHttpFixtureProgress, attachHttpFixtureProgress, createHttpCheckpointRelay } from "./http-fixture-progress.mjs";

function control(overrides = {}) {
  const state = { now: 100, lines: [], writes: 0, clocks: 0 };
  const progress = createHttpFixtureProgress({
    clock: () => { state.clocks++; return state.now; },
    write: (line) => { state.writes++; state.lines.push(line); return true; },
    ...overrides,
  });
  return { state, progress, rows: () => state.lines.map((line) => JSON.parse(line)) };
}

function modeledStartup(progress) {
  const child = new EventEmitter();
  child.stderr = new EventEmitter();
  const lines = new EventEmitter();
  progress.observe("launch_requested");
  attachHttpFixtureProgress(child, lines, progress);
  const exited = new Error("original startup exit");
  const timedOut = new Error("original watchdog");
  let rejectReady;
  let cleared = false;
  const ready = new Promise((resolve, reject) => {
    rejectReady = reject;
    const onExit = () => { cleared = true; reject(exited); };
    child.once("exit", onExit);
    lines.on("line", (line) => {
      const prefix = "READY ";
      if (!line.startsWith(prefix)) return;
      cleared = true;
      child.off("exit", onExit);
      const value = JSON.parse(line.slice(prefix.length));
      progress.observe("ready_received");
      resolve(value);
    });
  });
  return { child, lines, ready, exited, timedOut, expire: () => {
    if (cleared) return;
    progress.observe("watchdog_expired");
    // The observer has no watchdog or workload promise of its own.
    rejectReady(timedOut);
  } };
}

test("fixed parent boundaries report Cargo creation and keep compiler and app entry unobserved", () => {
  let now = 100;
  const lines = [];
  const progress = createHttpFixtureProgress({
    clock: () => now,
    write: (line) => { lines.push(line); return true; },
  });
  const events = [
    "launch_requested", "child_spawned", "stdout_seen", "stderr_seen",
    "ready_received", "watchdog_expired", "child_exited",
  ];
  for (const event of events) {
    progress.observe(event, event === "child_exited" ? 0 : null);
    now += 5;
  }
  const rows = lines.map((line) => JSON.parse(line));
  assert.deepEqual(rows.map((row) => row.event), events);
  assert.deepEqual(rows.map((row) => row.sequence), [1, 2, 3, 4, 5, 6, 7]);
  assert.deepEqual(rows.map((row) => row.elapsed_ms), [0, 5, 10, 15, 20, 25, 30]);
  assert.deepEqual(rows.map((row) => row.child_state), [
    "not_started", "spawned", "spawned", "spawned", "spawned", "spawned", "exited",
  ]);
  for (const row of rows) {
    assert.equal(row.schema, "mount-rs.http-fixture-progress.v1");
    assert.equal(row.compilation_observation, "unobserved");
    assert.equal(row.runtime_entry_observation, "unobserved");
  }
  assert.equal(rows[0].stdout_seen, false);
  assert.equal(rows[0].stderr_seen, false);
  assert.equal(rows[3].stdout_seen, true);
  assert.equal(rows[3].stderr_seen, true);
  assert.equal(rows[6].exit_kind, "success");
  assert.equal(rows[6].exit_code, 0);
  assert.equal(progress.receipt().available, true);
});

test("passive listeners observe first stream activity and lifetime exit after readiness", async () => {
  const { progress, rows } = control();
  const model = modeledStartup(progress);
  assert.equal(model.child.listenerCount("error"), 0);
  model.child.emit("spawn");
  model.lines.emit("line", "PRIVATE compiler/path/SQL stdout");
  model.lines.emit("line", "READY {\"url\":\"PRIVATE_URL\"}");
  assert.deepEqual(await model.ready, { url: "PRIVATE_URL" });
  model.child.stderr.emit("data", "PRIVATE stderr/argv/environment");
  model.child.stderr.emit("data", "SECOND_PRIVATE");
  assert.equal(model.child.listenerCount("exit"), 1);
  model.child.emit("exit", null, "PRIVATE_SIGNAL");
  assert.deepEqual(rows().map((row) => row.event), [
    "launch_requested", "child_spawned", "stdout_seen", "ready_received", "stderr_seen", "child_exited",
  ]);
  assert.equal(rows().at(-1).exit_kind, "signal");
  assert.equal(rows().at(-1).exit_code, null);
  assert.equal(rows().at(-1).child_state, "exited");
  assert.equal(JSON.stringify(rows()).includes("PRIVATE"), false);
  assert.equal(model.child.listenerCount("error"), 0);
});

test("exit before readiness preserves the original rejection and does not suppress spawn errors", async () => {
  const { progress, rows } = control();
  const model = modeledStartup(progress);
  const rejected = assert.rejects(model.ready, (error) => error === model.exited);
  model.child.emit("spawn");
  model.child.emit("exit", 7, null);
  await rejected;
  assert.equal(rows().at(-1).exit_kind, "nonzero");
  assert.equal(rows().at(-1).exit_code, 7);
  const original = new Error("PRIVATE unhandled spawn failure");
  assert.throws(() => model.child.emit("error", original), (error) => error === original);
});

test("the modeled existing watchdog retains the original rejection and last observed child state", async () => {
  for (const overrides of [{}, { clock: () => { throw new Error("PRIVATE clock"); } }, { write: () => false }]) {
    const { progress, state, rows } = control(overrides);
    const model = modeledStartup(progress);
    const rejected = assert.rejects(model.ready, (error) => error === model.timedOut);
    model.child.emit("spawn");
    state.now += 120000;
    assert.doesNotThrow(() => model.expire());
    await rejected;
    if (progress.receipt().available) {
      assert.equal(rows().at(-1).event, "watchdog_expired");
      assert.equal(rows().at(-1).child_state, "spawned");
      assert.equal(rows().at(-1).stdout_seen, false);
      assert.equal(rows().at(-1).stderr_seen, false);
      assert.equal(rows().at(-1).elapsed_ms, 120000);
    }
    model.child.emit("exit", null, "PRIVATE_SIGNAL");
    assert.equal(progress.receipt().child_state, "exited");
  }
});

test("parsed readiness cancels the modeled watchdog and malformed JSON never reports readiness", async () => {
  const { progress, rows } = control();
  const readyModel = modeledStartup(progress);
  readyModel.child.emit("spawn");
  readyModel.lines.emit("line", "READY {\"value\":7}");
  assert.deepEqual(await readyModel.ready, { value: 7 });
  readyModel.expire();
  assert.equal(rows().some((row) => row.event === "watchdog_expired"), false);
  readyModel.child.emit("exit", 0, null);
  const malformed = control();
  const malformedModel = modeledStartup(malformed.progress);
  assert.throws(() => malformedModel.lines.emit("line", "READY {PRIVATE malformed"), SyntaxError);
  assert.equal(malformed.rows().some((row) => row.event === "ready_received"), false);
  malformedModel.child.emit("exit", 0, null);
});

test("duplicate activity is bounded and deadline plus later exit remain observable", () => {
  const { progress, state, rows } = control();
  for (let count = 0; count < 1000; count++) {
    for (const event of ["launch_requested", "child_spawned", "stdout_seen", "stderr_seen", "ready_received"]) {
      progress.observe(event);
    }
  }
  state.now += 120000;
  progress.observe("watchdog_expired");
  progress.observe("child_exited", 0);
  progress.observe("watchdog_expired");
  progress.observe("child_exited", 9);
  assert.equal(rows().length, 7);
  assert.ok(rows().length <= 16);
  assert.equal(state.clocks, 7);
  assert.deepEqual(rows().slice(-2).map((row) => row.event), ["watchdog_expired", "child_exited"]);
  assert.equal(rows().at(-1).exit_code, 0);
});

test("maximal valid scalars stay within 512 UTF-8 bytes per line", () => {
  const { progress, state, rows } = control();
  state.now = 0;
  progress.observe("launch_requested");
  state.now = Number.MAX_SAFE_INTEGER;
  progress.observe("child_exited", 0xffff_ffff);
  assert.equal(rows().at(-1).elapsed_ms, Number.MAX_SAFE_INTEGER);
  assert.equal(rows().at(-1).exit_code, 0xffff_ffff);
  for (const line of state.lines) assert.ok(Buffer.byteLength(line, "utf8") <= 512);
});

for (const [value, reason] of [[NaN, "clock_invalid"], [Infinity, "clock_invalid"], [-1, "clock_invalid"], ["PRIVATE_CLOCK", "clock_invalid"]]) {
  test(`invalid diagnostic clock is unavailable (${reason}) without throwing`, () => {
    const { progress, rows } = control({ clock: () => value });
    assert.doesNotThrow(() => { progress.observe("launch_requested"); progress.observe("watchdog_expired"); });
    assert.equal(progress.receipt().available, false);
    assert.equal(progress.receipt().reason, reason);
    assert.equal(rows().filter((row) => row.event === "observer_unavailable").length, 1);
    assert.equal(JSON.stringify(rows()).includes("PRIVATE"), false);
  });
}

test("regressed clock emits one fixed unavailable boundary and no false elapsed time", () => {
  const { progress, state, rows } = control();
  progress.observe("launch_requested");
  state.now--;
  progress.observe("child_spawned");
  progress.observe("watchdog_expired");
  assert.equal(progress.receipt().reason, "clock_regressed");
  assert.deepEqual(rows().map((row) => row.event), ["launch_requested", "observer_unavailable"]);
  assert.equal(rows().at(-1).elapsed_ms, 0);
});

for (const [write, reason] of [[() => { throw new Error("PRIVATE sink"); }, "sink_failed"], [() => false, "sink_backpressure"]]) {
  test(`sink failure preserves original workload rejection (${reason})`, async () => {
    let writes = 0;
    const { progress } = control({ write: (line) => { writes++; return write(line); } });
    const model = modeledStartup(progress);
    const rejected = assert.rejects(model.ready, (error) => error === model.exited);
    assert.doesNotThrow(() => { model.child.emit("spawn"); model.child.emit("exit", 9, null); });
    await rejected;
    assert.equal(progress.receipt().available, false);
    assert.equal(progress.receipt().reason, reason);
    assert.equal(writes, 1);
  });
}

test("clock exceptions and invalid event strings are fixed and redacted", () => {
  for (const options of [{ clock: () => { throw new Error("PRIVATE clock"); } }, {}]) {
    const { progress, rows } = control(options);
    assert.doesNotThrow(() => progress.observe(options.clock ? "launch_requested" : "PRIVATE event/path/SQL"));
    assert.equal(progress.receipt().available, false);
    assert.match(progress.receipt().reason, /^(?:clock_failed|invalid_event)$/u);
    assert.equal(JSON.stringify(rows()).includes("PRIVATE"), false);
  }
});

test("invalid exit payloads never coerce or publish private values", () => {
  const { progress, rows } = control();
  const privateValue = { toString() { throw new Error("PRIVATE coercion"); } };
  progress.observe("launch_requested");
  assert.doesNotThrow(() => progress.observe("child_exited", privateValue, privateValue));
  assert.equal(rows().at(-1).exit_kind, null);
  assert.equal(rows().at(-1).exit_code, null);
  assert.equal(JSON.stringify(rows()).includes("PRIVATE"), false);
});

function childRecord(overrides = {}) {
  return {
    schema: "mount-rs.http-oracle-startup.v1",
    pid: 321,
    scope: "application_async_main_body",
    stage: "main_body_entered",
    sequence: 1,
    elapsed_ms: 0,
    availability: "observed",
    reason: null,
    compiler_observation: "unobserved",
    ...overrides,
  };
}

test("bounded relay emits a complete fixed child checkpoint after fragmented stderr", () => {
  const output = [];
  const relay = createHttpCheckpointRelay({ write: (line) => { output.push(line); return true; } });
  const record = childRecord();
  const bytes = Buffer.from(`${JSON.stringify(record)}\n`);
  for (const part of [bytes.subarray(0, 3), bytes.subarray(3, 40), bytes.subarray(40)]) relay.push(part);
  assert.equal(output.length, 1);
  assert.deepEqual(JSON.parse(output[0]), record);
});

test("relay reassembles the fixed record at every byte boundary", () => {
  const bytes = Buffer.from(`${JSON.stringify(childRecord())}\n`);
  for (let split = 0; split <= bytes.length; split++) {
    const output = [];
    const relay = createHttpCheckpointRelay({ write: (line) => { output.push(line); return true; } });
    relay.push(bytes.subarray(0, split));
    relay.push(bytes.subarray(split));
    assert.equal(output.length, 1, `byte boundary ${split}`);
    assert.deepEqual(JSON.parse(output[0]), childRecord());
  }
});

test("relay refuses unknown, malformed and oversized stderr without forwarding raw details", () => {
  const output = [];
  const relay = createHttpCheckpointRelay({ write: (line) => { output.push(line); return true; } });
  const invalid = [
    "PRIVATE raw argv/env/path/SQL/error",
    "{PRIVATE malformed",
    JSON.stringify(childRecord({ stage: "PRIVATE stage" })),
    JSON.stringify(childRecord({ private_error: "PRIVATE details" })),
    JSON.stringify(childRecord({ compiler_observation: "observed" })),
    JSON.stringify(childRecord({ pid: 0 })),
    JSON.stringify(childRecord({ sequence: 17 })),
    JSON.stringify(childRecord({ elapsed_ms: -1 })),
    JSON.stringify(childRecord({ elapsed_ms: Number.MAX_SAFE_INTEGER + 1 })),
    JSON.stringify(childRecord({ availability: "unavailable" })),
    "PRIVATE".repeat(15000),
  ];
  for (const line of invalid) relay.push(Buffer.from(`${line}\n`));
  assert.deepEqual(output, []);
  relay.push(Buffer.from(`${JSON.stringify(childRecord())}\n`));
  assert.equal(output.length, 1);
  assert.equal(output.join("").includes("PRIVATE"), false);
  assert.equal(relay.receipt().refused_records, invalid.length);
});

test("relay preserves monotonic sequence, elapsed and first self-reported PID without an OS identity claim", () => {
  const output = [];
  const relay = createHttpCheckpointRelay({ write: (line) => { output.push(line); return true; } });
  const push = (row) => relay.push(Buffer.from(`${JSON.stringify(row)}\n`));
  push(childRecord());
  push(childRecord({ stage: "s3_create_started", sequence: 2, pid: 999 }));
  push(childRecord({ sequence: 2 }));
  push(childRecord({ stage: "s3_create_started", sequence: 2, elapsed_ms: 5 }));
  push(childRecord({ stage: "s3_create_completed", sequence: 2, elapsed_ms: 6 }));
  push(childRecord({ stage: "s3_create_completed", sequence: 3, elapsed_ms: 4 }));
  push(childRecord({ stage: "s3_create_completed", sequence: 3, elapsed_ms: 6 }));
  assert.equal(output.length, 3);
  assert.deepEqual(output.map((line) => JSON.parse(line).stage), ["main_body_entered", "s3_create_started", "s3_create_completed"]);
  assert.equal(relay.receipt().self_reported_pid, 321);
  assert.equal(relay.receipt().refused_records, 4);
});

test("relay accepts only fixed unavailable reasons and ends the child stream after one unavailable record", () => {
  const output = [];
  const relay = createHttpCheckpointRelay({ write: (line) => { output.push(line); return true; } });
  const push = (row) => relay.push(Buffer.from(`${JSON.stringify(row)}\n`));
  push(childRecord({ stage: "observer_unavailable", availability: "unavailable", reason: "PRIVATE error" }));
  push(childRecord({ stage: "observer_unavailable", availability: "unavailable", reason: "clock_invalid", elapsed_ms: null }));
  push(childRecord({ stage: "main_body_entered", sequence: 2 }));
  assert.equal(output.length, 1);
  assert.equal(JSON.parse(output[0]).reason, "clock_invalid");
  assert.equal(output.join("").includes("PRIVATE"), false);
});

test("relay bounds all current child stages and largest numeric scalars to 512-byte lines", () => {
  const output = [];
  const relay = createHttpCheckpointRelay({ write: (line) => { output.push(line); return true; } });
  const stages = ["main_body_entered", "s3_create_started", "s3_create_completed", "webdav_create_started", "webdav_create_completed", "webdav_listen_started", "webdav_listen_completed", "ready_published"];
  for (const [index, stage] of stages.entries()) {
    relay.push(Buffer.from(`${JSON.stringify(childRecord({ stage, sequence: index + 1, pid: 0xffff_ffff, elapsed_ms: Number.MAX_SAFE_INTEGER }))}\n`));
  }
  assert.equal(output.length, 8);
  assert.ok(output.length <= 16);
  for (const line of output) {
    assert.ok(Buffer.byteLength(line, "utf8") <= 512);
    assert.equal(JSON.parse(line).elapsed_ms, Number.MAX_SAFE_INTEGER);
  }
});

for (const [sink, reason] of [[() => { throw new Error("PRIVATE sink"); }, "sink_failed"], [() => false, "sink_short_write"]]) {
  test(`relay disables a failed sink without retry or replacing the caller result (${reason})`, () => {
    let writes = 0;
    const relay = createHttpCheckpointRelay({ write: (line) => { writes++; return sink(line); } });
    const original = new Error("original fixture error");
    const result = { error: original };
    assert.doesNotThrow(() => {
      relay.push(Buffer.from(`${JSON.stringify(childRecord())}\n`));
      relay.push(Buffer.from(`${JSON.stringify(childRecord({ stage: "ready_published", sequence: 2 }))}\n`));
    });
    assert.equal(result.error, original);
    assert.equal(writes, 1);
    assert.equal(relay.receipt().available, false);
    assert.equal(relay.receipt().reason, reason);
  });
}

test("relay refuses non-byte input without coercion", () => {
  const relay = createHttpCheckpointRelay({ write: () => true });
  const privateValue = { toString() { throw new Error("PRIVATE coercion"); }, [Symbol.iterator]() { throw new Error("PRIVATE iteration"); } };
  assert.doesNotThrow(() => relay.push(privateValue));
  assert.equal(relay.receipt().refused_records, 1);
});
