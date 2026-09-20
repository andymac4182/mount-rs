import assert from "node:assert/strict";
import net from "node:net";

import {
  awaitHandlerDetaches,
  startPgliteServer,
} from "../../../tests/pglite/server.mjs";

const CONNECTION_TIMEOUT_MS = 2_000;
const PROBE_TABLE = "mount_rs_slot_release_probe";

function withTimeout(operation, label) {
  let timer;
  const timeout = new Promise((_, reject) => {
    timer = setTimeout(() => {
      reject(new Error(`${label} timed out after ${CONNECTION_TIMEOUT_MS}ms`));
    }, CONNECTION_TIMEOUT_MS);
  });
  return Promise.race([operation, timeout]).finally(() => clearTimeout(timer));
}

function connectionAttempt(server) {
  return new Promise((resolve, reject) => {
    const socket = net.connect(server.port, "127.0.0.1");
    let settled = false;
    const finish = (result, error) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      server.removeEventListener("connection", onAccepted);
      socket.removeListener("close", onClosed);
      socket.removeListener("error", onError);
      if (error) {
        socket.destroy();
        reject(error);
      } else {
        // Keep later client-side socket errors from becoming process-fatal
        // while the server-side handler remains the subject of this test.
        socket.on("error", () => {});
        resolve({ accepted: result, socket });
      }
    };
    const onAccepted = () => finish(true);
    const onClosed = () => finish(false);
    const onError = (error) => finish(undefined, error);
    const timer = setTimeout(() => {
      finish(
        undefined,
        new Error(
          `connection attempt timed out after ${CONNECTION_TIMEOUT_MS}ms`,
        ),
      );
    }, CONNECTION_TIMEOUT_MS);
    server.addEventListener("connection", onAccepted);
    socket.once("close", onClosed);
    socket.once("error", onError);
  });
}

async function rejectedConnection(server) {
  const result = await connectionAttempt(server);
  assert.equal(result.accepted, false);
  result.socket.destroy();
}

function frame(type, payload) {
  const body = Buffer.isBuffer(payload) ? payload : Buffer.from(payload);
  const packet = Buffer.alloc(5 + body.length);
  packet[0] = type.charCodeAt(0);
  packet.writeUInt32BE(body.length + 4, 1);
  body.copy(packet, 5);
  return packet;
}

function startupFrame() {
  const parameters = Buffer.from(
    "user\0postgres\0database\0postgres\0\0",
    "utf8",
  );
  const packet = Buffer.alloc(8 + parameters.length);
  packet.writeUInt32BE(packet.length, 0);
  packet.writeUInt32BE(196608, 4);
  parameters.copy(packet, 8);
  return packet;
}

function parseError(payload) {
  const fields = {};
  let offset = 0;
  while (offset < payload.length && payload[offset] !== 0) {
    const code = String.fromCharCode(payload[offset++]);
    const end = payload.indexOf(0, offset);
    if (end < 0) break;
    fields[code] = payload.toString("utf8", offset, end);
    offset = end + 1;
  }
  return fields.M ?? "unknown PostgreSQL wire error";
}

function parseDataRow(payload) {
  const count = payload.readUInt16BE(0);
  let offset = 2;
  const values = [];
  for (let index = 0; index < count; index += 1) {
    const length = payload.readInt32BE(offset);
    offset += 4;
    if (length < 0) {
      values.push(null);
    } else {
      values.push(payload.toString("utf8", offset, offset + length));
      offset += length;
    }
  }
  return values;
}

class WireClient {
  constructor(socket) {
    this.socket = socket;
    this.buffer = Buffer.alloc(0);
    this.waiters = [];
    this.failure = null;
    socket.on("data", (chunk) => {
      this.buffer = Buffer.concat([this.buffer, chunk]);
      this.#drain();
    });
    socket.on("error", (error) => {
      this.failure = error;
      this.#rejectWaiters(error);
    });
    socket.on("close", () => {
      if (!this.failure) {
        this.failure = new Error("PGlite wire client socket closed");
      }
      this.#rejectWaiters(this.failure);
    });
  }

  #rejectWaiters(error) {
    for (const waiter of this.waiters.splice(0)) waiter.reject(error);
  }

  #drain() {
    while (this.waiters.length > 0 && this.buffer.length >= 5) {
      const length = this.buffer.readUInt32BE(1);
      if (length < 4) {
        this.#rejectWaiters(new Error("invalid PostgreSQL wire frame length"));
        return;
      }
      if (this.buffer.length < length + 1) return;
      const packet = this.buffer.subarray(0, length + 1);
      this.buffer = this.buffer.subarray(length + 1);
      const waiter = this.waiters.shift();
      waiter.resolve({
        payload: packet.subarray(5),
        type: String.fromCharCode(packet[0]),
      });
    }
  }

  nextFrame() {
    if (this.failure) return Promise.reject(this.failure);
    return new Promise((resolve, reject) => {
      this.waiters.push({ reject, resolve });
      this.#drain();
    });
  }

  async readUntilReady() {
    const frames = [];
    let wireError;
    while (true) {
      const current = await this.nextFrame();
      frames.push(current);
      if (current.type === "E") wireError = parseError(current.payload);
      if (current.type === "Z") {
        if (wireError) throw new Error(wireError);
        return {
          frames,
          ready: String.fromCharCode(current.payload[0]),
        };
      }
    }
  }

  async startup() {
    this.socket.write(startupFrame());
    return withTimeout(this.readUntilReady(), "wire startup");
  }

  async query(sql) {
    this.socket.write(frame("Q", Buffer.from(`${sql}\0`, "utf8")));
    return withTimeout(this.readUntilReady(), `wire query ${sql}`);
  }

  rows(result) {
    return result.frames
      .filter(({ type }) => type === "D")
      .map(({ payload }) => parseDataRow(payload));
  }

  destroy() {
    this.socket.destroy();
  }
}

