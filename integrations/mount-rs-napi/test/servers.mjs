import assert from "node:assert/strict";
import * as net from "node:net";
import { Duplex } from "node:stream";
import { pathToFileURL } from "node:url";

import {
  Filesystem,
  NfsConnection,
  Nfs4Session,
  NfsSession,
  createNfsServer,
  createP9Server,
  createS3Server,
  createWebdavServer,
  WebdavSession,
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

async function waitUntil(predicate, label) {
  const deadline = Date.now() + IO_TIMEOUT_MS;
  while (!predicate()) {
    if (Date.now() >= deadline) throw new Error(`${label} timed out`);
    await new Promise((resolve) => setTimeout(resolve, 1));
  }
}

function duplexPair(highWaterMark = 16 * 1024) {
  let left;
  let right;
  const make = () => new Duplex({
    readableHighWaterMark: highWaterMark,
    writableHighWaterMark: highWaterMark,
    read() {},
    write(chunk, _encoding, callback) {
      const peer = this === left ? right : left;
      peer.push(Buffer.from(chunk));
      callback();
    },
    final(callback) {
      const peer = this === left ? right : left;
      peer.push(null);
      callback();
    },
  });
  left = make();
  right = make();
  return [left, right];
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

function nfsV4NullCall(xid) {
  const call = nfsNullCall(xid);
  call.writeUInt32BE(100_003, 12);
  call.writeUInt32BE(4, 16);
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
    maxHandles: 2,
    onTransportError(error, peer) {
      reports.push({ error, peer });
      throw new Error("NFS hook callback deliberately threw");
    },
  });
  let socket;
  let serverReader;
  let listening;
  let nfsConnection;
  try {
    assert.equal(server.host, "127.0.0.1");
    assert.equal(server.port, 0);
    listening = (await listenLifecycle(server, "NFS")).listening;
    assert.ok(server.port > 0);
    assert.equal(server.connections, 0);
    assert.ok(server.session instanceof NfsSession);
    assert.ok(server.session.v4 instanceof Nfs4Session);
    assert.deepEqual(server.session.mounts, []);
    assert.equal(server.session.destroyed, false);
    assert.deepEqual(server.session.stats, {
      requests: 0,
      replies: 0,
      errors: 0,
      dropped: 0,
      procedures: {},
    });

    const directReply = await server.session.handleCall(nfsNullCall(40));
    assert.ok(Buffer.isBuffer(directReply));
    assert.equal(directReply.readUInt32BE(0), 40);
    assert.equal(directReply.readUInt32BE(20), 0);
    assert.equal(server.session.stats.requests, 1);

    const directV4Reply = await server.session.handleCall(nfsV4NullCall(44));
    assert.ok(Buffer.isBuffer(directV4Reply));
    assert.equal(directV4Reply.readUInt32BE(0), 44);
    assert.equal(directV4Reply.readUInt32BE(4), 1);
    assert.equal(directV4Reply.readUInt32BE(20), 0);
    assert.equal(server.session.v4.destroyed, false);
    assert.equal(server.session.stats.requests, 2);
    assert.equal(server.session.stats.procedures["NFS4:NULL"], 1);
    assert.equal(server.session.v4.stats.requests, 2);
    const rootHandle = [{ id: 1n, fileid: 1n, path: "/" }];
    assert.deepEqual(server.session.handles, rootHandle);
    assert.deepEqual(server.session.v4.handles, rootHandle);

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
    assert.ok(server.connections >= 1);
    await waitUntil(() => server.clients().length === 1, "NFS client registration");
    [nfsConnection] = server.clients();
    assert.ok(nfsConnection instanceof NfsConnection);
    assert.equal(nfsConnection.id, 1);
    assert.match(nfsConnection.peer, /^127\.0\.0\.1:\d+$/);
    assert.equal(nfsConnection.isClosed, false);
    assert.ok(nfsConnection.session instanceof NfsSession);
    assert.ok(nfsConnection.session.v4 instanceof Nfs4Session);
    assert.ok(nfsConnection.closed instanceof Promise);
    assert.strictEqual(nfsConnection.closed, nfsConnection.closed);

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

    // A connection object can request transport teardown and await its closed
    // state. The second connection below is reserved for the actual
    // record-framing failure that should be reported.
    await within(nfsConnection.close(), "NFS connection close");
    await within(nfsConnection.closed, "NFS connection closed");
    assert.equal(nfsConnection.isClosed, true);
    assert.equal(server.connections, 0);
    assert.equal(server.clients().length, 0);
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
    assert.equal(server.connections, 0);
    assert.equal(server.session.destroyed, true);
    assert.equal(server.session.v4.destroyed, true);
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
    assert.equal(connection.stream, undefined);
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

async function exerciseP9AttachedStream() {
  const filesystem = memoryFilesystem();
  const server = createP9Server(filesystem, { host: "127.0.0.1", port: 0 });
  const listener = net.createServer();
  await new Promise((resolve, reject) => {
    listener.once("error", reject);
    listener.listen(0, "127.0.0.1", resolve);
  });
  const address = listener.address();
  assert.ok(address && typeof address === "object");
  const acceptedPromise = new Promise((resolve, reject) => {
    listener.once("connection", resolve);
    listener.once("error", reject);
  });
  const client = net.createConnection({ host: "127.0.0.1", port: address.port });
  await new Promise((resolve, reject) => {
    client.once("connect", resolve);
    client.once("error", reject);
  });
  const accepted = await acceptedPromise;
  const connection = server.attach(accepted, { peer: "attached-test", own: true });
  assert.strictEqual(connection.stream, accepted);
  assert.equal(connection.peer, "attached-test");
  assert.equal(server.connections, 1);
  assert.strictEqual(server.clients()[0], connection);
  assert.throws(
    () => server.attach(accepted),
    /already attached/,
  );

  const reader = new BufferedSocket(client);
  const version = await p9Request(
    client,
    reader,
    100,
    0xffff,
    Buffer.concat([Buffer.from([0x00, 0x00, 0x01, 0x00]), p9String("9P2000.L")]),
    101,
  );
  assert.equal(version.readUInt32LE(0), 65_536);
  const direct = await connection.session.handleCall(
    p9Frame(
      100,
      0xfffe,
      Buffer.concat([Buffer.from([0x00, 0x00, 0x01, 0x00]), p9String("9P2000.L")]),
    ),
  );
  assert.ok(Buffer.isBuffer(direct));
  assert.equal(direct[4], 101);
  assert.equal(direct.readUInt16LE(5), 0xfffe);
  assert.equal(await connection.session.handleCall(Buffer.from([1, 2, 3])), null);

  await connection.close();
  await within(connection.closed, "attached 9P connection close");
  assert.equal(connection.isClosed, true);
  assert.equal(server.connections, 0);
  await server.close();
  await new Promise((resolve) => listener.close(resolve));
  if (!client.destroyed) client.destroy();
}

async function exerciseP9AttachedDuplex() {
  const server = createP9Server(memoryFilesystem(), { maxInFlight: 2 });
  const [serverSide, clientSide] = duplexPair();
  const connection = server.attach(serverSide);
  const reader = new BufferedSocket(clientSide);
  await p9Request(
    clientSide,
    reader,
    100,
    0xffff,
    Buffer.concat([Buffer.from([0x00, 0x00, 0x01, 0x00]), p9String("9P2000.L")]),
    101,
  );
  assert.strictEqual(connection.stream, serverSide);
  assert.equal(connection.peer, undefined);
  const peerEof = new Promise((resolve) => clientSide.once("end", resolve));
  await server.close();
  await within(connection.closed, "attached duplex server close");
  await within(peerEof, "attached duplex peer EOF");
  assert.equal(connection.session.destroyed, true);
  assert.equal(connection.isClosed, true);
  assert.equal(server.connections, 0);
  assert.equal(serverSide.destroyed, false);
  clientSide.destroy();
  serverSide.destroy();
}

async function exerciseP9AttachedBackpressure() {
  const pending = [];
  const stream = new Duplex({
    readableHighWaterMark: 1,
    writableHighWaterMark: 1,
    read() {},
    write(chunk, _encoding, callback) {
      pending.push({ chunk: Buffer.from(chunk), callback });
    },
  });
  const server = createP9Server(memoryFilesystem(), { maxInFlight: 4 });
  const connection = server.attach(stream, {
    peer: "backpressure-test",
    own: false,
    maxFrame: 256,
    maxInFlight: 1,
  });
  const version = p9Frame(
    100,
    0xffff,
    Buffer.concat([Buffer.from([0x00, 0x00, 0x01, 0x00]), p9String("9P2000.L")]),
  );
  stream.push(Buffer.concat([version, p9Frame(250, 1)]));
  await waitUntil(() => pending.length === 1, "attached backpressure first reply");
  assert.equal(connection.session.stats.requests, 1);
  assert.equal(stream.isPaused(), true);
  pending.shift().callback();
  await waitUntil(
    () => pending.length === 1 && connection.session.stats.requests === 2,
    "attached backpressure second reply",
  );
  assert.equal(stream.isPaused(), true);
  pending.shift().callback();
  await waitUntil(() => connection.session.stats.replies >= 2, "attached backpressure drain");
  await connection.close();
  await within(connection.closed, "attached backpressure close");
  await server.close();
  assert.equal(connection.session.destroyed, true);
  stream.destroy();
}

async function exerciseP9AttachedFrameLimit() {
  const reports = [];
  const stream = new Duplex({
    read() {},
    write(_chunk, _encoding, callback) {
      callback();
    },
  });
  const server = createP9Server(memoryFilesystem(), {
    onTransportError(error, peer) {
      reports.push({ error, peer });
    },
  });
  const connection = server.attach(stream, {
    peer: "attached-frame-limit",
    own: false,
    maxFrame: 32,
  });
  const oversized = Buffer.alloc(33);
  oversized.writeUInt32LE(33, 0);
  stream.push(oversized);
  await within(connection.closed, "attached frame-limit close");
  assert.equal(reports.length, 1);
  assert.match(reports[0].error.message, /invalid 9P frame size 33/);
  assert.equal(reports[0].peer, "attached-frame-limit");
  assert.equal(connection.session.destroyed, true);
  await server.close();
  stream.destroy();
}

async function exerciseP9AttachedWriteFailure() {
  const reports = [];
  const stream = new Duplex({
    read() {},
    write() {
      throw new Error("attached stream refuses to carry a reply");
    },
  });
  const server = createP9Server(memoryFilesystem(), {
    onTransportError(error, peer) {
      reports.push({ error, peer });
    },
  });
  const connection = server.attach(stream, { peer: "hostile-test", own: false });
  stream.push(
    p9Frame(
      100,
      0xffff,
      Buffer.concat([Buffer.from([0x00, 0x00, 0x01, 0x00]), p9String("9P2000.L")]),
    ),
  );
  await within(connection.closed, "attached hostile stream close");
  assert.equal(reports.length, 1);
  assert.match(reports[0].error.message, /refuses to carry/);
  assert.equal(reports[0].peer, "hostile-test");
  assert.equal(connection.session.destroyed, true);
  assert.equal(server.connections, 0);
  await server.close();
  assert.equal(stream.destroyed, false);
  stream.destroy();
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
    drainTimeout: 1000,
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

    const beforeStreamStats = await server.session.stats();
    const streamedObject = Buffer.alloc(300 * 1024, 0x37);
    let requestChunks = 0;
    async function* streamedRequestBody() {
      for (const start of [0, 100 * 1024, 200 * 1024]) {
        requestChunks += 1;
        await new Promise((resolve) => setImmediate(resolve));
        yield streamedObject.subarray(start, start + 100 * 1024);
      }
    }
    const streamedPut = await within(
      server.session.handleRequestStream(
        {
          method: "PUT",
          target: "/photos/streamed-s3.txt",
          headers: [{ name: "content-length", value: String(streamedObject.length) }],
        },
        streamedRequestBody(),
      ),
      "S3 streamed PUT",
    );
    assert.equal(streamedPut.status, 200);
    assert.ok(streamedPut.body);
    for await (const _chunk of streamedPut.body) {}
    assert.equal(requestChunks, 3);
    assert.deepEqual(
      Buffer.from(await photos.readFile("/streamed-s3.txt")),
      streamedObject,
    );

    const emptyRequest = () => new ReadableStream({
      start(controller) {
        controller.close();
      },
    });
    const streamedGet = await within(
      server.session.handleRequestStream(
        { method: "GET", target: "/photos/streamed-s3.txt", headers: [] },
        emptyRequest(),
      ),
      "S3 streamed GET",
    );
    assert.equal(streamedGet.status, 200);
    assert.ok(streamedGet.body);
    const streamedChunks = [];
    for await (const chunk of streamedGet.body) streamedChunks.push(Buffer.from(chunk));
    assert.ok(streamedChunks.length >= 3);
    assert.deepEqual(Buffer.concat(streamedChunks), streamedObject);

    const cancelledGet = await within(
      server.session.handleRequestStream(
        { method: "GET", target: "/photos/streamed-s3.txt", headers: [] },
        emptyRequest(),
      ),
      "S3 streamed cancellation setup",
    );
    const cancelledIterator = cancelledGet.body[Symbol.asyncIterator]();
    const firstCancelledChunk = await cancelledIterator.next();
    assert.equal(firstCancelledChunk.done, false);
    assert.ok(firstCancelledChunk.value.length > 0);
    await cancelledIterator.return();
    assert.equal((await cancelledIterator.next()).done, true);

    async function* failingRequestBody() {
      yield Buffer.from("partial");
      throw new Error("deliberate S3 request stream failure");
    }
    const failedStream = await within(
      server.session.handleRequestStream(
        {
          method: "PUT",
          target: "/photos/streamed-failure.txt",
          headers: [{ name: "content-length", value: "7" }],
        },
        failingRequestBody(),
      ),
      "S3 streamed request failure",
    );
    assert.equal(failedStream.status, 400);

    const streamedStats = await server.session.stats();
    assert.equal(streamedStats.requests, beforeStreamStats.requests + 4);
    assert.equal(streamedStats.replies, beforeStreamStats.replies + 4);
    assert.equal(streamedStats.errors, beforeStreamStats.errors + 1);
    assert.equal(
      streamedStats.operations.PutObject,
      (beforeStreamStats.operations.PutObject ?? 0) + 2,
    );
    assert.equal(
      streamedStats.operations.GetObject,
      (beforeStreamStats.operations.GetObject ?? 0) + 2,
    );
  } finally {
    await runPhase("S3 cleanup: server lifecycle", () =>
      closeLifecycle(server, "S3", listening),
    );
  }
}

async function exerciseWebdav() {
  const filesystem = memoryFilesystem();
  const reports = [];
  const server = createWebdavServer(filesystem, {
    host: "127.0.0.1",
    port: 0,
    realm: "mount-rs-integration",
    readChunkBytes: 16 * 1024,
    maxXmlBytes: 128 * 1024,
    maxBodyBytes: 1024 * 1024,
    locks: {
      defaultTimeoutSeconds: 30,
      maxTimeoutSeconds: 60,
      maxLocks: 4,
    },
    debug: true,
    onTransportError(error, peer) {
      reports.push({ error, peer });
    },
  });
  let listening;
  let faultSocket;
  try {
    listening = (await listenLifecycle(server, "WebDAV")).listening;
    assert.ok(server.port > 0);
    assert.equal(server.url.endsWith("/"), false);
    assert.ok(server.session instanceof WebdavSession);
    assert.ok(server.session.driver instanceof Filesystem);
    assert.deepEqual(server.session.assertions, []);
    assert.deepEqual(server.session.options, {
      realm: "mount-rs-integration",
      readChunkBytes: 16 * 1024,
      maxXmlBytes: 128 * 1024,
      maxBodyBytes: 1024 * 1024,
      locks: {
        defaultTimeoutSeconds: 30,
        maxTimeoutSeconds: 60,
        maxLocks: 4,
      },
      debug: true,
    });

    const object = Buffer.from("WebDAV over the real loopback HTTP listener");
    const direct = await server.session.handleRequest(
      { method: "PUT", target: "/direct-webdav.txt", headers: [] },
      object,
    );
    assert.ok([200, 201, 204].includes(direct.status));
    assert.equal(direct.body ?? null, null);

    const lock = await server.session.handleRequest(
      {
        method: "LOCK",
        target: "/direct-webdav.txt",
        headers: [{ name: "depth", value: "0" }],
      },
      Buffer.from('<D:lockinfo xmlns:D="DAV:"><D:lockscope><D:exclusive/></D:lockscope><D:locktype><D:write/></D:locktype></D:lockinfo>'),
    );
    assert.equal(lock.status, 200);
    const lockToken = lock.headers.find(({ name }) => name === "lock-token")?.value;
    assert.match(lockToken ?? "", /^<urn:uuid:/);
    assert.equal(server.session.locks.length, 1);
    assert.deepEqual(
      {
        path: server.session.locks[0].path,
        depth: server.session.locks[0].depth,
        exclusive: server.session.locks[0].exclusive,
        timeoutSeconds: server.session.locks[0].timeoutSeconds,
      },
      {
        path: "/direct-webdav.txt",
        depth: "0",
        exclusive: true,
        timeoutSeconds: 30,
      },
    );
    const unlock = await server.session.handleRequest(
      {
        method: "UNLOCK",
        target: "/direct-webdav.txt",
        headers: [{ name: "lock-token", value: lockToken }],
      },
      null,
    );
    assert.equal(unlock.status, 204);
    assert.equal(server.session.locks.length, 0);

    const directRequest = (method, target, headers = [], body = null) =>
      server.session.handleRequest({ method, target, headers }, body);
    const lockBody = Buffer.from('<D:lockinfo xmlns:D="DAV:"><D:lockscope><D:exclusive/></D:lockscope><D:locktype><D:write/></D:locktype></D:lockinfo>');
    const conflictLock = await directRequest(
      "LOCK",
      "/lock-conflict.txt",
      [{ name: "depth", value: "0" }],
      lockBody,
    );
    assert.ok([200, 201].includes(conflictLock.status));
    const conflictToken = conflictLock.headers.find(({ name }) => name === "lock-token")?.value;
    assert.match(conflictToken ?? "", /^<urn:uuid:/);
    const blockedWrite = await directRequest(
      "PUT",
      "/lock-conflict.txt",
      [],
      Buffer.from("blocked by the active lock"),
    );
    assert.equal(blockedWrite.status, 423);
    const conflictUnlock = await directRequest(
      "UNLOCK",
      "/lock-conflict.txt",
      [{ name: "lock-token", value: conflictToken }],
    );
    assert.equal(conflictUnlock.status, 204);

    const expiringLock = await directRequest(
      "LOCK",
      "/lock-expiring.txt",
      [
        { name: "depth", value: "0" },
        { name: "timeout", value: "Second-1" },
      ],
      lockBody,
    );
    assert.ok([200, 201].includes(expiringLock.status));
    assert.equal(server.session.locks.length, 1);
    assert.equal(server.session.locks[0].timeoutSeconds, 1);
    await new Promise((resolve) => setTimeout(resolve, 1_100));
    assert.equal(server.session.locks.length, 0);

    const options = await directRequest("OPTIONS", "/direct-methods");
    assert.equal(options.status, 200);
    const mkcol = await directRequest("MKCOL", "/direct-methods");
    assert.ok([201, 204].includes(mkcol.status));
    const methodObject = Buffer.from("direct WebDAV method coverage");
    const methodPut = await directRequest(
      "PUT",
      "/direct-methods/source.txt",
      [],
      methodObject,
    );
    assert.ok([200, 201, 204].includes(methodPut.status));
    const head = await directRequest("HEAD", "/direct-methods/source.txt");
    assert.equal(head.status, 200);
    assert.equal(head.body ?? null, null);
    assert.equal(
      Number(head.headers.find(({ name }) => name === "content-length")?.value),
      methodObject.length,
    );
    const methodGet = await directRequest("GET", "/direct-methods/source.txt");
    assert.equal(methodGet.status, 200);
    assert.deepEqual(methodGet.body, methodObject);
    const propfind = await directRequest(
      "PROPFIND",
      "/direct-methods",
      [{ name: "depth", value: "1" }],
      Buffer.from('<D:propfind xmlns:D="DAV:"><D:allprop/></D:propfind>'),
    );
    assert.equal(propfind.status, 207);
    assert.match(propfind.body.toString("utf8"), /<multistatus xmlns="DAV:">/);
    const proppatch = await directRequest(
      "PROPPATCH",
      "/direct-methods/source.txt",
      [],
      Buffer.from('<D:propertyupdate xmlns:D="DAV:"><D:set><D:prop><D:getlastmodified>2026-09-21T00:00:00.000Z</D:getlastmodified></D:prop></D:set></D:propertyupdate>'),
    );
    assert.equal(proppatch.status, 207);
    const copy = await directRequest(
      "COPY",
      "/direct-methods/source.txt",
      [{ name: "destination", value: "/direct-methods/copy.txt" }],
    );
    assert.ok([201, 204].includes(copy.status));
    const move = await directRequest(
      "MOVE",
      "/direct-methods/copy.txt",
      [{ name: "destination", value: "/direct-methods/moved.txt" }],
    );
    assert.ok([201, 204].includes(move.status));
    const methodLock = await directRequest(
      "LOCK",
      "/direct-methods/moved.txt",
      [{ name: "depth", value: "0" }],
      Buffer.from('<D:lockinfo xmlns:D="DAV:"><D:lockscope><D:exclusive/></D:lockscope><D:locktype><D:write/></D:locktype></D:lockinfo>'),
    );
    assert.equal(methodLock.status, 200);
    const methodLockToken = methodLock.headers.find(({ name }) => name === "lock-token")?.value;
    assert.match(methodLockToken ?? "", /^<urn:uuid:/);
    assert.match(methodLock.body.toString("utf8"), /<lockdiscovery>/);
    const methodUnlock = await directRequest(
      "UNLOCK",
      "/direct-methods/moved.txt",
      [{ name: "lock-token", value: methodLockToken }],
    );
    assert.equal(methodUnlock.status, 204);
    const unsupported = await directRequest("PATCH", "/direct-methods/moved.txt");
    assert.equal(unsupported.status, 405);
    const remove = await directRequest("DELETE", "/direct-methods/moved.txt");
    assert.equal(remove.status, 204);

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

    const streamedObject = Buffer.alloc(40 * 1024, 0x5a);
    let requestChunks = 0;
    async function* streamedRequestBody() {
      for (const start of [0, 7 * 1024, 23 * 1024]) {
        requestChunks += 1;
        await new Promise((resolve) => setImmediate(resolve));
        yield streamedObject.subarray(
          start,
          start === 0 ? 7 * 1024 : start === 7 * 1024 ? 23 * 1024 : undefined,
        );
      }
    }
    const streamedPut = await within(
      server.session.handleRequestStream(
        { method: "PUT", target: "/streamed-webdav.txt", headers: [] },
        streamedRequestBody(),
      ),
      "WebDAV streamed PUT",
    );
    assert.ok([200, 201, 204].includes(streamedPut.status));
    assert.equal(streamedPut.body ?? null, null);
    assert.equal(requestChunks, 3);
    assert.deepEqual(
      Buffer.from(await filesystem.readFile("/streamed-webdav.txt")),
      streamedObject,
    );

    const emptyRequest = () => new ReadableStream({
      start(controller) {
        controller.close();
      },
    });
    const streamedGet = await within(
      server.session.handleRequestStream(
        { method: "GET", target: "/streamed-webdav.txt", headers: [] },
        emptyRequest(),
      ),
      "WebDAV streamed GET",
    );
    assert.equal(streamedGet.status, 200);
    assert.ok(streamedGet.body);
    const streamedChunks = [];
    for await (const chunk of streamedGet.body) streamedChunks.push(Buffer.from(chunk));
    assert.ok(streamedChunks.length >= 3);
    assert.deepEqual(Buffer.concat(streamedChunks), streamedObject);

    const cancelledGet = await within(
      server.session.handleRequestStream(
        { method: "GET", target: "/streamed-webdav.txt", headers: [] },
        emptyRequest(),
      ),
      "WebDAV streamed cancellation setup",
    );
    const cancelledIterator = cancelledGet.body[Symbol.asyncIterator]();
    const firstCancelledChunk = await cancelledIterator.next();
    assert.equal(firstCancelledChunk.done, false);
    assert.ok(firstCancelledChunk.value.length > 0);
    await cancelledIterator.return();
    assert.equal((await cancelledIterator.next()).done, true);

    let emittedFailureChunk = false;
    async function* failingRequestBody() {
      emittedFailureChunk = true;
      yield Buffer.from("partial");
      throw new Error("deliberate WebDAV request stream failure");
    }
    const failedStream = await within(
      server.session.handleRequestStream(
        { method: "PUT", target: "/streamed-failure.txt", headers: [] },
        failingRequestBody(),
      ),
      "WebDAV streamed request failure",
    );
    assert.equal(emittedFailureChunk, true);
    assert.equal(failedStream.status, 500);

    await filesystem.writeFile(
      "/peer-fault-webdav.txt",
      Buffer.alloc(4 * 1024 * 1024, 0x2d),
    );
    const faultReplyCount = server.session.stats.replies;
    faultSocket = (await connectLoopback(server.port)).socket;
    await writeSocket(
      faultSocket,
      Buffer.from(
        `GET /peer-fault-webdav.txt HTTP/1.1\r\nHost: 127.0.0.1:${server.port}\r\n\r\n`,
      ),
      "WebDAV JavaScript peer-fault request",
    );
    await within(
      (async () => {
        while (server.session.stats.replies <= faultReplyCount) {
          await new Promise((resolve) => setImmediate(resolve));
        }
      })(),
      "WebDAV JavaScript peer-fault response readiness",
    );
    faultSocket.destroy(new Error("deliberate WebDAV peer reset"));
    const faultReport = await waitForTransportError(reports, "WebDAV JavaScript peer fault");
    assert.ok(faultReport.error instanceof Error);
    assert.match(faultReport.peer, /^127\.0\.0\.1:\d+$/);
    assert.equal(reports.length, 1);
    faultSocket = undefined;

    const authServer = createWebdavServer(filesystem, {
      host: "127.0.0.1",
      credentials: { username: "alice", password: "secret" },
      realm: "private",
    });
    try {
      assert.deepEqual(authServer.session.options.credentials, {
        username: "alice",
        password: "secret",
      });
      const unauthenticated = await authServer.session.handleRequest(
        { method: "OPTIONS", target: "/", headers: [] },
        null,
      );
      assert.equal(unauthenticated.status, 401);
      assert.match(
        unauthenticated.headers.find(({ name }) => name === "www-authenticate")?.value ?? "",
        /^Basic realm="private"/,
      );
      const authenticated = await authServer.session.handleRequest(
        {
          method: "OPTIONS",
          target: "/",
          headers: [{
            name: "authorization",
            value: `Basic ${Buffer.from("alice:secret").toString("base64")}`,
          }],
        },
        null,
      );
      assert.equal(authenticated.status, 200);
    } finally {
      await authServer.close();
    }
  } finally {
    if (faultSocket) {
      await runPhase("WebDAV cleanup: fault socket close", () =>
        closeSocket(faultSocket, "WebDAV fault socket close"),
      );
    }
    await runPhase("WebDAV cleanup: server lifecycle", () =>
      closeLifecycle(server, "WebDAV", listening),
    );
    assert.equal(reports.length, 1);
  }
}

await within(
  (async () => {
    await runPhase("NFS exercise", exerciseNfs);
    await runPhase("9P exercise", exerciseP9);
    await runPhase("9P attached stream", exerciseP9AttachedStream);
    await runPhase("9P attached duplex", exerciseP9AttachedDuplex);
    await runPhase("9P attached backpressure", exerciseP9AttachedBackpressure);
    await runPhase("9P attached frame limit", exerciseP9AttachedFrameLimit);
    await runPhase("9P attached write failure", exerciseP9AttachedWriteFailure);
    await runPhase("S3 exercise", exerciseS3);
    await runPhase("WebDAV exercise", exerciseWebdav);
  })(),
  () => `N-API server integration (${phaseSummary()})`,
);

console.log("mount-rs N-API server integration: PASS");
