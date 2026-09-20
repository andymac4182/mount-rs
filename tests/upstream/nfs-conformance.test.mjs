import { spawn, execFileSync } from 'node:child_process';
import { createInterface } from 'node:readline';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { once } from 'node:events';
const root = fileURLToPath(new URL('../..', import.meta.url));
const source = process.env.MOUNTX_SOURCE;
if (!source) throw new Error('MOUNTX_SOURCE is required');
const upstream = (path) => import(pathToFileURL(`${source}/${path}`).href);
const { conformance } = await upstream('test/conformance.ts');
const { createLoopback } = await upstream('src/harness.ts');
const { NfsClient, nfsDriver, check } = await upstream('test/nfs/v3/client.ts');
const { Nfs4Client } = await upstream('test/nfs/v4/client.ts');
const { nfs4Driver } = await upstream('test/nfs/v4/driver.ts');
const { createNfsServer } = await upstream('src/nfs/server.ts');
const { createMemoryDriver } = await upstream('src/drivers/memory.ts');

const build = execFileSync('cargo', [
  'build', '--locked', '--example', 'nfs_oracle', '--message-format=json',
], { cwd: root, stdio: ['ignore', 'pipe', 'inherit'], timeout: 180000, encoding: 'utf8' });
const fixture = build.trim().split('\n').map((line) => JSON.parse(line))
  .find((item) => item.reason === 'compiler-artifact' && item.target.name === 'nfs_oracle' && item.executable)
  ?.executable;
if (!fixture) throw new Error('Cargo did not return the NFS fixture executable');
// These are the upstream protocol column claims, including its real losses:
// stateless/unlinked handles and mknod types not carried by the NFS enums.
const capabilities = {
  handles: false, atomicRename: true, hardlinks: true, symlinks: true,
  permissions: true, times: true, truncate: true, caseSensitive: true,
  statfs: true, readOnly: false, durableWrites: false, extensions: ['mknod'],
};

async function serve(version) {
  const child = spawn(fixture, [], {
    cwd: root, stdio: ['pipe', 'pipe', 'inherit'],
  });
  const exited = once(child, 'exit');
  const lines = createInterface({ input: child.stdout });
  let client;
  let timer;
  const stop = async () => {
    try { client?.close(); } finally {
      child.stdin.end();
      const kill = setTimeout(() => child.kill('SIGKILL'), 5000);
      try {
        const [code, signal] = await exited;
        if (code !== 0) throw new Error(`NFS fixture exit: ${code}/${signal}`);
      } finally { clearTimeout(kill); lines.close(); }
    }
  };
  try {
    const port = await Promise.race([
      once(lines, 'line').then(([line]) => Number(line)),
      exited.then(([code]) => { throw new Error(`NFS fixture exited early: ${code}`); }),
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error('NFS fixture startup timed out')), 10000);
      }),
    ]);
    clearTimeout(timer);
    if (!Number.isInteger(port) || port <= 0 || port > 65535) throw new Error('Invalid NFS port');
    if (version === 3) {
      client = await NfsClient.connect({ port });
      const rootHandle = check(await client.mnt('/'), 'mount').fh;
      return { fs: createLoopback(nfsDriver(client, rootHandle)), cleanup: stop };
    }
    client = await Nfs4Client.open({ port });
    const rootHandle = await client.rootFh();
    return { fs: createLoopback(nfs4Driver(client, rootHandle)), cleanup: stop };
  } catch (error) {
    clearTimeout(timer);
    await stop();
    throw error;
  }
}
for (const version of [3, 4]) {
  conformance({
    name: `TypeScript NFSv${version === 3 ? '3' : '4.1'} control`,
    capabilities, carries: [], errors: version === 4 ? 'host' : 'linux',
    setup: async () => {
      const server = createNfsServer(createMemoryDriver());
      await server.listen();
      let client;
      const cleanup = async () => {
        client?.close();
        await server.close();
      };
      try {
        if (version === 3) {
          client = await NfsClient.connect({ port: server.port });
          const rootHandle = check(await client.mnt('/'), 'mount').fh;
          return { fs: createLoopback(nfsDriver(client, rootHandle)), cleanup };
        }
        client = await Nfs4Client.open({ port: server.port });
        return { fs: createLoopback(nfs4Driver(client, await client.rootFh())), cleanup };
      } catch (error) { await cleanup(); throw error; }
    },
  });
  conformance({
    name: `Rust NFSv${version === 3 ? '3' : '4.1'} through upstream TCP client`,
    capabilities, carries: [],
    errors: version === 4 ? 'host' : 'linux',
    setup: () => serve(version),
  });
}
