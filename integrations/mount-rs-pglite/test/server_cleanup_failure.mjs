import assert from "node:assert/strict";
import net from "node:net";

import {
  awaitHandlerDetaches,
  startPgliteServer,
} from "../../../tests/pglite/server.mjs";

const TIMEOUT_MS = 2_000;

function withTimeout(operation, label) {
  let timer;
  const timeout = new Promise((_, reject) => {
    timer = setTimeout(() => {
      reject(new Error(`${label} timed out after ${TIMEOUT_MS}ms`));
    }, TIMEOUT_MS);
  });
  return Promise.race([operation, timeout]).finally(() => clearTimeout(timer));
}

function connectionAttempt(server) {
  return new Promise((resolve, reject) => {
    const socket = net.connect(server.port, "127.0.0.1");
    let settled = false;
    let timer;
    const finish = (accepted, error) => {
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
        socket.on("error", () => {});
        resolve({ accepted, socket });
      }
    };
    const onAccepted = () => finish(true);
    const onClosed = () => finish(false);
    const onError = (error) => finish(false, error);
    timer = setTimeout(() => {
      finish(
        false,
        new Error(`connection attempt timed out after ${TIMEOUT_MS}ms`),
      );
    }, TIMEOUT_MS);
    server.addEventListener("connection", onAccepted);
    socket.once("close", onClosed);
    socket.once("error", onError);
  });
}

function isInjectedCleanupFailure(error, injected) {
  return (
    error instanceof AggregateError &&
    error.errors.some((cause) => cause === injected)
  );
}

const { database, server } = await startPgliteServer({
  maxConnections: 1,
  port: 0,
});
let handler;
let originalDetach;
let expectedFailureObserved = false;
let passed = false;
const injected = new Error("injected cleanup failure");

try {
  const first = await connectionAttempt(server);
  assert.equal(first.accepted, true);
  handler = [...server.handlers][0];
  assert.ok(handler);
  originalDetach = handler.detach;
  let closeEvents = 0;
  handler.addEventListener("close", () => {
    closeEvents += 1;
  });
  handler.detach = () => Promise.reject(injected);

  const serverSocketClosed = new Promise((resolve) => {
    handler.socket.once("close", resolve);
    handler.socket.destroy();
  });
  await withTimeout(serverSocketClosed, "injected server socket close");

  await assert.rejects(
    withTimeout(awaitHandlerDetaches(), "injected cleanup observation"),
    (error) => {
      expectedFailureObserved = isInjectedCleanupFailure(error, injected);
      return expectedFailureObserved;
    },
  );
  assert.equal(handler.active, false);
  assert.equal(handler.isAttached, true);
  assert.equal(closeEvents, 0);
  assert.equal(server.handlers.has(handler), true);
  assert.equal(server.getStats().activeConnections, 1);

  const reconnect = await connectionAttempt(server);
  assert.equal(reconnect.accepted, false);
  reconnect.socket.destroy();
  assert.equal(server.handlers.has(handler), true);
  assert.equal(server.getStats().activeConnections, 1);
  passed = true;
} finally {
  if (handler && originalDetach) handler.detach = originalDetach;
  await withTimeout(server.stop(), "failure-test server stop");
  try {
    await withTimeout(awaitHandlerDetaches(), "failure-test cleanup drain");
  } catch (error) {
    if (!isInjectedCleanupFailure(error, injected)) throw error;
  }
  await withTimeout(database.close(), "failure-test database shutdown");
  if (passed && expectedFailureObserved) process.exitCode = 0;
}

console.log("pglite cleanup failure fail-closed: ok");
