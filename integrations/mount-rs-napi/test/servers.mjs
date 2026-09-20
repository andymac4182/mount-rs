import assert from "node:assert/strict";
import * as net from "node:net";
import { pathToFileURL } from "node:url";

import {
  Filesystem,
  createNfsServer,
  createP9Server,
  createS3Server,
  createWebdavServer,
} from "../index.js";

let memoryFilesystem = () => Filesystem.memory();
if (process.env.MOUNT_RS_STRUCTURAL_SERVERS === "1") {
  assert.ok(process.env.MOUNTX_SOURCE, "structural server tests require the oracle");
  const { createMemoryDriver } = await import(pathToFileURL(`${process.env.MOUNTX_SOURCE}/src/drivers/memory.ts`).href);
  const { createLoopback } = await import(pathToFileURL(`${process.env.MOUNTX_SOURCE}/src/harness.ts`).href);
  memoryFilesystem = () => {
    const driver = createMemoryDriver();
    const loopback = createLoopback(driver);
    return { ...driver, writeFile: loopback.writeFile.bind(loopback), readFile: loopback.readFile.bind(loopback) };
  };
}

const IO_TIMEOUT_MS = 5_000;
let activePhase = { label: "startup", startedAt: Date.now() };

function phaseSummary() {
  return `${activePhase.label} (+${Date.now() - activePhase.startedAt}ms)`;
}

async function runPhase(label, task) {
  const previous = activePhase;
  activePhase = { label, startedAt: Date.now() };
  try {
    return await task();
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    throw new Error(`${label} failed after ${Date.now() - activePhase.startedAt}ms: ${message}`, {
      cause: error,
    });
  } finally {
    activePhase = previous;
  }
}

async function within(promise, label) {
  let timer;
  try {
    return await Promise.race([
      promise,
      new Promise((_, reject) => {
        timer = setTimeout(
          () =>
            reject(
              new Error(
                `${typeof label === "function" ? label() : label} timed out after ${IO_TIMEOUT_MS}ms`,
              ),
            ),
          IO_TIMEOUT_MS,
        );
      }),
    ]);
  } finally {
    if (timer !== undefined) {
      clearTimeout(timer);
    }
  }
}

async function waitForTransportError(reports, label) {
  const deadline = Date.now() + IO_TIMEOUT_MS;
  while (reports.length === 0) {
    const remaining = deadline - Date.now();
    if (remaining <= 0) {
      throw new Error(`${label} timed out`);
    }
    await new Promise((resolve) => setTimeout(resolve, Math.min(remaining, 10)));
  }
  return reports[0];
}

function writeSocket(socket, bytes, label) {
  return within(
    new Promise((resolve, reject) => {
      socket.write(bytes, (error) => (error ? reject(error) : resolve()));
    }),
    label,
  );
}

class BufferedSocket {
  #socket;
  #chunks = [];
  #bytes = 0;
  #waiters = [];
  #failure;

