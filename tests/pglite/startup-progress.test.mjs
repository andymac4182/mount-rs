import assert from "node:assert/strict";
import { test } from "node:test";
import { createPgliteStartupProgress, loadPgliteStartupImports } from "./startup-progress.mjs";

const STAGES = [
  "module_body_entered", "pglite_import_started", "pglite_import_completed",
  "socket_import_started", "socket_import_completed", "configuration_ready",
  "cleanup_install_started", "cleanup_install_completed", "engine_create_started",
  "engine_create_completed", "socket_construct_started", "socket_construct_completed",
  "socket_start_started", "socket_start_completed", "ready_published",
];

test("fifteen fixed stages precede the reserved unavailable boundary", () => {
  let now = 100;
  const lines = [];
  const progress = createPgliteStartupProgress({
    enabled: true, clock: () => now,
    write: (line) => { lines.push(line); return true; },
  });
  for (const stage of STAGES) { progress.observe(stage); now += 5; }
  const rows = lines.map((line) => JSON.parse(line));
  assert.deepEqual(rows.map((row) => row.stage), STAGES);
  assert.deepEqual(rows.map((row) => row.sequence), STAGES.map((_, index) => index + 1));
  assert.deepEqual(rows.map((row) => row.elapsed_ms), STAGES.map((_, index) => index * 5));
  for (const row of rows) {
    assert.deepEqual(Object.keys(row).sort(), ["availability", "elapsed_ms", "pid", "reason", "schema", "scope", "sequence", "stage"]);
    assert.equal(row.schema, "mount-rs.pglite-startup.v1");
    assert.equal(row.pid, process.pid);
    assert.equal(row.scope, "node_module_body");
    assert.equal(row.availability, "observed");
    assert.equal(row.reason, null);
  }
});

function control(overrides = {}) {
  const state = { now: 100, lines: [], clocks: 0, writes: 0 };
  const progress = createPgliteStartupProgress({
    enabled: true,
    clock: () => { state.clocks++; return state.now; },
    write: (line) => { state.writes++; state.lines.push(line); return true; },
    ...overrides,
  });
  return { progress, state, rows: () => state.lines.map((line) => JSON.parse(line)) };
}

function dependencies() {
  class PGlite {}
  class PGLiteSocketHandler {}
  class PGLiteSocketServer {}
  return { PGlite, PGLiteSocketHandler, PGLiteSocketServer };
}

// Model the existing calls without loading PGlite, starting a server or owning
// a child. Failure identity and completed-stage absence are the assertions.
async function modeledOperations(progress, operations) {
  progress.observe("configuration_ready");
  progress.observe("cleanup_install_started");
  operations.installCleanup();
  progress.observe("cleanup_install_completed");
  progress.observe("engine_create_started");
  const database = await operations.createDatabase();
  progress.observe("engine_create_completed");
  progress.observe("socket_construct_started");
  const server = operations.constructServer(database);
  progress.observe("socket_construct_completed");
  progress.observe("socket_start_started");
  await server.start();
  progress.observe("socket_start_completed");
  operations.publishReady();
  progress.observe("ready_published");
  return { database, server };
}

function operations() {
  const database = Object.freeze({ private: "PRIVATE_DATABASE" });
  const server = { start: async () => undefined };
  return {
    database, server,
    installCleanup() {},
    createDatabase: async () => database,
    constructServer(value) { assert.equal(value, database); return server; },
    publishReady() {},
  };
}

test("duplicates add no clocks or writes and preserve one terminal unavailable slot", () => {
  const { progress, state, rows } = control();
  for (let count = 0; count < 1000; count++) {
    for (const stage of STAGES) progress.observe(stage);
  }
  progress.observe("PRIVATE_STAGE path/env/error");
  progress.observe("SECOND_PRIVATE_STAGE");
  assert.equal(state.clocks, 15);
  assert.equal(state.writes, 16);
  assert.equal(rows().at(-1).stage, "observer_unavailable");
  assert.equal(rows().at(-1).sequence, 16);
  assert.equal(rows().at(-1).reason, "invalid_stage");
  assert.equal(rows().at(-1).availability, "unavailable");
  assert.equal(JSON.stringify(rows()).includes("PRIVATE"), false);
});

