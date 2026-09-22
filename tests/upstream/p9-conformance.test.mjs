import { spawn, execFileSync } from 'node:child_process';
import { createConnection } from 'node:net';
import { createInterface } from 'node:readline';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { once } from 'node:events';
import { describe, expect, it } from 'vitest';

const root = fileURLToPath(new URL('../..', import.meta.url));
const source = process.env.MOUNTX_SOURCE;
if (!source) throw new Error('MOUNTX_SOURCE is required');
const upstream = (path) => import(pathToFileURL(`${source}/${path}`).href);

const { conformance } = await upstream('test/conformance.ts');
const { createLoopback } = await upstream('src/harness.ts');
const { createMemoryDriver } = await upstream('src/drivers/memory.ts');
const { P9Session } = await upstream('src/9p/session.ts');
const { P9Client, p9Driver } = await upstream('test/9p/client.ts');
const { ERRNO_CODES } = await upstream('src/errors.ts');
const { P9_GETATTR_BASIC, P9_TGETATTR } = await upstream('src/9p/constants.ts');
const { O_CREAT, O_RDONLY, O_RDWR, O_TRUNC, O_WRONLY } = await upstream('src/fuse/constants.ts');

const build = execFileSync(`${root}/scripts/cargo-shared`, [
  'build', '--locked', '--example', 'p9_oracle', '--message-format=json',
], {
  cwd: root,
  stdio: ['ignore', 'pipe', 'inherit'],
  timeout: 180000,
  encoding: 'utf8',
});
const fixture = build.trim().split('\n').map((line) => JSON.parse(line))
  .find((item) => item.reason === 'compiler-artifact' && item.target.name === 'p9_oracle' && item.executable)
  ?.executable;
if (!fixture) throw new Error('Cargo did not return the 9P fixture executable');

// This is the upstream 9P column's full capability claim. The suite itself is
// the authority for the 71 cases; this declaration only says what the wire
// preserves for the driver behind it.
const THROUGH_9P = {
  handles: true,
  atomicRename: true,
  hardlinks: true,
  symlinks: true,
  permissions: true,
  times: true,
  truncate: true,
  caseSensitive: true,
  statfs: true,
  readOnly: false,
  durableWrites: false,
  extensions: ['mknod'],
};

/**
 * One bounded framed TCP transport for the upstream P9Client constructor.
 * Replies are matched by their wire tag, so concurrent calls cannot consume
 * one another's frames. A broken stream resolves pending calls as dropped;
 * it never manufactures an Rlerror for a request the server did not answer.
 */
async function connectP9(port) {
  const maxFrame = 1024 * 1024;
  const socket = createConnection({ host: '127.0.0.1', port });
  await once(socket, 'connect');
  let buffered = Buffer.alloc(0);
  let closed = false;
  const pending = new Map();
  let closeResolve;
  const closedPromise = new Promise((resolve) => { closeResolve = resolve; });

  const dropPending = () => {
    for (const { resolve } of pending.values()) resolve(null);
    pending.clear();
  };

  const close = () => {
    if (closed) return closedPromise;
    closed = true;
    dropPending();
    socket.destroy();
    return closedPromise;
  };

  socket.on('data', (chunk) => {
    if (closed) return;
    buffered = Buffer.concat([buffered, chunk]);
    while (buffered.length >= 4) {
      const size = buffered.readUInt32LE(0);
      if (size < 7 || size > maxFrame) {
        void close();
        return;
      }
      if (buffered.length < size) return;
      const frame = Buffer.from(buffered.subarray(0, size));
      buffered = buffered.subarray(size);
      const tag = frame.readUInt16LE(5);
      const waiter = pending.get(tag);
      if (!waiter) {
        void close();
        return;
      }
      pending.delete(tag);
      waiter.resolve(frame);
    }
  });
  socket.on('error', () => {
    // The upstream client receives null and reports a dropped reply. The
    // transport does not invent a protocol error or an errno on the server's
    // behalf.
    dropPending();
  });
  socket.on('close', () => {
    closed = true;
    dropPending();
    closeResolve();
  });

  const transport = (request) => {
    if (closed) return Promise.resolve(null);
    if (request.byteLength < 7 || request.byteLength > maxFrame) {
      return Promise.reject(new Error('invalid 9P request frame'));
    }
    const frame = Buffer.from(request);
    if (frame.readUInt32LE(0) !== frame.length) {
      return Promise.reject(new Error('9P request length does not match frame'));
    }
    const tag = frame.readUInt16LE(5);
    if (pending.has(tag)) {
      return Promise.reject(new Error(`duplicate pending 9P tag ${tag}`));
    }
    return new Promise((resolve) => {
      pending.set(tag, { resolve });
      socket.write(frame, (error) => {
        if (error) {
          pending.delete(tag);
          resolve(null);
        }
      });
    });
  };
  return { client: new P9Client(transport), close, closed: closedPromise };
}