  constructor(socket) {
    this.#socket = socket;
    socket.on("data", (chunk) => {
      this.#chunks.push(Buffer.from(chunk));
      this.#bytes += chunk.length;
      this.#pump();
    });
    socket.on("error", (error) => this.#fail(error));
    socket.on("close", () => this.#fail(new Error("socket closed before the reply")));
  }

  readExactly(length) {
    if (this.#failure) {
      return Promise.reject(this.#failure);
    }
    if (this.#bytes >= length) {
      return Promise.resolve(this.#take(length));
    }
    return new Promise((resolve, reject) => {
      this.#waiters.push({ length, resolve, reject });
    });
  }

  #take(length) {
    const result = Buffer.allocUnsafe(length);
    let offset = 0;
    while (offset < length) {
      const chunk = this.#chunks[0];
      const count = Math.min(chunk.length, length - offset);
      chunk.copy(result, offset, 0, count);
      offset += count;
      this.#bytes -= count;
      if (count === chunk.length) {
        this.#chunks.shift();
      } else {
        this.#chunks[0] = chunk.subarray(count);
      }
    }
    return result;
  }

  #pump() {
    while (this.#waiters.length > 0 && this.#bytes >= this.#waiters[0].length) {
      const waiter = this.#waiters.shift();
      waiter.resolve(this.#take(waiter.length));
    }
  }

  #fail(error) {
    if (this.#failure) {
      return;
    }
    this.#failure = error;
    for (const waiter of this.#waiters.splice(0)) {
      waiter.reject(error);
    }
  }
}

async function connectLoopback(port) {
  const socket = net.createConnection({ host: "127.0.0.1", port });
  await within(
    new Promise((resolve, reject) => {
      socket.once("connect", resolve);
      socket.once("error", reject);
    }),
    `connect to ${port}`,
  );
  return { socket, reader: new BufferedSocket(socket) };
}

async function closeSocket(socket, label) {
  if (!socket.destroyed) {
    socket.destroy();
  }
  if (!socket.destroyed) {
    await within(new Promise((resolve) => socket.once("close", resolve)), label);
  }
}

async function listenLifecycle(server, label) {
  const listening = server.listen();
  assert.strictEqual(listening, server.listen(), `${label} listen is cached`);
  assert.strictEqual(await within(listening, `${label} listen`), server);
  assert.strictEqual(server.listen(), listening, `${label} does not rebind`);
  return { listening };
}

async function closeLifecycle(server, label, listening) {
  const closing = server.close();
  assert.strictEqual(closing, server.close(), `${label} close is cached`);
  await within(closing, `${label} close`);
  assert.strictEqual(server.listen(), listening, `${label} relisten is cached`);
  assert.strictEqual(
    await within(server[Symbol.asyncDispose](), `${label} async dispose`),
    undefined,
  );
}

function nfsNullCall(xid) {
  const call = Buffer.alloc(40);
  const words = [xid, 0, 2, 100_005, 3, 0, 0, 0, 0, 0];
  words.forEach((word, index) => call.writeUInt32BE(word, index * 4));
  return call;
}

function nfsRecord(record) {
  const marker = Buffer.alloc(4);
  marker.writeUInt32BE((0x8000_0000 | record.length) >>> 0);
  return Buffer.concat([marker, record]);
}

async function exerciseNfs() {
  const filesystem = memoryFilesystem();
  const reports = [];
  const server = createNfsServer(filesystem, {
    host: "127.0.0.1",
    port: 0,
    maxRecord: 256,
    onTransportError(error, peer) {
      reports.push({ error, peer });
      throw new Error("NFS hook callback deliberately threw");
    },
  });
  let socket;
  let serverReader;
  let listening;
  try {
    assert.equal(server.host, "127.0.0.1");
    assert.equal(server.port, 0);
    listening = (await listenLifecycle(server, "NFS")).listening;
    assert.ok(server.port > 0);

    ({ socket, reader: serverReader } = await connectLoopback(server.port));
    await writeSocket(socket, nfsRecord(nfsNullCall(41)), "NFS NULL call");
    const marker = (await within(serverReader.readExactly(4), "NFS record marker")).readUInt32BE(0);
    assert.equal(marker >>> 31, 1);
    const reply = await within(
      serverReader.readExactly(marker & 0x7fff_ffff),
      "NFS NULL reply",
    );
    assert.equal(reply.readUInt32BE(0), 41);
    assert.equal(reply.readUInt32BE(4), 1);
    assert.equal(reply.readUInt32BE(20), 0);

    // MOUNT '/' then GETATTR drives the filesystem stat callback over real
    // RPC, including when the input is a structural JavaScript driver.
    const mountCall = nfsNullCall(42);
    mountCall.writeUInt32BE(1, 20);
    await writeSocket(socket, nfsRecord(Buffer.concat([mountCall, Buffer.from([0, 0, 0, 1, 47, 0, 0, 0])])), "NFS MOUNT");
    const mountMarker = (await serverReader.readExactly(4)).readUInt32BE(0);
    const mountReply = await serverReader.readExactly(mountMarker & 0x7fff_ffff);
    assert.equal(mountReply.readUInt32BE(24), 0, "MOUNT status");
    const handleLength = mountReply.readUInt32BE(28);
    const handle = mountReply.subarray(28, 32 + ((handleLength + 3) & ~3));
    const getattr = nfsNullCall(43);
    getattr.writeUInt32BE(100_003, 12);
    getattr.writeUInt32BE(1, 20);
    await writeSocket(socket, nfsRecord(Buffer.concat([getattr, handle])), "NFS GETATTR");
    const attrMarker = (await serverReader.readExactly(4)).readUInt32BE(0);
    const attrReply = await serverReader.readExactly(attrMarker & 0x7fff_ffff);
    assert.equal(attrReply.readUInt32BE(24), 0, "GETATTR status");
    assert.equal(attrReply.readUInt32BE(28), 2, "root is a directory");

    // A decodable RPC body that the protocol layer cannot dispatch is not a
    // transport failure and must not reach onTransportError.
    await writeSocket(socket, nfsRecord(Buffer.from([1, 2, 3])), "NFS bad RPC body");
    await new Promise((resolve) => setTimeout(resolve, 25));
    assert.deepEqual(reports, []);

    // EOF is an ordinary disconnect. The second connection below is reserved
    // for the actual record-framing failure that should be reported.
    await closeSocket(socket, "NFS orderly disconnect");
    socket = undefined;
    await new Promise((resolve) => setTimeout(resolve, 25));
    assert.deepEqual(reports, []);

    ({ socket, reader: serverReader } = await connectLoopback(server.port));
    const malformedMarker = Buffer.alloc(4);
    malformedMarker.writeUInt32BE((0x8000_0000 | 257) >>> 0);
    await writeSocket(socket, malformedMarker, "NFS oversized record marker");
    const report = await waitForTransportError(reports, "NFS transport error");
    assert.ok(report.error instanceof Error);
    assert.match(report.error.message, /record|limit|maximum|larger/i);
    assert.equal(report.peer, "127.0.0.1");
    assert.equal(reports.length, 1);
  } finally {
    if (socket) {
      await runPhase("NFS cleanup: socket close", () => closeSocket(socket, "NFS socket close"));
    }
    await runPhase("NFS cleanup: server lifecycle", () =>
      closeLifecycle(server, "NFS", listening),
    );
    assert.equal(reports.length, 1);
  }
}

function p9String(value) {
  const bytes = Buffer.from(value);
  const length = Buffer.alloc(2);
  length.writeUInt16LE(bytes.length);
  return Buffer.concat([length, bytes]);
}

function p9Frame(type, tag, body = Buffer.alloc(0)) {
  const frame = Buffer.alloc(7 + body.length);
  frame.writeUInt32LE(frame.length, 0);
  frame.writeUInt8(type, 4);
  frame.writeUInt16LE(tag, 5);
  body.copy(frame, 7);
  return frame;
}

async function readP9Frame(reader, label) {
  const header = await within(reader.readExactly(7), `${label} header`);
  const size = header.readUInt32LE(0);
  assert.ok(size >= 7 && size <= 1024 * 1024, `${label} has a bounded frame size`);
  return Buffer.concat([
    header,
    await within(reader.readExactly(size - 7), `${label} body`),
  ]);
}

async function p9Request(socket, reader, type, tag, body, expectedType) {
  await writeSocket(socket, p9Frame(type, tag, body), `9P request ${type}`);
  const response = await readP9Frame(reader, `9P response ${expectedType}`);
  assert.equal(response[4], expectedType);
  assert.equal(response.readUInt16LE(5), tag);
  return response.subarray(7);
}

async function exerciseP9() {
  const filesystem = memoryFilesystem();
  const original = Buffer.from("before-9p");
  await filesystem.writeFile("/servers-9p.txt", original);
  const reports = [];
  const server = createP9Server(filesystem, {
    host: "127.0.0.1",
    port: 0,
    onTransportError(error, peer) {
      reports.push({ error, peer });
      throw new Error("9P hook callback deliberately threw");
    },
  });
  let socket;
  let serverReader;
  let connection;
  let listening;
  try {
    listening = (await listenLifecycle(server, "9P")).listening;
    assert.ok(server.port > 0);
    assert.match(server.address(), /^127\.0\.0\.1:\d+$/);

    ({ socket, reader: serverReader } = await connectLoopback(server.port));
    const versionBody = await p9Request(
      socket,
      serverReader,
      100,
      0xffff,
      Buffer.concat([Buffer.from([0x00, 0x00, 0x01, 0x00]), p9String("9P2000.L")]),
      101,
    );
    assert.equal(versionBody.readUInt32LE(0), 65_536);
    assert.equal(versionBody.subarray(6).toString(), "9P2000.L");

    [connection] = server.clients();
    assert.ok(connection);
    const session = connection.session;
    assert.strictEqual(connection.session, session);
    assert.equal(session.msize, 65_536);
    assert.equal(session.version, "9P2000.L");
    assert.equal(session.destroyed, false);
    assert.ok(session.stats.requests >= 1);

    // An unsupported message is a normal Rlerror protocol reply, not a
    // transport failure.
    await p9Request(socket, serverReader, 250, 7, Buffer.alloc(0), 7);
    assert.deepEqual(reports, []);

    const attachBody = Buffer.alloc(12);
    attachBody.writeUInt32LE(1, 0);
    attachBody.writeUInt32LE(0xffff_ffff, 4);
    attachBody.writeUInt32LE(0xffff_ffff, 8);
    await p9Request(
      socket,
      serverReader,
      104,
      1,
      Buffer.concat([attachBody.subarray(0, 8), p9String("node"), p9String(""), attachBody.subarray(8)]),
      105,
    );
    assert.ok(server.connections >= 1);

    const walkBody = Buffer.alloc(10);
    walkBody.writeUInt32LE(1, 0);
    walkBody.writeUInt32LE(2, 4);
    walkBody.writeUInt16LE(1, 8);
    const walkReply = await p9Request(
      socket,
      serverReader,
      110,
      2,
      Buffer.concat([walkBody, p9String("servers-9p.txt")]),
      111,
    );
    assert.equal(walkReply.readUInt16LE(0), 1);

    const lopenBody = Buffer.alloc(8);
    lopenBody.writeUInt32LE(2, 0);
    lopenBody.writeUInt32LE(2, 4);
    await p9Request(socket, serverReader, 12, 3, lopenBody, 13);

    const writeData = Buffer.from("after-9p");
    const writeBody = Buffer.alloc(16 + writeData.length);
    writeBody.writeUInt32LE(2, 0);
    writeBody.writeBigUInt64LE(0n, 4);
    writeBody.writeUInt32LE(writeData.length, 12);
    writeData.copy(writeBody, 16);
    const writeReply = await p9Request(socket, serverReader, 118, 4, writeBody, 119);
    assert.equal(writeReply.readUInt32LE(0), writeData.length);

    const readBody = Buffer.alloc(16);
    readBody.writeUInt32LE(2, 0);
    readBody.writeBigUInt64LE(0n, 4);
    readBody.writeUInt32LE(writeData.length, 12);
    const readReply = await p9Request(socket, serverReader, 116, 5, readBody, 117);
    assert.equal(readReply.readUInt32LE(0), writeData.length);
    assert.deepEqual(readReply.subarray(4), writeData);

    const clunkBody = Buffer.alloc(4);
    clunkBody.writeUInt32LE(2, 0);
    await p9Request(socket, serverReader, 120, 6, clunkBody, 121);

    // Orderly disconnects are deliberately silent. Reconnect for the bounded
    // framing failure below so the two cases cannot race one another.
    await closeSocket(socket, "9P orderly disconnect");
    socket = undefined;
    await within(connection.closed, "9P orderly connection closed");
    assert.equal(connection.session.destroyed, true);
    await new Promise((resolve) => setTimeout(resolve, 25));
    assert.deepEqual(reports, []);

    ({ socket, reader: serverReader } = await connectLoopback(server.port));
    await p9Request(
      socket,
      serverReader,
      100,
      0xffff,
      Buffer.concat([Buffer.from([0x00, 0x00, 0x01, 0x00]), p9String("9P2000.L")]),
      101,
    );
    [connection] = server.clients();
    assert.ok(connection);

    // A size below the 7-byte 9P header cannot be resynchronized and is a
    // transport framing failure. The callback's deliberate throw must be
    // caught by the TSFN bridge rather than becoming an uncaught exception.
    const malformedFrame = Buffer.alloc(7);
    malformedFrame.writeUInt32LE(6, 0);
    await writeSocket(socket, malformedFrame, "9P malformed frame");
    const report = await waitForTransportError(reports, "9P transport error");
    assert.ok(report.error instanceof Error);
    assert.match(report.error.message, /header|frame|size|below/i);
    assert.match(report.peer, /^127\.0\.0\.1:\d+$/);
    assert.equal(reports.length, 1);
  } finally {
    if (socket) {
      await runPhase("9P cleanup: socket close", () => closeSocket(socket, "9P socket close"));
    }
    if (connection) {
      await runPhase("9P cleanup: connection close", () =>
        within(connection.closed, "9P connection closed"),
      );
      assert.equal(connection.session.destroyed, true);
    }
    await runPhase("9P cleanup: server lifecycle", () =>
      closeLifecycle(server, "9P", listening),
    );
    assert.equal(reports.length, 1);
  }
}

async function fetchBody(url, init, label) {
  const response = await within(
    fetch(url, { ...init, signal: AbortSignal.timeout(IO_TIMEOUT_MS) }),
    `${label} request`,
  );
  const body = await within(response.arrayBuffer(), `${label} body`);
  return { response, body: Buffer.from(body) };
}

async function exerciseS3() {
  const photos = memoryFilesystem();
  const notes = memoryFilesystem();
  const server = createS3Server({ buckets: { photos, notes } }, {
    bucket: "mountx",
    host: "127.0.0.1",
    port: 0,
  });
  let listening;
  try {
    assert.equal(server.port, 0);
    listening = (await listenLifecycle(server, "S3")).listening;
    assert.ok(server.port > 0);
    assert.equal(server.url.endsWith("/"), false);
    assert.deepEqual(server.buckets, ["notes", "photos"]);

    const object = Buffer.from("S3 over the real loopback HTTP listener");
    const put = await fetchBody(
      `${server.url}/photos/servers-s3.txt`,
      { method: "PUT", body: object },
      "S3 PUT",
    );
    assert.ok(put.response.status >= 200 && put.response.status < 300);

    const get = await fetchBody(
      `${server.url}/photos/servers-s3.txt`,
      { method: "GET" },
      "S3 GET",
    );
    assert.equal(get.response.status, 200);
    assert.deepEqual(get.body, object);

    const isolated = await fetchBody(
      `${server.url}/notes/servers-s3.txt`,
      { method: "GET" },
      "S3 bucket isolation",
    );
    assert.equal(isolated.response.status, 404);
    assert.deepEqual(Buffer.from(await photos.readFile("/servers-s3.txt")), object);
  } finally {
    await runPhase("S3 cleanup: server lifecycle", () =>
      closeLifecycle(server, "S3", listening),
    );
  }
}

async function exerciseWebdav() {
  const filesystem = memoryFilesystem();
  const server = createWebdavServer(filesystem, { host: "127.0.0.1", port: 0 });
  let listening;
  try {
    listening = (await listenLifecycle(server, "WebDAV")).listening;
    assert.ok(server.port > 0);
    assert.equal(server.url.endsWith("/"), false);

    const object = Buffer.from("WebDAV over the real loopback HTTP listener");
    const put = await fetchBody(
      `${server.url}/servers-webdav.txt`,
      { method: "PUT", body: object },
      "WebDAV PUT",
    );
    assert.ok([200, 201, 204].includes(put.response.status));

    const get = await fetchBody(
      `${server.url}/servers-webdav.txt`,
      { method: "GET" },
      "WebDAV GET",
    );
    assert.equal(get.response.status, 200);
    assert.deepEqual(get.body, object);
  } finally {
    await runPhase("WebDAV cleanup: server lifecycle", () =>
      closeLifecycle(server, "WebDAV", listening),
    );
  }
}

await within(
  (async () => {
    await runPhase("NFS exercise", exerciseNfs);
    await runPhase("9P exercise", exerciseP9);
    await runPhase("S3 exercise", exerciseS3);
    await runPhase("WebDAV exercise", exerciseWebdav);
  })(),
  () => `N-API server integration (${phaseSummary()})`,
);

console.log("mount-rs N-API server integration: PASS");
