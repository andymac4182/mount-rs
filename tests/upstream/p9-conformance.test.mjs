import { spawn, execFileSync } from 'node:child_process';
import { createConnection } from 'node:net';
import { createInterface } from 'node:readline';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { once } from 'node:events';

const root = fileURLToPath(new URL('../..', import.meta.url));
const source = process.env.MOUNTX_SOURCE;
if (!source) throw new Error('MOUNTX_SOURCE is required');
const upstream = (path) => import(pathToFileURL(`${source}/${path}`).href);

const { conformance } = await upstream('test/conformance.ts');
const { createLoopback } = await upstream('src/harness.ts');
const { createMemoryDriver } = await upstream('src/drivers/memory.ts');
const { P9Session } = await upstream('src/9p/session.ts');
const { P9Client, p9Driver } = await upstream('test/9p/client.ts');

const build = execFileSync('cargo', [
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
  return { client: new P9Client(transport), close };
}

async function serveRust() {
  const child = spawn(fixture, [], {
    cwd: root,
    stdio: ['pipe', 'pipe', 'inherit'],
  });
  const exited = once(child, 'exit');
  const lines = createInterface({ input: child.stdout });
  let client;
  let disconnect;
  let timer;
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
    ({ client, close: disconnect } = await connectP9(port));
    await client.version();
    await client.attach(0);
    return { fs: createLoopback(p9Driver(client, 0)), cleanup: stop };
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
