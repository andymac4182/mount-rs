// Isolated diagnostic, never used by the production provider. Node >= 20.
// PGLITE_MODULE_ROOT=/path/to/tests/pglite/node_modules node direct-baseline.mjs
// PGLITE_BASELINE_DATA_DIR=/fresh/owned/path enables persistent engine storage.
// PGLITE_BASELINE_ITERATIONS=1000 PGLITE_BASELINE_CLIENTS=10 override the workload.
import { createRequire } from "node:module";
import { pathToFileURL } from "node:url";
import { resolve } from "node:path";
import { Socket } from "node:net";
import { readFileSync } from "node:fs";
import { performance } from "node:perf_hooks";

const moduleRoot = process.env.PGLITE_MODULE_ROOT;
if (!moduleRoot) throw new Error("PGLITE_MODULE_ROOT must name installed test dependencies");
const require = createRequire(resolve(moduleRoot, "../package.json"));
const { PGlite } = await import(pathToFileURL(require.resolve("@electric-sql/pglite")));
const { PGLiteSocketServer } = await import(pathToFileURL(require.resolve("@electric-sql/pglite-socket")));
const iterations = Number(process.env.PGLITE_BASELINE_ITERATIONS ?? 1000);
const clients = Number(process.env.PGLITE_BASELINE_CLIENTS ?? 10);
const fragmentedProbe = process.env.PGLITE_BASELINE_FRAGMENTED_PROBE === "1";
const fragmentBytes = Number(process.env.PGLITE_BASELINE_FRAGMENT_BYTES ?? 128 * 1024);
const parameterCounts = (process.env.PGLITE_BASELINE_PARAMETER_COUNTS ?? "1,2,3,4,5").split(",").map(Number);
const describeStatement = process.env.PGLITE_BASELINE_DESCRIBE_STATEMENT !== "0";
if (!Number.isSafeInteger(iterations) || iterations < 1 || !Number.isSafeInteger(clients) || clients < 1)
  throw new Error("iterations and clients must be positive integers");
if (!Number.isSafeInteger(fragmentBytes) || fragmentBytes < 1 || parameterCounts.length === 0
  || parameterCounts.some((n) => !Number.isSafeInteger(n) || n < 1 || n > 100))
  throw new Error("invalid fragment byte count or parameter count pattern");
const dataDir = process.env.PGLITE_BASELINE_DATA_DIR;
const database = await PGlite.create(dataDir);
const report = { schema: "mount-rs-pglite-direct-v1", iterations, clients,
  storage: dataDir ? "persistent-directory" : "in-memory", data_dir: dataDir ?? null,
  process: { node: process.version },
  dependencies: Object.fromEntries(["pglite", "pglite-socket"].map((name) => [name,
    JSON.parse(readFileSync(resolve(moduleRoot, "@electric-sql", name, "package.json"), "utf8")).version])),
  describe_target: describeStatement ? "statement (tokio-postgres query_typed)" : "portal",
  stages: [], counters: {}, wire: [] };
const i16 = (n) => { const b = Buffer.alloc(2); b.writeInt16BE(n); return b; };
const i32 = (n) => { const b = Buffer.alloc(4); b.writeInt32BE(n); return b; };
const cstr = (s) => Buffer.from(`${s}\0`);
const frame = (tag, ...parts) => { const body = Buffer.concat(parts); return Buffer.concat([Buffer.from(tag), i32(body.length + 4), body]); };