async function connectedWireClient(server) {
  const result = await connectionAttempt(server);
  assert.equal(result.accepted, true);
  const client = new WireClient(result.socket);
  const startup = await client.startup();
  assert.equal(startup.ready, "I");
  return client;
}

function destroyServerHandlers(server) {
  const handlers = [...server.handlers];
  const socketRecords = handlers.map((handler) => ({
    closeInfo: closeMarkers.get(handler.socket),
  }));
  const socketClosed = handlers.map(
    (handler) =>
      new Promise((resolve) => {
        handler.socket.once("close", resolve);
        handler.socket.destroy();
      }),
  );
  const handlerClosed = handlers.map(
    (handler) =>
      new Promise((resolve) => {
        handler.addEventListener("close", resolve, { once: true });
      }),
  );
  return { handlerClosed, handlers, socketClosed, socketRecords };
}

function holdUpstreamHandleCloseImmediates() {
  const original = globalThis.setImmediate;
  const held = [];
  globalThis.setImmediate = (callback, ...args) => {
    if (String(callback).includes("handleClose")) {
      held.push({ args, callback });
      return {
        hasRef: () => false,
        ref() {},
        unref() {},
      };
    }
    return original(callback, ...args);
  };
  return {
    held,
    restore() {
      globalThis.setImmediate = original;
    },
  };
}

const { database, server } = await startPgliteServer({
  maxConnections: 2,
  port: 0,
});
let immediateGate;
const rawServer = server.server;
const closeMarkers = new WeakMap();
const unrelatedConnectionListener = (socket) => {
  const marker = { calls: 0 };
  const listener = () => {
    marker.calls += 1;
  };
  closeMarkers.set(socket, { listener, marker });
  socket.on("close", listener);
};
rawServer.on("connection", unrelatedConnectionListener);

try {
  const first = await connectedWireClient(server);
  const second = await connectedWireClient(server);
  assert.deepEqual(server.getStats(), {
    activeConnections: 2,
    maxConnections: 2,
    queuedQueries: 0,
  });

  for (const handler of server.handlers) {
    const closeInfo = closeMarkers.get(handler.socket);
    assert.ok(closeInfo, "unrelated close listener was not installed");
    assert.equal(handler.socket.listeners("close").includes(closeInfo.listener), true);
  }

  // The cap remains real: a third TCP socket is rejected while both handler
  // slots are live.
  await rejectedConnection(server);
  assert.equal(server.getStats().activeConnections, 2);

  // Drive a real PostgreSQL transaction through the socket protocol. The
  // handler owns the transaction in QueryQueueManager, so detach must issue
  // ROLLBACK before the reconnect is accepted.
  assert.equal((await first.query("BEGIN")).ready, "T");
  assert.equal(
    (await first.query(`CREATE TABLE ${PROBE_TABLE}(value integer)`)).ready,
    "T",
  );
  assert.equal(database.isInTransaction(), true);

  immediateGate = holdUpstreamHandleCloseImmediates();
  const { handlerClosed, handlers, socketClosed, socketRecords } =
    destroyServerHandlers(server);
  await withTimeout(Promise.all(socketClosed), "server socket close");

  let transactionStateAtConnection;
  const transactionStateListener = () => {
    transactionStateAtConnection = database.isInTransaction();
  };
  server.addEventListener("connection", transactionStateListener);
  const reopenedFirst = await connectedWireClient(server);
  server.removeEventListener("connection", transactionStateListener);
  immediateGate.restore();

  assert.equal(immediateGate.held.length, 0);
  assert.equal(transactionStateAtConnection, false);
  assert.equal(database.isInTransaction(), false);
  assert.deepEqual(
    reopenedFirst.rows(
      await reopenedFirst.query(
        `SELECT to_regclass('public.${PROBE_TABLE}')`,
      ),
    ),
    [[null]],
  );
  assert.equal(server.getStats().activeConnections, 1);

  await withTimeout(Promise.all(handlerClosed), "handler cleanup");
  for (const { closeInfo } of socketRecords) {
    assert.equal(closeInfo.marker.calls, 1);
  }

  const reopenedSecond = await connectedWireClient(server);
  assert.deepEqual(server.getStats(), {
    activeConnections: 2,
    maxConnections: 2,
    queuedQueries: 0,
  });
  assert.equal(handlers.every((handler) => !handler.isAttached), true);
  reopenedFirst.destroy();
  reopenedSecond.destroy();
} finally {
  immediateGate?.restore();
  rawServer.removeListener("connection", unrelatedConnectionListener);
  await withTimeout(server.stop(), "server stop");
  await withTimeout(awaitHandlerDetaches(), "handler detach shutdown");
  await withTimeout(database.close(), "database shutdown");
}

console.log("pglite server slot release: ok");