test("largest valid elapsed scalar fits the 512 byte cap including newline", () => {
  const { progress, state, rows } = control();
  state.now = 0;
  progress.observe("module_body_entered");
  state.now = Number.MAX_SAFE_INTEGER;
  progress.observe("ready_published");
  progress.observe("PRIVATE_UNKNOWN");
  assert.equal(rows()[1].elapsed_ms, Number.MAX_SAFE_INTEGER);
  for (const line of state.lines) assert.ok(Buffer.byteLength(line, "utf8") <= 512);
  assert.equal(rows().at(-1).availability, "unavailable");
});

test("ordered fake imports preserve exports and wait for the first import", async () => {
  const { progress, rows } = control();
  const expected = dependencies();
  const calls = [];
  let release;
  progress.observe("module_body_entered");
  const loaded = loadPgliteStartupImports(progress, (specifier) => {
    calls.push(specifier);
    if (specifier === "@electric-sql/pglite") return new Promise((resolve) => { release = resolve; });
    return { PGLiteSocketHandler: expected.PGLiteSocketHandler, PGLiteSocketServer: expected.PGLiteSocketServer };
  });
  assert.deepEqual(calls, ["@electric-sql/pglite"]);
  assert.deepEqual(rows().map((row) => row.stage), ["module_body_entered", "pglite_import_started"]);
  release({ PGlite: expected.PGlite });
  assert.deepEqual(await loaded, expected);
  assert.deepEqual(calls, ["@electric-sql/pglite", "@electric-sql/pglite-socket"]);
  assert.deepEqual(rows().map((row) => row.stage), STAGES.slice(0, 5));
});

for (const failedSpecifier of ["@electric-sql/pglite", "@electric-sql/pglite-socket"]) {
  test(`original fake import rejection is retained (${failedSpecifier})`, async () => {
    const { progress, rows } = control();
    const original = new Error("PRIVATE_IMPORT_ERROR");
    const calls = [];
    const expected = dependencies();
    const loaded = loadPgliteStartupImports(progress, async (specifier) => {
      calls.push(specifier);
      if (specifier === failedSpecifier) throw original;
      return expected;
    });
    await assert.rejects(loaded, (error) => error === original);
    assert.equal(calls.filter((name) => name === failedSpecifier).length, 1);
    const failedStage = failedSpecifier.endsWith("-socket") ? "socket_import_started" : "pglite_import_started";
    assert.equal(rows().at(-1).stage, failedStage);
    assert.equal(rows().some((row) => row.stage === failedStage.replace("started", "completed")), false);
    assert.equal(JSON.stringify(rows()).includes("PRIVATE"), false);
  });
}

test("fake operations retain result identity and readiness is published last", async () => {
  const { progress, rows } = control();
  const fake = operations();
  const result = await modeledOperations(progress, fake);
  assert.equal(result.database, fake.database);
  assert.equal(result.server, fake.server);
  assert.deepEqual(rows().map((row) => row.stage), STAGES.slice(5));
  assert.equal(JSON.stringify(rows()).includes("PRIVATE"), false);
});

for (const [operation, lastStage] of [
  ["installCleanup", "cleanup_install_started"],
  ["createDatabase", "engine_create_started"],
  ["constructServer", "socket_construct_started"],
  ["start", "socket_start_started"],
  ["publishReady", "socket_start_completed"],
]) {
  test(`fake operation error preserves original identity (${operation})`, async () => {
    const { progress, rows } = control();
    const fake = operations();
    const original = new Error("PRIVATE_OPERATION_ERROR");
    if (operation === "start") fake.server.start = () => Promise.reject(original);
    else fake[operation] = () => { throw original; };
    await assert.rejects(modeledOperations(progress, fake), (error) => error === original);
    assert.equal(rows().at(-1).stage, lastStage);
    assert.equal(rows().some((row) => row.stage === "ready_published"), false);
    assert.equal(JSON.stringify(rows()).includes("PRIVATE"), false);
  });
}

test("disabled library reporting reads no clock or sink during fake imports and operations", async () => {
  let clocks = 0;
  let writes = 0;
  const progress = createPgliteStartupProgress({
    enabled: false,
    clock: () => { clocks++; throw new Error("PRIVATE clock"); },
    write: () => { writes++; throw new Error("PRIVATE sink"); },
  });
  progress.observe("module_body_entered");
  const expected = dependencies();
  assert.deepEqual(await loadPgliteStartupImports(progress, async () => expected), expected);
  await modeledOperations(progress, operations());
  progress.observe("PRIVATE_STAGE");
  assert.equal(clocks, 0);
  assert.equal(writes, 0);
  assert.equal(progress.receipt().enabled, false);
  assert.equal(progress.receipt().attempted_records, 0);
});