// Counts every frontend/backend frame and byte, including responses and errors.
// A client has one outstanding statement; no frontend pipelining is involved.
class WireClient {
  constructor(port) {
    this.socket = new Socket(); this.buffer = Buffer.alloc(0); this.rows = [];
    this.stats = { frontend_messages: {}, backend_messages: {}, frontend_bytes: 0, backend_bytes: 0, requests: 0, errors: 0 };
    this.pending = null;
    this.socket.on("data", (data) => {
      this.stats.backend_bytes += data.length;
      this.buffer = Buffer.concat([this.buffer, data]);
      while (this.buffer.length >= 5) {
        const n = this.buffer.readInt32BE(1) + 1;
        if (n < 5) return this.fail(new Error("invalid backend frame length"));
        if (this.buffer.length < n) break;
        const tag = String.fromCharCode(this.buffer[0]);
        const body = this.buffer.subarray(5, n); this.buffer = this.buffer.subarray(n);
        this.stats.backend_messages[tag] = (this.stats.backend_messages[tag] ?? 0) + 1;
        if (tag === "E") {
          const fields = {}; let offset = 0;
          while (body[offset]) { const key = String.fromCharCode(body[offset++]); const end = body.indexOf(0, offset); fields[key] = body.toString("utf8", offset, end); offset = end + 1; }
          this.error = new Error(`${fields.C ?? "unknown"}: ${fields.M ?? "backend error"}`);
          this.stats.errors++;
        }
        if (tag === "R" && body.readInt32BE(0) !== 0) this.fail(new Error("diagnostic only supports trusted PGlite startup"));
        if (tag === "D") {
          const values = []; let offset = 2;
          for (let j = 0; j < body.readInt16BE(0); j++) { const size = body.readInt32BE(offset); offset += 4; values.push(size === -1 ? null : body.toString("utf8", offset, offset + size)); if (size !== -1) offset += size; }
          this.rows.push(values);
        }
        if (tag === "Z" && this.pending) {
          const pending = this.pending; this.pending = null;
          if (this.error) { const error = this.error; this.error = null; pending.reject(error); }
          else pending.resolve(this.rows);
        }
      }
    });
    this.socket.on("error", (error) => this.fail(error));
    this.socket.on("close", () => this.fail(new Error("wire connection closed")));
    this.socket.setTimeout(10000);
    this.socket.on("timeout", () => { this.fail(new Error("wire connection idle for 10 seconds")); this.socket.destroy(); });
    this.ready = new Promise((resolveReady, reject) => {
      this.pending = { resolve: resolveReady, reject };
      this.socket.connect(port, "127.0.0.1", () => {
        const body = Buffer.concat([i32(196608), cstr("user"), cstr("postgres"), cstr("database"), cstr("postgres"), Buffer.from([0])]);
        const startup = Buffer.concat([i32(body.length + 4), body]);
        this.stats.frontend_messages.startup = 1;
        this.stats.frontend_bytes += startup.length; this.socket.write(startup);
      });
    });
  }
  fail(error) { if (this.pending) { const pending = this.pending; this.pending = null; pending.reject(error); } }
  async query(sql, values = [], name = "") {
    if (this.socket.destroyed) throw new Error("wire connection already destroyed");
    if (this.pending) throw new Error("one outstanding statement per diagnostic client");
    this.rows = []; this.stats.requests++;
    const frames = [
      frame("P", cstr(name), cstr(sql), i16(values.length), ...values.map((v) => i32(Buffer.isBuffer(v) ? 17 : 23))),
      frame("B", cstr(""), cstr(name), i16(values.length), ...values.map((v) => i16(Buffer.isBuffer(v) ? 1 : 0)), i16(values.length), ...values.flatMap((v) => { const b = Buffer.isBuffer(v) ? v : Buffer.from(String(v)); return [i32(b.length), b]; }), i16(0)),
      frame("D", Buffer.from(describeStatement ? "S" : "P"), cstr(describeStatement ? name : "")), frame("E", cstr(""), i32(0)),
      ...(name ? [frame("C", Buffer.from("S"), cstr(name))] : []), frame("S"),
    ];
    for (const f of frames) { const tag = String.fromCharCode(f[0]); this.stats.frontend_messages[tag] = (this.stats.frontend_messages[tag] ?? 0) + 1; this.stats.frontend_bytes += f.length; }
    return new Promise((resolveQuery, reject) => {
      const timeout = setTimeout(() => { this.fail(new Error("wire query exceeded 10 seconds")); this.socket.destroy(); }, 10000);
      this.pending = { resolve: (rows) => { clearTimeout(timeout); resolveQuery(rows); }, reject: (error) => { clearTimeout(timeout); reject(error); } };
      this.socket.write(Buffer.concat(frames));
    });
  }
  async close() {
    if (this.socket.destroyed) return;
    const terminate = frame("X"); this.stats.frontend_bytes += terminate.length;
    this.stats.frontend_messages.X = (this.stats.frontend_messages.X ?? 0) + 1;
    this.socket.end(terminate); await new Promise((resolveClose) => this.socket.once("close", resolveClose));
  }
}

