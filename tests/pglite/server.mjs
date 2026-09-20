import { PGlite } from "@electric-sql/pglite";
import {
  PGLiteSocketHandler,
  PGLiteSocketServer,
} from "@electric-sql/pglite-socket";
import { createRequire } from "node:module";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

const require = createRequire(import.meta.url);
const EXPECTED_PGLITE_SOCKET_VERSION = "0.2.11";
const EXPECTED_UPSTREAM_CLOSE_LISTENERS = 1;

let fatalReported = false;
let fatalError;
let notifyFatal;
const fatalSignal = new Promise((resolve) => {
  notifyFatal = resolve;
});

function reportFatal(kind, reason) {
  const error = reason instanceof Error ? reason : new Error(String(reason));
  if (fatalReported) return fatalError;
  fatalReported = true;
  fatalError = error;
  const details = String(error.stack ?? error)
    .split("\n")
    .slice(0, 6)
    .map((line) => (line.length > 500 ? `${line.slice(0, 500)}…` : line))
    .join("\n");
  process.stderr.write(`[pglite-server] ${kind}\n${details}\n`);
  process.exitCode = 1;
  notifyFatal?.(error);
  return error;
}

process.once("uncaughtException", (error) => {
  reportFatal("uncaughtException", error);
});
process.once("unhandledRejection", (reason) => {
  reportFatal("unhandledRejection", reason);
});

const pendingDetaches = new Set();
const pendingHandlerCloses = new Set();
const handlerCloseTasks = new WeakMap();
const cleanupFailures = new Set();
let cleanupInstalled = false;

function observeFailure(promise, kind) {
  promise
    .catch((error) => {
      cleanupFailures.add(error);
      reportFatal(kind, error);
    })
    .catch(() => {
      // Keep the observer itself handled if reportFatal is replaced by a
      // caller that throws. The original promise remains rejected.
    });
  return promise;
}

function assertPgliteSocketSourceVersion() {
  const entry = require.resolve("@electric-sql/pglite-socket");
  const packageRoot = resolve(entry, "..", "..");
  const version = JSON.parse(
    readFileSync(resolve(packageRoot, "package.json"), "utf8"),
  ).version;
  if (version !== EXPECTED_PGLITE_SOCKET_VERSION) {
    throw new Error(
      `PGlite socket cleanup shim expects @electric-sql/pglite-socket@${EXPECTED_PGLITE_SOCKET_VERSION}, found ${version}`,
    );
  }
}

function addedListeners(before, after) {
  const remaining = [...after];
  for (const listener of before) {
    const index = remaining.indexOf(listener);
    if (index < 0) {
      throw new Error("PGlite socket attach removed an existing close listener");
    }
    remaining.splice(index, 1);
  }
  return remaining;
}

/**
 * Adapt pglite-socket 0.2.11's client-close ordering for bounded servers.
 *
 * The upstream handler schedules handleClose with setImmediate. That leaves
 * the handler in PGLiteSocketServer.handlers after the peer socket is closed,
 * so a reconnect can be rejected by maxConnections before the old handler is
 * removed. Mark the handler inactive, detach it, and dispatch close only after
 * queue/transaction cleanup completes. The close event is the server-side
 * observation that the client socket is gone; no retry or elapsed-time wait is
 * needed.
 */
