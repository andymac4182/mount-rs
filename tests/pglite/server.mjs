import { PGlite } from "@electric-sql/pglite";
import {
  PGLiteSocketHandler,
  PGLiteSocketServer,
} from "@electric-sql/pglite-socket";

const socketPath = process.argv[2];
const port = process.env.PGLITE_PORT ? Number(process.env.PGLITE_PORT) : undefined;
const maxConnections = process.env.PGLITE_MAX_CONNECTIONS
  ? Number(process.env.PGLITE_MAX_CONNECTIONS)
  : 4;
const dataDir = process.env.PGLITE_DATA_DIR;
if (!socketPath && !port) throw new Error("usage: node server.mjs /tmp/mount-rs-pglite.sock");

let fatalReported = false;
function reportFatal(kind, reason) {
  if (fatalReported) return;
  fatalReported = true;
  const error = reason instanceof Error ? reason : new Error(String(reason));
  const details = String(error.stack ?? error)
    .split("\n")
    .slice(0, 6)
    .map((line) => (line.length > 500 ? `${line.slice(0, 500)}…` : line))
    .join("\n");
  process.stderr.write(`[pglite-server] ${kind}\n${details}\n`);
  process.exitCode = 1;
  process.exit(1);
}

process.once("uncaughtException", (error) => {
  reportFatal("uncaughtException", error);
});
process.once("unhandledRejection", (reason) => {
  reportFatal("unhandledRejection", reason);
});

// pglite-socket 0.2.11 starts handler.detach() without awaiting it from both
// stop() and the socket close callback. Track the public handler method so the
// database is not closed until every asynchronous detach has settled.
const pendingDetaches = new Set();
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
      reportFatal("socket handler detach failed", error);
    },
  );
  pendingDetaches.add(tracked);
  return tracked;
};

const nextImmediate = () => new Promise((resolve) => setImmediate(resolve));
async function awaitHandlerDetaches() {
  // handleClose schedules detach(false) with setImmediate. Drain two event
  // loop turns around server.stop() before allowing PGlite.close() to clear
  // the database module used by clearTransactionIfNeeded().
  await nextImmediate();
  while (pendingDetaches.size > 0) {
    await Promise.all([...pendingDetaches]);
  }
  await nextImmediate();
  while (pendingDetaches.size > 0) {
    await Promise.all([...pendingDetaches]);
  }
}

const database = dataDir ? await PGlite.create(dataDir) : await PGlite.create();
const server = new PGLiteSocketServer(
  port
    ? { db: database, port, host: "127.0.0.1", maxConnections }
    : { db: database, path: socketPath, maxConnections },
);
await server.start();
console.log(`PGLITE_READY ${port ? `127.0.0.1:${port}` : socketPath}`);
process.stdout.flush?.();

await new Promise((resolve) => {
  process.once("SIGINT", resolve);
  process.once("SIGTERM", resolve);
});
await server.stop();
await awaitHandlerDetaches();
await database.close();
