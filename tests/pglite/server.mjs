import { PGlite } from "@electric-sql/pglite";
import { PGLiteSocketServer } from "@electric-sql/pglite-socket";

const socketPath = process.argv[2];
const port = process.env.PGLITE_PORT ? Number(process.env.PGLITE_PORT) : undefined;
const maxConnections = process.env.PGLITE_MAX_CONNECTIONS
  ? Number(process.env.PGLITE_MAX_CONNECTIONS)
  : 4;
const dataDir = process.env.PGLITE_DATA_DIR;
if (!socketPath && !port) throw new Error("usage: node server.mjs /tmp/mount-rs-pglite.sock");

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
await database.close();