export function installPgliteSocketCleanup() {
  if (cleanupInstalled) return;
  assertPgliteSocketSourceVersion();
  cleanupInstalled = true;

  // pglite-socket starts detach() without awaiting it from both stop() and
  // the socket close path. Track the public method so database shutdown never
  // races an in-flight queue/transaction cleanup.
  const detach = PGLiteSocketHandler.prototype.detach;
  PGLiteSocketHandler.prototype.detach = function (...args) {
    let operation;
    try {
      operation = Promise.resolve(detach.apply(this, args));
    } catch (error) {
      operation = Promise.reject(error);
    }
    let tracked;
    tracked = operation.then(
      (value) => {
        pendingDetaches.delete(tracked);
        return value;
      },
      (error) => {
        pendingDetaches.delete(tracked);
        throw error;
      },
    );
    pendingDetaches.add(tracked);
    observeFailure(tracked, "socket handler detach failed");
    return tracked;
  };

  const closeHandler = (handler) => {
    const existing = handlerCloseTasks.get(handler);
    if (existing) return existing;

    const operation = (async () => {
      handler.active = false;
      // Keep the server slot occupied until detach has cleared this handler's
      // queued work and rolled back its transaction, then dispatch close so
      // PGLiteSocketServer removes the handler from its bounded set.
      await handler.detach(false);
      handler.dispatchEvent(new CustomEvent("close"));
    })();
    let tracked;
    tracked = operation.then(
      (value) => {
        pendingHandlerCloses.delete(tracked);
        return value;
      },
      (error) => {
        pendingHandlerCloses.delete(tracked);
        throw error;
      },
    );
    handlerCloseTasks.set(handler, tracked);
    pendingHandlerCloses.add(tracked);
    observeFailure(tracked, "socket handler close failed");
    return tracked;
  };

  const awaitHandlerCloses = async () => {
    while (pendingHandlerCloses.size > 0) {
      await Promise.all([...pendingHandlerCloses]);
    }
  };

  const attach = PGLiteSocketHandler.prototype.attach;
  PGLiteSocketHandler.prototype.attach = function (...args) {
    const socket = args[0];
    const before = socket.listeners("close");
    const result = attach.apply(this, args);
    const after = socket.listeners("close");
    const upstreamCloseListeners = addedListeners(before, after);
    if (upstreamCloseListeners.length !== EXPECTED_UPSTREAM_CLOSE_LISTENERS) {
      throw new Error(
        `PGlite socket attach expected ${EXPECTED_UPSTREAM_CLOSE_LISTENERS} new close listener for @electric-sql/pglite-socket@${EXPECTED_PGLITE_SOCKET_VERSION}, found ${upstreamCloseListeners.length}`,
      );
    }

    // Replace exactly the close listener installed by the pinned upstream
    // attach() implementation. Preserve listeners owned by other layers and
    // install this before awaiting attach(), so an already-closing socket is
    // covered without an ordering gap.
    for (const listener of upstreamCloseListeners) {
      socket.removeListener("close", listener);
    }
    socket.on("close", () => {
      void closeHandler(this);
    });
    if (socket.destroyed || socket.readableEnded) {
      void closeHandler(this);
    }
    return result;
  };

  const handleConnection = PGLiteSocketServer.prototype.handleConnection;
  PGLiteSocketServer.prototype.handleConnection = function (...args) {
    const operation = (async () => {
      try {
        // A peer can destroy a socket before Node delivers its close event.
        // Drain such handlers before the upstream maxConnections check; the
        // cap itself remains unchanged and is still enforced by the original
        // implementation.
        const stale = [];
        for (const handler of this.handlers) {
          const socket = handler.socket;
          if (
            handlerCloseTasks.has(handler) ||
            !handler.active ||
            socket?.destroyed ||
            socket?.readableEnded
          ) {
            stale.push(closeHandler(handler));
          }
        }
        if (stale.length > 0) await Promise.all(stale);
        await awaitHandlerCloses();
        return await handleConnection.apply(this, args);
      } catch (error) {
        // Do not leave an incoming socket open when cleanup failed. Existing
        // handlers remain in the bounded set because no close event is
        // dispatched on the failed cleanup path.
        try {
          args[0]?.destroy?.();
        } catch {
          // Preserve the cleanup error as the operation's failure.
        }
        throw error;
      }
    })();
    return observeFailure(operation, "socket connection cleanup failed");
  };
}

export async function awaitHandlerDetaches() {
  while (pendingDetaches.size > 0 || pendingHandlerCloses.size > 0) {
    await Promise.allSettled([
      ...pendingDetaches,
      ...pendingHandlerCloses,
    ]);
  }
  if (cleanupFailures.size > 0) {
    throw new AggregateError(
      [...cleanupFailures],
      "PGlite socket cleanup failed; bounded handlers were not released",
    );
  }
}

export async function startPgliteServer({
  dataDir,
  maxConnections = 4,
  path,
  port,
} = {}) {
  installPgliteSocketCleanup();
  const database = dataDir ? await PGlite.create(dataDir) : await PGlite.create();
  const server = new PGLiteSocketServer(
    typeof port === "number"
      ? { db: database, port, host: "127.0.0.1", maxConnections }
      : { db: database, path, maxConnections },
  );
  await server.start();
  return { database, server };
}

async function runServer() {
  const socketPath = process.argv[2];
  const port = process.env.PGLITE_PORT ? Number(process.env.PGLITE_PORT) : undefined;
  const maxConnections = process.env.PGLITE_MAX_CONNECTIONS
    ? Number(process.env.PGLITE_MAX_CONNECTIONS)
    : 4;
  const dataDir = process.env.PGLITE_DATA_DIR;
  if (!socketPath && port === undefined) {
    throw new Error("usage: node server.mjs /tmp/mount-rs-pglite.sock");
  }

  let database;
  let server;
  try {
    ({ database, server } = await startPgliteServer({
      dataDir,
      maxConnections,
      path: socketPath,
      port,
    }));
    console.log(`PGLITE_READY ${typeof port === "number" ? `127.0.0.1:${server.port}` : socketPath}`);
    process.stdout.flush?.();

    await Promise.race([
      new Promise((resolve) => {
        process.once("SIGINT", resolve);
        process.once("SIGTERM", resolve);
      }),
      fatalSignal,
    ]);
  } finally {
    try {
      if (server) await server.stop();
    } catch (error) {
      reportFatal("PGlite socket server shutdown failed", error);
    }
    try {
      await awaitHandlerDetaches();
    } catch (error) {
      reportFatal("PGlite socket cleanup did not settle", error);
    }
    try {
      if (database) await database.close();
    } catch (error) {
      reportFatal("PGlite database shutdown failed", error);
    }
  }
  if (fatalError) throw fatalError;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) {
  await runServer();
}
