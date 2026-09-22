import assert from "node:assert/strict";
import { chmod, mkdtemp, rm, stat } from "node:fs/promises";
import * as net from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Duplex } from "node:stream";
import { pathToFileURL } from "node:url";

import {
  Filesystem,
  NfsConnection,
  Nfs3Session,
  Nfs4Session,
  NfsSession,
  S3Session,
  createNfsServer,
  createP9Server,
  createS3Server,
  createWebdavServer,
  WebdavSession,
} from "../index.js";
import * as nfs from "../nfs.cjs";

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

const IO_TIMEOUT_MS = 20_000;
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

function markPhase(label) {
  activePhase = { label, startedAt: Date.now() };
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

async function connectUnix(path) {
  const socket = net.createConnection({ path });
  await within(
    new Promise((resolve, reject) => {
      socket.once("connect", resolve);
      socket.once("error", reject);
    }),
    `connect to ${path}`,
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

function nfsV4Compound(tag, operations) {
  return nfs.encodeXdr((writer) => {
    writer.string(tag);
    writer.u32(1);
    writer.u32(operations.length);
    for (const [opcode, body] of operations) {
      writer.u32(opcode);
      writer.raw(body);
    }
  });
}

function nfsV4Operation(write) {
  return nfs.encodeXdr(write);
}

function nfsV4Call(xid, tag, operations) {
  return nfs.encodeCall({
    xid,
    program: 100_003,
    version: 4,
    procedure: 1,
    cred: nfs.AUTH_NULL,
    verf: nfs.AUTH_NULL,
    args: nfsV4Compound(tag, operations),
  });
}

function nativeErrorMessage(error) {
  const message = String(error?.message ?? error);
  const fields = message.split("|");
  if (fields[0] === "__mount_rs_error_v1__" && fields.length === 7) {
    return Buffer.from(fields[6], "hex").toString("utf8");
  }
  return message;
}

function decodeNfsV4Compound(reply, label) {
  const decoded = nfs.decodeReply(reply);
  assert.equal(decoded.reply.acceptStat, nfs.RPC_SUCCESS, `${label} RPC status`);
  const status = decoded.results.u32(`${label} compound status`);
  const tag = decoded.results.string(undefined, `${label} tag`);
  const count = decoded.results.u32(`${label} result count`);
  return { reader: decoded.results, status, tag, count };
}

function readNfsV4Sequence(reader, label) {
  assert.equal(reader.u32(`${label} operation`), 53);
  assert.equal(reader.u32(`${label} status`), 0);
  reader.fixedOpaque(16, `${label} session id`);
  reader.u32(`${label} sequence`);
  reader.u32(`${label} slot`);
  reader.u32(`${label} highest slot`);
  reader.u32(`${label} target highest slot`);
  reader.u32(`${label} status flags`);
}

function nfsRecord(record) {
  const marker = Buffer.alloc(4);
  marker.writeUInt32BE((0x8000_0000 | record.length) >>> 0);
  return Buffer.concat([marker, record]);
}

async function exerciseNfs() {
  const filesystem = memoryFilesystem();
  const reports = [];
  const sessionErrors = [];
  const nameCalls = [];
  const idCalls = [];
  const clockCalls = [];
  const server = createNfsServer(filesystem, {
    host: "127.0.0.1",
    port: 0,
    maxRecord: 256,
    maxHandles: 2,
    nfs4: {
      idmap: {
        domain: "example.test",
        users: { root: 0 },
        groups: { root: 0 },
        nameOf(id, group) {
          nameCalls.push({ id, group });
          return `${group ? "group" : "user"}-${id}`;
        },
        idOf(name, group) {
          idCalls.push({ name, group });
          if (!group && name === "named-user") return 0;
          if (group && name === "named-group") return 0;
          return undefined;
        },
      },
      now() {
        clockCalls.push(Date.now());
        return 1_000 + clockCalls.length;
      },
      seed: 0x10203040,
      leaseSeconds: 7,
      maxSessions: 1,
      maxForeSlots: 1,
      maxOperations: 3,
      maxRequestSize: 4096,
      maxCachedResponseSize: 32,
      maxOpensPerFile: 1,
      maxLocksPerFile: 1,
      requireReclaimComplete: true,
    },
    onTransportError(error, peer) {
      reports.push({ error, peer });
      throw new Error("NFS hook callback deliberately threw");
    },
    onError(error, call) {
      sessionErrors.push({ error, call });
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
    assert.ok(server.session.v3 instanceof Nfs3Session);
    assert.ok(server.session.v4 instanceof Nfs4Session);
    assert.ok(server.session.driver instanceof Filesystem);
    assert.ok(server.session.v3.driver instanceof Filesystem);
    assert.ok(server.session.v4.driver instanceof Filesystem);
    assert.equal(server.session.options.maxHandles, 2);
    assert.equal(server.session.options.nfs4.leaseSeconds, 7);
    assert.equal(server.session.options.nfs4.idmapConfigured, true);
    assert.deepEqual(
      [...server.session.writeVerifier],
      [...server.session.v3.writeVerifier],
    );
    assert.deepEqual(
      [...server.session.writeVerifier],
      [...server.session.v4.writeVerifier],
    );
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

    const directV3Reply = await server.session.v3.handleCall(nfsNullCall(41));
    assert.ok(Buffer.isBuffer(directV3Reply));
    assert.equal(directV3Reply.readUInt32BE(0), 41);
    assert.equal(directV3Reply.readUInt32BE(20), 0);
    assert.equal(server.session.stats.requests, 2);

    const directV4Reply = await server.session.handleCall(nfsV4NullCall(44));
    assert.ok(Buffer.isBuffer(directV4Reply));
    assert.equal(directV4Reply.readUInt32BE(0), 44);
    assert.equal(directV4Reply.readUInt32BE(4), 1);
    assert.equal(directV4Reply.readUInt32BE(20), 0);
    assert.equal(server.session.v4.destroyed, false);
    assert.equal(server.session.stats.requests, 3);
    assert.equal(server.session.stats.procedures["NFS4:NULL"], 1);
    assert.equal(server.session.v4.stats.requests, 3);
    assert.equal(await server.session.v4.sweepExpired(), 0);
    assert.ok(clockCalls.length > 0, "NFSv4 uses the injected JavaScript clock");

    // Exercise the callback-backed NFSv4 state path through the actual N-API
    // server. The synchronous JS callbacks are invoked from the Rust async
    // worker and their translated owner strings are returned on the wire.
    const exchangeReply = await server.session.v4.handleCall(
      nfsV4Call(46, "exchange", [[42, nfsV4Operation((writer) => {
        writer.fixedOpaque(Buffer.from("nfs-vrfr"), 8);
        writer.varOpaque(Buffer.from("nfs-napi-callback"));
        writer.u32(0);
        writer.u32(0);
        writer.u32(0);
      })]]),
    );
    const exchange = decodeNfsV4Compound(exchangeReply, "EXCHANGE_ID");
    assert.equal(exchange.status, 0);
    assert.equal(exchange.tag, "exchange");
    assert.equal(exchange.count, 1);
    assert.equal(exchange.reader.u32("EXCHANGE_ID operation"), 42);
    assert.equal(exchange.reader.u32("EXCHANGE_ID status"), 0);
    const clientId = exchange.reader.u64("EXCHANGE_ID clientid");
    exchange.reader.u32("EXCHANGE_ID sequence");
    exchange.reader.u32("EXCHANGE_ID flags");
    exchange.reader.u32("EXCHANGE_ID state protection");
    exchange.reader.u64("EXCHANGE_ID server owner minor");
    exchange.reader.varOpaque(undefined, "EXCHANGE_ID server owner major");
    exchange.reader.varOpaque(undefined, "EXCHANGE_ID server scope");
    assert.equal(exchange.reader.u32("EXCHANGE_ID implementation count"), 0);
    exchange.reader.end("EXCHANGE_ID reply");

    const channelAttrs = (writer) => {
      writer.u32(0);
      writer.u32(1 << 20);
      writer.u32(1 << 20);
      writer.u32(1 << 20);
      writer.u32(8);
      writer.u32(1);
      writer.u32(0);
    };
    const createSessionReply = await server.session.v4.handleCall(
      nfsV4Call(47, "create", [[43, nfsV4Operation((writer) => {
        writer.u64(clientId);
        writer.u32(1);
        writer.u32(0);
        channelAttrs(writer);
        channelAttrs(writer);
        writer.u32(0);
        writer.u32(1);
        writer.u32(nfs.AUTH_NONE);
      })]]),
    );
    const createSession = decodeNfsV4Compound(createSessionReply, "CREATE_SESSION");
    assert.equal(createSession.status, 0);
    assert.equal(createSession.tag, "create");
    assert.equal(createSession.count, 1);
    assert.equal(createSession.reader.u32("CREATE_SESSION operation"), 43);
    assert.equal(createSession.reader.u32("CREATE_SESSION status"), 0);
    const sessionId = createSession.reader.fixedOpaque(16, "CREATE_SESSION session id");
    createSession.reader.u32("CREATE_SESSION sequence");
    createSession.reader.u32("CREATE_SESSION flags");
    for (let channel = 0; channel < 2; channel += 1) {
      for (let field = 0; field < 6; field += 1) createSession.reader.u32("channel attribute");
      assert.equal(createSession.reader.u32("channel RDMA count"), 0);
    }
    createSession.reader.end("CREATE_SESSION reply");

    const sequence = (sequenceNumber) => nfsV4Operation((writer) => {
      writer.fixedOpaque(sessionId, 16);
      writer.u32(sequenceNumber);
      writer.u32(0);
      writer.u32(0);
      writer.bool(false);
    });
    const putRoot = nfsV4Operation(() => {});
    const requestedOwnerBitmap = nfsV4Operation((writer) => {
      writer.u32(2);
      writer.u32(0);
      writer.u32((1 << 4) | (1 << 5));
    });
    const getattrReply = await server.session.v4.handleCall(
      nfsV4Call(48, "getattr", [
        [53, sequence(1)],
        [24, putRoot],
        [9, requestedOwnerBitmap],
      ]),
    );
    const getattrResult = decodeNfsV4Compound(getattrReply, "GETATTR");
    assert.equal(getattrResult.status, 0);
    assert.equal(getattrResult.tag, "getattr");
    assert.equal(getattrResult.count, 3);
    readNfsV4Sequence(getattrResult.reader, "GETATTR SEQUENCE");
    assert.equal(getattrResult.reader.u32("GETATTR PUTROOTFH operation"), 24);
    assert.equal(getattrResult.reader.u32("GETATTR PUTROOTFH status"), 0);
    assert.equal(getattrResult.reader.u32("GETATTR operation"), 9);
    assert.equal(getattrResult.reader.u32("GETATTR status"), 0);
    assert.equal(getattrResult.reader.u32("GETATTR bitmap words"), 2);
    assert.equal(getattrResult.reader.u32("GETATTR owner bitmap"), 0);
    assert.equal(getattrResult.reader.u32("GETATTR group bitmap"), (1 << 4) | (1 << 5));
    const ownerAttrs = getattrResult.reader.varOpaque(undefined, "GETATTR attributes");
    const decodedOwnerAttrs = nfs.decodeXdr(ownerAttrs, (reader) => [
      reader.string(),
      reader.string(),
    ]);
    const userCall = nameCalls.find(({ group }) => !group);
    const groupCall = nameCalls.find(({ group }) => group);
    assert.ok(userCall, "owner callback was invoked");
    assert.ok(groupCall, "owner-group callback was invoked");
    assert.deepEqual(decodedOwnerAttrs, [
      `user-${userCall.id}@example.test`,
      `group-${groupCall.id}@example.test`,
    ]);
    getattrResult.reader.end("GETATTR reply");

    const setattrAttrs = nfs.encodeXdr((writer) => {
      writer.string("named-user@example.test");
      writer.string("named-group@example.test");
    });
    const setattr = nfsV4Operation((writer) => {
      writer.u32(0);
      writer.fixedOpaque(Buffer.alloc(12), 12);
      writer.u32(2);
      writer.u32(0);
      writer.u32((1 << 4) | (1 << 5));
      writer.varOpaque(setattrAttrs);
    });
    const setattrReply = await server.session.v4.handleCall(
      nfsV4Call(49, "setattr", [[53, sequence(2)], [24, putRoot], [34, setattr]]),
    );
    const setattrResult = decodeNfsV4Compound(setattrReply, "SETATTR");
    assert.equal(setattrResult.status, 0);
    assert.equal(setattrResult.tag, "setattr");
    assert.equal(setattrResult.count, 3);
    readNfsV4Sequence(setattrResult.reader, "SETATTR SEQUENCE");
    assert.equal(setattrResult.reader.u32("SETATTR PUTROOTFH operation"), 24);
    assert.equal(setattrResult.reader.u32("SETATTR PUTROOTFH status"), 0);
    assert.equal(setattrResult.reader.u32("SETATTR operation"), 34);
    assert.equal(setattrResult.reader.u32("SETATTR status"), 0);
    assert.equal(setattrResult.reader.u32("SETATTR applied bitmap words"), 2);
    assert.equal(setattrResult.reader.u32("SETATTR applied owner"), 0);
    assert.equal(setattrResult.reader.u32("SETATTR applied group"), (1 << 4) | (1 << 5));
    setattrResult.reader.end("SETATTR reply");
    assert.deepEqual(idCalls, [
      { name: "named-user", group: false },
      { name: "named-group", group: true },
    ]);
    const malformedV4 = nfsV4NullCall(45);
    malformedV4.writeUInt32BE(1, 20);
    const malformedV4Reply = await server.session.v4.handleCall(malformedV4);
    assert.ok(Buffer.isBuffer(malformedV4Reply));
    assert.equal(malformedV4Reply.readUInt32BE(20), 4, "malformed COMPOUND is RPC garbage args");
    await waitUntil(() => sessionErrors.length === 1, "NFS session error callback");
    assert.ok(sessionErrors[0].error instanceof Error);
    assert.match(sessionErrors[0].error.message, /COMPOUND|truncated|byte/i);
    assert.equal(sessionErrors[0].call.xid, 45);
    const rootHandle = [{ id: 1n, fileid: 1n, key: "0:1", path: "/" }];
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
    assert.ok(nfsConnection.session.v3 instanceof Nfs3Session);
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

async function exerciseNfsSessionDestroy() {
  const server = createNfsServer(memoryFilesystem());
  try {
    assert.equal(server.session.destroyed, false);
    assert.equal(server.session.v4.destroyed, false);
    await server.session.v4.destroy();
    assert.equal(server.session.destroyed, false);
    assert.equal(server.session.v4.destroyed, true);
    await server.session.destroy();
    assert.equal(server.session.destroyed, true);
    assert.equal(server.session.v4.destroyed, true);
  } finally {
    await server.close();
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
    assert.equal(server.options.host, "127.0.0.1");
    assert.equal(server.options.port, 0);
    assert.equal(server.options.maxFrame, 1024 * 1024);
    assert.equal(server.options.maxInFlight, 16);
    assert.equal(server.options.useDriverIno, true);
    assert.equal(server.options.readOnly, false);
    assert.equal(server.options.claimOwnership, true);
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

    [connection] = server.clients;
    assert.ok(connection);
    assert.equal(connection.stream, undefined);
    const session = connection.session;
    assert.strictEqual(connection.session, session);
    assert.equal(session.options.msize, undefined);
    assert.equal(session.options.useDriverIno, true);
    assert.equal(session.options.readOnly, false);
    assert.equal(session.options.claimOwnership, true);
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
    assert.deepEqual(session.userFor(1), {
      uname: "node",
      uid: undefined,
      aname: "",
    });
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
    [connection] = server.clients;
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

async function exerciseP9Unix() {
  if (process.platform === "win32") return;

  const directory = await mkdtemp(join(tmpdir(), "mount-rs-napi-9p-unix-"));
  await chmod(directory, 0o700);
  try {
    const path = join(directory, "9p.sock");
    const server = createP9Server(memoryFilesystem(), { path, socketMode: 0o600 });
    let socket;
    let serverReader;
    try {
      assert.equal(server.path, path);
      assert.equal(server.address(), path);
      await within(server.listen(), "9P Unix listen");
      assert.equal(server.path, path);
      assert.equal(server.address(), path);
      assert.equal((await stat(path)).mode & 0o777, 0o600);

      ({ socket, reader: serverReader } = await connectUnix(path));
      await p9Request(
        socket,
        serverReader,
        100,
        0xffff,
        Buffer.concat([Buffer.from([0x00, 0x00, 0x01, 0x00]), p9String("9P2000.L")]),
        101,
      );
      const [connection] = server.clients;
      assert.ok(connection);
      assert.equal(connection.stream, undefined);
      assert.equal(connection.peer, path);
    } finally {
      if (socket) await closeSocket(socket, "9P Unix socket close");
      await within(server.close(), "9P Unix server close");
    }
    await assert.rejects(stat(path), (error) => error?.code === "ENOENT");

    await chmod(directory, 0o755);
    const refusedPath = join(directory, "refused.sock");
    const refused = createP9Server(memoryFilesystem(), { path: refusedPath });
    await assert.rejects(
      refused.listen(),
      (error) => /directory must be uid .* mode 0700/.test(nativeErrorMessage(error)),
    );
    await refused.close();

    const sharedPath = join(directory, "shared.sock");
    const shared = createP9Server(memoryFilesystem(), {
      path: sharedPath,
      allowSharedDirectory: true,
    });
    let sharedSocket;
    let sharedReader;
    try {
      await within(shared.listen(), "9P shared-directory Unix listen");
      assert.equal((await stat(sharedPath)).mode & 0o777, 0o600);
      ({ socket: sharedSocket, reader: sharedReader } = await connectUnix(sharedPath));
      await p9Request(
        sharedSocket,
        sharedReader,
        100,
        0xffff,
        Buffer.concat([Buffer.from([0x00, 0x00, 0x01, 0x00]), p9String("9P2000.L")]),
        101,
      );
    } finally {
      if (sharedSocket) await closeSocket(sharedSocket, "9P shared Unix socket close");
      await within(shared.close(), "9P shared-directory Unix server close");
    }
    await assert.rejects(stat(sharedPath), (error) => error?.code === "ENOENT");

    assert.throws(
      () => createP9Server(memoryFilesystem(), { path: join(directory, "both.sock"), port: 0 }),
      (error) => /either path or host\/port, not both/.test(nativeErrorMessage(error)),
    );
  } finally {
    await rm(directory, { recursive: true, force: true });
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
  assert.strictEqual(server.clients[0], connection);
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

function xmlField(body, name) {
  const match = Buffer.from(body).toString().match(new RegExp(`<${name}>([^<]*)</${name}>`));
  assert.ok(match, `missing S3 XML field ${name}`);
  return match[1];
}

function bufferedS3(session, method, target, body = Buffer.alloc(0)) {
  const headers = body.length === 0
    ? []
    : [{ name: "content-length", value: String(body.length) }];
  return session.handleRequest({ method, target, headers }, body);
}

async function exerciseS3() {
  const photos = memoryFilesystem();
  const notes = memoryFilesystem();
  const reports = [];
  const server = createS3Server({ buckets: { photos, notes } }, {
    bucket: "mountx",
    host: "127.0.0.1",
    port: 0,
    drainTimeout: 1000,
    debug: true,
    onTransportError(error, peer) {
      reports.push({ error, peer });
      throw new Error("S3 hook callback deliberately threw");
    },
  });
  let listening;
  let idleSocket;
  let faultSocket;
  try {
    markPhase("S3 listener setup");
    assert.equal(server.port, 0);
    listening = (await listenLifecycle(server, "S3")).listening;
    assert.ok(server.port > 0);
    assert.equal(server.connections, 0);
    assert.equal(server.url.endsWith("/"), false);
    assert.deepEqual(server.buckets, ["notes", "photos"]);
    markPhase("S3 idle connection setup");
    idleSocket = (await connectLoopback(server.port)).socket;
    await waitUntil(() => server.connections >= 1, "S3 accepted connection count");
    await closeSocket(idleSocket, "S3 idle connection close");
    idleSocket = undefined;
    await waitUntil(() => server.connections === 0, "S3 disconnected connection count");
    markPhase("S3 session inspection");
    assert.ok(server.session instanceof S3Session);
    assert.deepEqual(server.session.bucketNames, ["notes", "photos"]);
    assert.deepEqual(Object.keys(server.session.buckets).sort(), ["notes", "photos"]);
    assert.ok(server.session.buckets.photos instanceof Filesystem);
    assert.ok(server.session.buckets.notes instanceof Filesystem);
    assert.equal(server.session.options.credentialsConfigured, false);
    assert.equal(server.session.options.region, undefined);
    assert.equal(server.session.options.maxBodyBytes, 512 * 1024 * 1024);
    assert.equal(server.session.options.maxXmlBytes, 16 * 1024 * 1024);
    assert.equal(server.session.options.readChunkBytes, 128 * 1024);
    assert.equal(server.session.options.multipartStagingTtlMs, 24 * 60 * 60 * 1000);
    assert.equal(server.session.options.multipartStagingMaxBytes, 8 * 1024 * 1024 * 1024);
    assert.equal(server.session.options.debug, true);
    assert.deepEqual(await server.session.stats(), {
      requests: 0,
      replies: 0,
      errors: 0,
      operations: {},
      assertions: 0,
      durationMsTotal: 0,
      durationMsMax: 0,
      requestBytes: 0,
      responseBytes: 0,
      errorClasses: {},
    });
    assert.deepEqual(server.session.assertions, []);

    const object = Buffer.from("S3 over the real loopback HTTP listener");
    markPhase("S3 buffered PUT");
    const put = await fetchBody(
      `${server.url}/photos/servers-s3.txt`,
      { method: "PUT", body: object },
      "S3 PUT",
    );
    assert.ok(put.response.status >= 200 && put.response.status < 300);

    markPhase("S3 buffered GET");
    const get = await fetchBody(
      `${server.url}/photos/servers-s3.txt`,
      { method: "GET" },
      "S3 GET",
    );
    assert.equal(get.response.status, 200);
    assert.deepEqual(get.body, object);

    markPhase("S3 bucket isolation");
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
    markPhase("S3 streamed PUT");
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

    markPhase("S3 multipart initiation");
    const initiated = await bufferedS3(server.session, "POST", "/photos/restarted-s3.bin?uploads");
    assert.equal(initiated.status, 200);
    const uploadId = xmlField(initiated.body, "UploadId");
    markPhase("S3 multipart part upload");
    const part = await bufferedS3(
      server.session,
      "PUT",
      `/photos/restarted-s3.bin?uploadId=${uploadId}&partNumber=1`,
      Buffer.from("N-API replacement-session bytes"),
    );
    assert.equal(part.status, 200);
    const partEtag = part.headers.find(({ name }) => name === "etag")?.value;
    assert.ok(partEtag, "replacement-session part ETag");

    // Rebuild the server facade over the same native Filesystem. Multipart
    // state is represented by the driver tree, not a session-local registry.
    markPhase("S3 replacement server setup");
    const replacement = createS3Server({ buckets: { photos } }, { debug: true });
    try {
      markPhase("S3 multipart listing after replacement");
      const listed = await bufferedS3(
        replacement.session,
        "GET",
        `/photos/restarted-s3.bin?uploadId=${uploadId}`,
      );
      assert.equal(listed.status, 200);
      assert.match(Buffer.from(listed.body).toString(), /<PartNumber>1<\/PartNumber>/);
      markPhase("S3 multipart completion after replacement");
      const completed = await bufferedS3(
        replacement.session,
        "POST",
        `/photos/restarted-s3.bin?uploadId=${uploadId}`,
        Buffer.from(
          `<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>${partEtag}</ETag></Part></CompleteMultipartUpload>`,
        ),
      );
      assert.equal(completed.status, 200);
      markPhase("S3 object read after replacement");
      const restartedObject = await bufferedS3(
        replacement.session,
        "GET",
        "/photos/restarted-s3.bin",
      );
      assert.equal(restartedObject.status, 200);
      assert.deepEqual(restartedObject.body, Buffer.from("N-API replacement-session bytes"));
    } finally {
      await replacement.close();
    }

    const emptyRequest = () => new ReadableStream({
      start(controller) {
        controller.close();
      },
    });
    markPhase("S3 streamed GET");
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

    markPhase("S3 streamed cancellation setup");
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
    markPhase("S3 streamed request failure");
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

    markPhase("S3 streamed stats");
    const streamedStats = await server.session.stats();
    assert.equal(streamedStats.requests, beforeStreamStats.requests + 6);
    assert.equal(streamedStats.replies, beforeStreamStats.replies + 6);
    assert.equal(streamedStats.errors, beforeStreamStats.errors + 1);
    assert.equal(
      streamedStats.operations.PutObject,
      (beforeStreamStats.operations.PutObject ?? 0) + 2,
    );
    assert.equal(
      streamedStats.operations.GetObject,
      (beforeStreamStats.operations.GetObject ?? 0) + 2,
    );
    assert.equal(streamedStats.assertions, 0);
    assert.deepEqual(server.session.assertions, []);
    assert.deepEqual(reports, []);

    markPhase("S3 peer-fault partial request");
    faultSocket = (await connectLoopback(server.port)).socket;
    await writeSocket(
      faultSocket,
      Buffer.from(`GET /photos/peer-fault-s3.txt HTTP/1.1\r\nHost: `),
      "S3 JavaScript peer-fault request",
    );
    markPhase("S3 peer-fault reset");
    faultSocket.resetAndDestroy();
    markPhase("S3 peer-fault callback");
    const faultReport = await waitForTransportError(reports, "S3 JavaScript peer fault");
    assert.ok(faultReport.error instanceof Error);
    assert.match(faultReport.peer, /^127\.0\.0\.1:\d+$/);
    assert.equal(reports.length, 1);
    faultSocket = undefined;
    markPhase("S3 peer-fault connection cleanup");
    await waitUntil(() => server.connections === 0, "S3 peer-fault connection cleanup");
  } finally {
    if (idleSocket) {
      await runPhase("S3 cleanup: idle socket close", () =>
        closeSocket(idleSocket, "S3 idle socket close"),
      );
    }
    if (faultSocket) {
      markPhase("S3 cleanup fault socket");
      await runPhase("S3 cleanup: fault socket close", () =>
        closeSocket(faultSocket, "S3 fault socket close"),
      );
    }
    markPhase("S3 cleanup server lifecycle");
    await runPhase("S3 cleanup: server lifecycle", () =>
      closeLifecycle(server, "S3", listening),
    );
    assert.equal(server.connections, 0);
    assert.equal(reports.length, 1);
  }
}

async function exerciseWebdav() {
  const filesystem = memoryFilesystem();
  const reports = [];
  const sessionErrors = [];
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
    onError(error, head) {
      sessionErrors.push({ error, head });
    },
  });
  let listening;
  let faultSocket;
  let exerciseFailed = false;
  try {
    listening = (await listenLifecycle(server, "WebDAV")).listening;
    assert.ok(server.port > 0);
    assert.equal(server.url.endsWith("/"), false);
    assert.ok(server.session instanceof WebdavSession);
    assert.ok(server.session.driver instanceof Filesystem);
    assert.deepEqual(server.session.assertions, []);
    assert.ok(server.session.stats.methods instanceof Map);
    assert.equal(server.session.stats.methods.size, 0);
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
    assert.equal(server.session.stats.methods.get("PUT"), 1);

    const lock = await server.session.handleRequest(
      {
        method: "LOCK",
        target: "/direct-webdav.txt",
        headers: [{ name: "depth", value: "0" }],
      },
      Buffer.from('<D:lockinfo xmlns:D="DAV:"><D:lockscope><D:exclusive/></D:lockscope><D:locktype><D:write/></D:locktype><D:owner><Z:name xmlns:Z="urn:test">A&amp;B</Z:name></D:owner></D:lockinfo>'),
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
    assert.deepEqual(server.session.locks[0].owner, {
      name: "owner",
      ns: "DAV:",
      text: "",
      children: [{ name: "name", ns: "urn:test", text: "A&B", children: [] }],
    });
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
    if (process.env.MOUNT_RS_STRUCTURAL_SERVERS === "1") {
      // The pinned JavaScript FsDriver contract has only unbounded readdir;
      // the native transport must refuse remote directory materialization
      // rather than silently turning that callback into an unbounded scan.
      assert.equal(propfind.status, 501);
    } else {
      assert.equal(propfind.status, 207);
      assert.match(propfind.body.toString("utf8"), /<multistatus xmlns="DAV:">/);
    }
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
    await new Promise((resolve) => setImmediate(resolve));
    const sessionError = sessionErrors.at(-1);
    assert.ok(sessionError);
    assert.ok(sessionError.error instanceof Error);
    assert.equal(sessionError.head.method, "PATCH");
    assert.equal(sessionError.head.target, "/direct-methods/moved.txt");
    const remove = await directRequest("DELETE", "/direct-methods/moved.txt");
    assert.equal(remove.status, 204);

    const concurrentCollection = await directRequest("MKCOL", "/concurrent");
    assert.ok([201, 204].includes(concurrentCollection.status));
    const concurrentObjects = Array.from({ length: 8 }, (_, index) =>
      Buffer.from(`concurrent WebDAV object ${index}`),
    );
    const concurrentPutReplies = await Promise.all(
      concurrentObjects.map((value, index) =>
        directRequest("PUT", `/concurrent/${index}.txt`, [], value),
      ),
    );
    assert.ok(concurrentPutReplies.every((reply) => [200, 201, 204].includes(reply.status)));
    const concurrentGetReplies = await Promise.all(
      concurrentObjects.map(async (value, index) => {
        const reply = await directRequest("GET", `/concurrent/${index}.txt`);
        return { body: reply.body, status: reply.status, value };
      }),
    );
    for (const reply of concurrentGetReplies) {
      assert.equal(reply.status, 200);
      assert.deepEqual(reply.body, reply.value);
    }

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
    assert.deepEqual(
      Buffer.from(await filesystem.readFile("/streamed-failure.txt")),
      Buffer.from("partial"),
      "WebDAV PUT preserves the written prefix when its request body fails",
    );

    const faultReportCount = reports.length;
    faultSocket = (await connectLoopback(server.port)).socket;
    await writeSocket(
      faultSocket,
      Buffer.from("GET /peer-fault-webdav.txt HTTP/1.1\r\nHost: "),
      "WebDAV JavaScript peer-fault request",
    );
    faultSocket.resetAndDestroy();
    const faultReport = await waitForTransportError(
      reports,
      "WebDAV JavaScript peer fault",
    );
    assert.ok(faultReport.error instanceof Error);
    assert.match(faultReport.peer, /^127\.0\.0\.1:\d+$/);
    assert.equal(reports.length, faultReportCount + 1);
    faultSocket = undefined;

    const malformedReports = [];
    const malformedServer = createWebdavServer(filesystem, {
      host: "127.0.0.1",
      port: 0,
      onTransportError(error, peer) {
        malformedReports.push({ error, peer });
      },
    });
    let malformedListening;
    let malformedSocket;
    try {
      malformedListening = (await listenLifecycle(malformedServer, "WebDAV malformed")).listening;
      malformedSocket = (await connectLoopback(malformedServer.port)).socket;
      await writeSocket(
        malformedSocket,
        Buffer.from("not a valid HTTP request\r\n\r\n"),
        "WebDAV malformed HTTP request",
      );
      const malformedReport = await waitForTransportError(
        malformedReports,
        "WebDAV malformed HTTP",
      );
      assert.ok(malformedReport.error instanceof Error);
      assert.match(malformedReport.peer, /^127\.0\.0\.1:\d+$/);
      assert.equal(malformedReports.length, 1);
    } finally {
      if (malformedSocket) {
        await runPhase("WebDAV cleanup: malformed socket close", () =>
          closeSocket(malformedSocket, "WebDAV malformed socket close"),
        );
      }
      await runPhase("WebDAV cleanup: malformed server lifecycle", () =>
        closeLifecycle(malformedServer, "WebDAV malformed", malformedListening),
      );
    }

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

    const restartServer = createWebdavServer(filesystem, {
      host: "127.0.0.1",
      port: 0,
      readChunkBytes: 4 * 1024,
    });
    let restartListening;
    try {
      restartListening = (await listenLifecycle(restartServer, "WebDAV restart seed")).listening;
      const restartObject = Buffer.from("survives same-driver WebDAV server recreation");
      const restartPut = await restartServer.session.handleRequest(
        { method: "PUT", target: "/restart-durable.txt", headers: [] },
        restartObject,
      );
      assert.ok([200, 201, 204].includes(restartPut.status));
      const restartLock = await restartServer.session.handleRequest(
        {
          method: "LOCK",
          target: "/restart-lock.txt",
          headers: [{ name: "depth", value: "0" }],
        },
        Buffer.from('<D:lockinfo xmlns:D="DAV:"><D:lockscope><D:exclusive/></D:lockscope><D:locktype><D:write/></D:locktype></D:lockinfo>'),
      );
      assert.ok([200, 201].includes(restartLock.status));
      assert.equal(restartServer.session.locks.length, 1);
    } finally {
      await closeLifecycle(restartServer, "WebDAV restart seed", restartListening);
    }

    const replacementServer = createWebdavServer(filesystem, {
      host: "127.0.0.1",
      port: 0,
      readChunkBytes: 4 * 1024,
    });
    let replacementListening;
    try {
      replacementListening = (await listenLifecycle(replacementServer, "WebDAV restart replacement")).listening;
      assert.equal(replacementServer.session.locks.length, 0);
      const replacementGet = await replacementServer.session.handleRequest(
        { method: "GET", target: "/restart-durable.txt", headers: [] },
        null,
      );
      assert.equal(replacementGet.status, 200);
      assert.deepEqual(
        replacementGet.body,
        Buffer.from("survives same-driver WebDAV server recreation"),
      );
    } finally {
      await closeLifecycle(replacementServer, "WebDAV restart replacement", replacementListening);
    }
  } catch (error) {
    exerciseFailed = true;
    throw error;
  } finally {
    if (faultSocket) {
      await runPhase("WebDAV cleanup: fault socket close", () =>
        closeSocket(faultSocket, "WebDAV fault socket close"),
      );
    }
    await runPhase("WebDAV cleanup: server lifecycle", () =>
      closeLifecycle(server, "WebDAV", listening),
    );
    if (!exerciseFailed) assert.equal(reports.length, 1);
  }
}

const requestedServerPhase = process.env.MOUNT_RS_SERVER_PHASE

await within(
  (async () => {
    if (requestedServerPhase === "p9") {
      await runPhase("9P exercise", exerciseP9);
      await runPhase("9P Unix listener policy", exerciseP9Unix);
      await runPhase("9P attached stream", exerciseP9AttachedStream);
      await runPhase("9P attached duplex", exerciseP9AttachedDuplex);
      await runPhase("9P attached backpressure", exerciseP9AttachedBackpressure);
      await runPhase("9P attached frame limit", exerciseP9AttachedFrameLimit);
      await runPhase("9P attached write failure", exerciseP9AttachedWriteFailure);
      return;
    }
    if (requestedServerPhase === "webdav") {
      await runPhase("WebDAV exercise", exerciseWebdav)
      return
    }
    if (requestedServerPhase !== undefined) {
      throw new Error(`unknown server phase: ${requestedServerPhase}`)
    }
    await runPhase("NFS exercise", exerciseNfs);
    await runPhase("NFS session destroy", exerciseNfsSessionDestroy);
    await runPhase("9P exercise", exerciseP9);
    await runPhase("9P Unix listener policy", exerciseP9Unix);
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