for (const [value, reason] of [[NaN, "clock_invalid"], [Infinity, "clock_invalid"], [-1, "clock_invalid"], ["PRIVATE_CLOCK", "clock_invalid"]]) {
  test(`invalid clock disables without private data (${String(value)})`, () => {
    const { progress, rows } = control({ clock: () => value });
    assert.doesNotThrow(() => progress.observe("module_body_entered"));
    progress.observe("ready_published");
    assert.equal(progress.receipt().availability, "unavailable");
    assert.equal(progress.receipt().reason, reason);
    assert.equal(rows().length, 1);
    assert.equal(rows()[0].stage, "observer_unavailable");
    assert.equal(rows()[0].elapsed_ms, null);
    assert.equal(JSON.stringify(rows()).includes("PRIVATE"), false);
  });
}

test("clock exception and regression emit one fixed unavailable boundary", () => {
  for (const failure of ["exception", "regression"]) {
    const { progress, state, rows } = control();
    progress.observe("module_body_entered");
    if (failure === "regression") state.now--;
    else state.now = Object.freeze({ private: "PRIVATE_CLOCK" });
    progress.observe("pglite_import_started");
    progress.observe("ready_published");
    assert.equal(rows().at(-1).stage, "observer_unavailable");
    assert.equal(rows().at(-1).elapsed_ms, 0);
    assert.equal(rows().length, 2);
    assert.equal(JSON.stringify(rows()).includes("PRIVATE"), false);
  }
  const thrown = control({ clock: () => { throw new Error("PRIVATE_CLOCK_ERROR"); } });
  assert.doesNotThrow(() => thrown.progress.observe("module_body_entered"));
  assert.equal(thrown.progress.receipt().reason, "clock_failed");
});

for (const [write, reason] of [
  [() => { throw new Error("PRIVATE_SINK_ERROR"); }, "sink_failed"],
  [() => false, "sink_short_write"],
  [() => undefined, "sink_short_write"],
]) {
  test(`sink failure has no retry and preserves fake operation failure (${reason})`, async () => {
    let writes = 0;
    const { progress } = control({ write: (line) => { writes++; return write(line); } });
    const fake = operations();
    const original = new Error("PRIVATE_WORKLOAD_ERROR");
    fake.createDatabase = () => { throw original; };
    await assert.rejects(modeledOperations(progress, fake), (error) => error === original);
    assert.equal(writes, 1);
    assert.equal(progress.receipt().availability, "unavailable");
    assert.equal(progress.receipt().reason, reason);
    assert.equal(progress.receipt().successful_writes, 0);
  });
}

test("diagnostic clock failure leaves original fake import failure intact", async () => {
  const { progress, rows } = control({ clock: () => { throw new Error("PRIVATE_CLOCK"); } });
  const original = new Error("PRIVATE_IMPORT");
  await assert.rejects(loadPgliteStartupImports(progress, () => { throw original; }), (error) => error === original);
  assert.equal(rows().length, 1);
  assert.equal(rows()[0].reason, "clock_failed");
  assert.equal(JSON.stringify(rows()).includes("PRIVATE"), false);
});

test("captured hooks and frozen scalar receipts cannot be changed by mutable options", () => {
  const lines = [];
  const options = { enabled: true, clock: () => 1, write: (line) => { lines.push(line); return true; } };
  const progress = createPgliteStartupProgress(options);
  options.enabled = false;
  options.clock = () => { throw new Error("PRIVATE_REPLACEMENT"); };
  options.write = () => { throw new Error("PRIVATE_REPLACEMENT"); };
  progress.observe("module_body_entered");
  assert.equal(lines.length, 1);
  const receipt = progress.receipt();
  assert.equal(Object.isFrozen(receipt), true);
  assert.throws(() => { receipt.reason = "PRIVATE_REASON"; }, TypeError);
  assert.equal(progress.receipt().reason, null);
});

test("invalid private stage values are not coerced or emitted", () => {
  const privateValue = { toString() { throw new Error("PRIVATE_COERCION"); } };
  const { progress, rows } = control();
  assert.doesNotThrow(() => progress.observe(privateValue));
  assert.equal(rows()[0].stage, "observer_unavailable");
  assert.equal(rows()[0].reason, "invalid_stage");
  assert.equal(JSON.stringify(rows()).includes("PRIVATE"), false);
});