async function serveRust(args = []) {
  const child = spawn(fixture, args, {
    cwd: root,
    stdio: ['pipe', 'pipe', 'inherit'],
  });
  const exited = once(child, 'exit');
  const lines = createInterface({ input: child.stdout });
  let client;
  let disconnect;
  let connectionClosed;
  let timer;
  let shutdownRequested = false;
  const stop = async () => {
    try {
      await disconnect?.();
    } finally {
      child.stdin.end();
      const kill = setTimeout(() => child.kill('SIGKILL'), 5000);
      try {
        const [code, signal] = await exited;
        if (code !== 0) throw new Error(`9P fixture exit: ${code}/${signal}`);
      } finally {
        clearTimeout(kill);
        lines.close();
      }
    }
  };
  try {
    const port = await Promise.race([
      once(lines, 'line').then(([line]) => Number(line)),
      exited.then(([code]) => { throw new Error(`9P fixture exited early: ${code}`); }),
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error('9P fixture startup timed out')), 10000);
      }),
    ]);
    clearTimeout(timer);
    if (!Number.isInteger(port) || port <= 0 || port > 65535) {
      throw new Error('Invalid 9P port');
    }
    ({ client, close: disconnect, closed: connectionClosed } = await connectP9(port));
    await client.version();
    await client.attach(0);
    return {
      client,
      fs: createLoopback(p9Driver(client, 0)),
      connectionClosed,
      requestShutdown: () => {
        if (shutdownRequested) return Promise.resolve();
        shutdownRequested = true;
        return new Promise((resolve, reject) => {
          child.stdin.write(Buffer.from([1]), (error) => {
            if (error) reject(error);
            else resolve();
          });
        });
      },
      cleanup: stop,
    };
  } catch (error) {
    clearTimeout(timer);
    await stop();
    throw error;
  }
}

conformance({
  name: 'TypeScript 9P session control',
  capabilities: THROUGH_9P,
  setup: async () => {
    const session = new P9Session(createMemoryDriver());
    const client = P9Client.overSession(session);
    await client.version();
    await client.attach(0);
    return {
      fs: createLoopback(p9Driver(client, 0)),
      cleanup: () => session.destroy(),
    };
  },
});

conformance({
  name: 'Rust 9P through upstream TCP client',
  capabilities: THROUGH_9P,
  setup: serveRust,
});

describe('Rust 9P actual-client transport cases', () => {
  it('preserves exact errno replies over the TCP wire', async () => {
    const server = await serveRust();
    try {
      await expect(server.client.walk(0, 100, ['missing'])).rejects.toMatchObject({
        code: 'ENOENT',
      });
      expect(
        await server.client.expectError(P9_TGETATTR, (writer) => {
          writer.u32(0);
        }),
      ).toBe(ERRNO_CODES.EINVAL);
      expect(
        await server.client.expectError(200, (writer) => {
          writer.u32(0);
        }),
      ).toBe(ERRNO_CODES.ENOTSUP);
    } finally {
      await server.cleanup();
    }
  });

  it('keeps mixed concurrent actual-client calls paired by wire tag', async () => {
    const server = await serveRust();
    try {
      await server.client.lopen(0, O_RDONLY);
      const attrCalls = Array.from({ length: 24 }, () =>
        server.client.getattr(0, P9_GETATTR_BASIC),
      );
      const statfsCalls = Array.from({ length: 8 }, () => server.client.statfs(0));
      const readdirCalls = Array.from({ length: 8 }, () => server.client.readdir(0, 0n));
      const [attrs, statfs, directories] = await Promise.all([
        Promise.all(attrCalls),
        Promise.all(statfsCalls),
        Promise.all(readdirCalls),
      ]);

      expect(attrs).toHaveLength(24);
      expect(attrs.every((attr) => (attr.valid & P9_GETATTR_BASIC) === P9_GETATTR_BASIC)).toBe(
        true,
      );
      expect(statfs.every((stats) => stats.bsize > 0)).toBe(true);
      expect(directories.every((entries) => Array.isArray(entries))).toBe(true);
    } finally {
      await server.cleanup();
    }
  });

  it('closes an active actual TCP client when the Rust server shuts down', async () => {
    const server = await serveRust();
    try {
      await server.requestShutdown();
      await expect(server.connectionClosed).resolves.toBeUndefined();
      await expect(server.client.getattr(0)).rejects.toThrow(/dropped without a reply/);
    } finally {
      await server.cleanup();
    }
  });

  it('allows read-only reads while rejecting every write intent', async () => {
    const server = await serveRust(['--read-only']);
    try {
      const writeIntents = [O_WRONLY, O_RDWR, O_RDONLY | O_TRUNC, O_RDONLY | O_CREAT];
      for (const [index, flags] of writeIntents.entries()) {
        const fid = 100 + index;
        await server.client.walk(0, fid, ['read-only.txt']);
        await expect(server.client.lopen(fid, flags)).rejects.toMatchObject({ code: 'EROFS' });
      }

      const readable = 200;
      await server.client.walk(0, readable, ['read-only.txt']);
      await server.client.lopen(readable, O_RDONLY);
      expect(new TextDecoder().decode(await server.client.read(readable, 0n))).toBe(
        'read-only over 9P',
      );
      await expect(server.client.write(readable, 0n, 'x')).rejects.toMatchObject({
        code: 'EROFS',
      });
    } finally {
      await server.cleanup();
    }
  });
});