async function counters() {
  const output = { sampling: { sql_statements: 0, force_next_flush: false, clear_snapshot: false } };
  // Awaiting this independent autocommit statement reaches its transaction end
  // before sampling, forcing pending backend counters past the normal 1s limit.
  for (const [name, sql] of [["force_next_flush", "SELECT pg_stat_force_next_flush()"],
    ["clear_snapshot", "SELECT pg_stat_clear_snapshot()"]]) {
    output.sampling.sql_statements++;
    try { await database.query(sql); output.sampling[name] = true; }
    catch (error) { output.sampling[`${name}_error`] = String(error.message); }
  }
  // All views are read in one statement. Table/TOAST/index page statistics
  // exclude catalog pages touched by these sampling queries themselves.
  output.sampling.sql_statements++;
  try {
    const data = (await database.query(`SELECT json_build_object(
      'settings', (SELECT json_agg(s) FROM (SELECT name,setting FROM pg_settings WHERE name IN ('track_counts','track_io_timing','shared_buffers') ORDER BY name) s),
      'database', (SELECT json_agg(d) FROM (SELECT datname,xact_commit,xact_rollback,blks_read,blks_hit,tup_returned,tup_fetched,tup_inserted,tup_updated,tup_deleted,blk_read_time,blk_write_time FROM pg_stat_database WHERE datname=current_database()) d),
      'io', (SELECT json_agg(i) FROM pg_stat_io i),
      'wal', (SELECT json_agg(w) FROM pg_stat_wal w),
      'table_pages', (SELECT json_agg(t) FROM pg_statio_user_tables t WHERE relname='direct_baseline_blocks'),
      'table_rows', (SELECT json_agg(t) FROM pg_stat_user_tables t WHERE relname='direct_baseline_blocks')
    ) AS counters`)).rows[0].counters;
    for (const [name, rows] of Object.entries(data)) output[name] = { available: true, rows: rows ?? [] };
  } catch (error) { output.combined_sample = { available: false, error: String(error.message) }; }
  return output;
}
function numericDelta(after, before) {
  const delta = {};
  for (const [key, value] of Object.entries(after ?? {})) {
    const old = before?.[key];
    if ((typeof value === "number" || (typeof value === "string" && /^\d+$/.test(value)))
      && (typeof old === "number" || (typeof old === "string" && /^\d+$/.test(old)))
      && Number.isSafeInteger(Number(value)) && Number.isSafeInteger(Number(old)))
      delta[key] = Number(value) - Number(old);
  }
  return delta;
}
function counterDelta(after, before, completed) {
  const output = { includes_observer_work: true,
    observer_sql_statements: before.sampling.sql_statements,
    database: numericDelta(after.database?.rows[0], before.database?.rows[0]),
    wal: numericDelta(after.wal?.rows[0], before.wal?.rows[0]),
    table_pages: numericDelta(after.table_pages?.rows[0], before.table_pages?.rows[0]),
    table_rows: numericDelta(after.table_rows?.rows[0], before.table_rows?.rows[0]),
    io: [] };
  if (before.sampling.force_next_flush && after.sampling.force_next_flush
    && before.sampling.clear_snapshot && after.sampling.clear_snapshot
    && typeof output.database.xact_commit === "number") {
    // Between snapshots: the preceding clear/sample transactions and current
    // flush transaction. Sampling has fixed shape and runs at quiescent bounds.
    output.commits_excluding_observer = output.database.xact_commit - before.sampling.sql_statements;
    output.commit_reconciliation = { expected_workload_commits: completed,
      matches: output.commits_excluding_observer === completed };
  }
  for (const row of after.io?.rows ?? []) {
    const old = before.io?.rows.find((r) => r.backend_type === row.backend_type && r.object === row.object && r.context === row.context);
    if (old) output.io.push({ backend_type: row.backend_type, object: row.object, context: row.context,
      delta: numericDelta(row, old) });
  }
  if (completed > 0) output.table_pages_per_completed_operation = Object.fromEntries(
    Object.entries(output.table_pages).filter(([key]) => key.endsWith("_read") || key.endsWith("_hit"))
      .map(([key, value]) => [key, value / completed]));
  return output;
}
async function stage(name, operation, sqlPerOperation) {
  const before = await counters();
  const latencies = []; let errors = 0; const samples = []; const start = performance.now();
  for (let j = 0; j < iterations; j++) { const t = performance.now(); try { await operation(j); } catch (error) { errors++; if (samples.length < 8) samples.push(String(error.message)); } latencies.push((performance.now() - t) * 1000); }
  const elapsed = performance.now() - start; latencies.sort((a, b) => a - b);
  const after = await counters();
  report.stages.push({ name, attempts: iterations, completed: iterations - errors, errors, error_samples: samples,
    elapsed_seconds: elapsed / 1000, operations_per_second: (iterations - errors) * 1000 / elapsed,
    sql_statements_attempted: iterations * sqlPerOperation,
    backend_counters_before: before, backend_counters_after: after,
    backend_counters_delta: counterDelta(after, before, iterations - errors),
    latency_us: { p50: latencies[Math.floor(latencies.length * .5)], p95: latencies[Math.min(latencies.length - 1, Math.floor(latencies.length * .95))] } });
}
let server;
try {
  report.version = (await database.query("SELECT version() AS version")).rows[0];
  await database.exec("CREATE TABLE IF NOT EXISTS direct_baseline_blocks (id INTEGER PRIMARY KEY, bytes BYTEA NOT NULL)");
  const bytes = new Uint8Array(4096).fill(42);
  await database.query("INSERT INTO direct_baseline_blocks VALUES (0,$1) ON CONFLICT(id) DO UPDATE SET bytes=excluded.bytes", [bytes]);
  report.counters.before = await counters();
  const emptyBefore = await counters(), emptyAfter = await counters();
  report.counters.empty_control = counterDelta(emptyAfter, emptyBefore, 0);
  await stage("engine_select_one", () => database.query("SELECT 1 AS value"), 1);
  await stage("engine_4k_upsert", (j) => database.query("INSERT INTO direct_baseline_blocks VALUES ($1,$2) ON CONFLICT(id) DO UPDATE SET bytes=excluded.bytes", [j, bytes]), 1);
  await stage("engine_4k_read", async (j) => {
    const result = await database.query("SELECT bytes FROM direct_baseline_blocks WHERE id=$1", [j]);
    if (result.rows[0]?.bytes?.length !== 4096) throw new Error("engine read payload length mismatch");
  }, 1);
  // Every operation below explicitly commits one transaction with two SQL statements.
  await stage("engine_4k_transaction", (j) => database.transaction(async (tx) => {
    await tx.query("UPDATE direct_baseline_blocks SET bytes=$2 WHERE id=$1", [j, bytes]);
    await tx.query("SELECT bytes FROM direct_baseline_blocks WHERE id=$1", [j]);
  }), 4);
  report.counters.after_engine = await counters();
  server = new PGLiteSocketServer({ db: database, host: "127.0.0.1", port: 0, maxConnections: clients + 2 });
  report.engine_protocol = { calls: 0, request_bytes: 0, response_bytes: 0, frontend_messages: {} };
  const execProtocol = database.execProtocolRawStream.bind(database);
  database.execProtocolRawStream = async (message, options) => {
    report.engine_protocol.calls++; report.engine_protocol.request_bytes += message.length;
    const tag = message[0] === 0 ? "startup" : String.fromCharCode(message[0]);
    report.engine_protocol.frontend_messages[tag] = (report.engine_protocol.frontend_messages[tag] ?? 0) + 1;
    return execProtocol(message, { ...options, onRawData: (data) => {
      report.engine_protocol.response_bytes += data.length;
      options?.onRawData?.(data);
    } });
  };
  await server.start();
  const single = new WireClient(server.port); await single.ready;
  await stage("wire_single_select_one", () => single.query("SELECT 1 AS value"), 1);
  await stage("wire_single_4k_upsert", (j) => single.query("INSERT INTO direct_baseline_blocks VALUES ($1,$2) ON CONFLICT(id) DO UPDATE SET bytes=excluded.bytes", [j, Buffer.from(bytes)]), 1);
  await stage("wire_single_4k_read", async (j) => {
    const rows = await single.query("SELECT bytes FROM direct_baseline_blocks WHERE id=$1", [j]);
    if (rows[0]?.[0]?.length !== 8194) throw new Error("wire text bytea read payload length mismatch");
  }, 1);
  report.wire.push({ name: "single_client", counters: single.stats }); await single.close();
  // Query shapes vary to expose shared unnamed prepared statement contamination.
  // Unique statement names are a diagnostic counterexample, not an adapter fix:
  // the unnamed portal and transaction state are still shared by the backend.
  for (const named of [false, true]) {
    const connections = Array.from({ length: clients }, () => new WireClient(server.port));
    await Promise.all(connections.map((client) => client.ready));
    let errors = 0, mismatches = 0; const samples = []; const start = performance.now();
    await Promise.all(connections.map(async (client, index) => {
      const n = parameterCounts[index % parameterCounts.length], values = Array.from({ length: n }, (_, i) => i + 1);
      const sql = `SELECT ${values.map((_, i) => `$${i + 1}::int`).join("+")} AS value${fragmentedProbe ? `,octet_length($${n + 1}::bytea) AS payload_bytes` : ""}`;
      if (fragmentedProbe) values.push(Buffer.alloc(fragmentBytes, 42));
      const expected = String(n * (n + 1) / 2);
      for (let j = 0; j < Math.min(iterations, 100); j++) {
        try { const rows = await client.query(sql, values, named ? `diagnostic_${index}_${j}` : ""); if (rows[0]?.[0] !== expected || (fragmentedProbe && rows[0]?.[1] !== String(fragmentBytes))) { mismatches++; if (samples.length < 8) samples.push(`client=${index} expected=${expected} observed=${JSON.stringify(rows)}`); } }
        catch (error) { errors++; if (samples.length < 8) samples.push(String(error.message)); }
      }
    }));
    report.wire.push({ name: named ? "multi_client_unique_statement_names" : "multi_client_unnamed_statements", bind_payload_bytes: fragmentedProbe ? fragmentBytes : 0, parameter_counts: parameterCounts,
      clients, attempts: clients * Math.min(iterations, 100), errors, mismatches, elapsed_seconds: (performance.now() - start) / 1000,
      error_samples: samples, counters: connections.map((client) => client.stats) });
    await Promise.all(connections.map((client) => client.close()));
  }
  report.counters.after_wire = await counters();
} catch (error) { report.fatal_error = String(error.stack ?? error); process.exitCode = 1; }
finally {
  try { if (server) await server.stop(); } catch (error) { report.server_cleanup_error = String(error.message); }
  try { await database.close(); } catch (error) { report.engine_cleanup_error = String(error.message); }
  console.log(JSON.stringify(report, null, 2));
}
