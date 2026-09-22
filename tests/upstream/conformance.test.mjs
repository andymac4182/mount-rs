import { pathToFileURL } from 'node:url';
import { createRequire } from 'node:module';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
const require = createRequire(import.meta.url);
const binding = require('../../bindings/mount-rs-napi/index.js');
const source = process.env.MOUNTX_SOURCE;
if (!source) throw new Error('MOUNTX_SOURCE must identify the pinned upstream checkout');
const { conformance } = await import(pathToFileURL(`${source}/test/conformance.ts`).href);
const { createMemoryDriver } = await import(pathToFileURL(`${source}/src/drivers/memory.ts`).href);
const { createNodeFsDriver } = await import(pathToFileURL(`${source}/src/drivers/node-fs.ts`).href);
const { createLoopback, resolveCapabilities } = await import(pathToFileURL(`${source}/src/harness.ts`).href);

// Run the actual, unmodified pinned upstream suite, including its capability
// and host-privilege skips. The TS control prevents our harness from silently
// changing what the shared suite exercises.
conformance({
  name: 'TypeScript memory control',
  capabilities: resolveCapabilities(createMemoryDriver()),
  setup: async () => ({ fs: createLoopback(createMemoryDriver()) }),
});
conformance({
  name: 'Rust memory via napi-rs',
  capabilities: new binding.Filesystem().capabilities,
  setup: async () => ({ fs: new binding.Filesystem() }),
});
conformance({
  name: 'Rust createMemoryDriver via napi-rs',
  capabilities: binding.createMemoryDriver().capabilities,
  setup: async () => {
    const fs = binding.createMemoryDriver();
    return { fs, cleanup: () => fs.shutdown() };
  },
});
for (const implementation of ['TypeScript', 'Rust']) {
  const factory = implementation === 'Rust' ? binding.createNodeFsDriver : createNodeFsDriver;
  conformance({
    name: `${implementation} rooted host driver`,
    errors: 'host',
    capabilities: implementation === 'Rust'
      ? factory(tmpdir()).capabilities
      : resolveCapabilities(factory(tmpdir())),
    setup: async () => {
      const root = await mkdtemp(join(tmpdir(), 'mount-rs-upstream-host-'));
      const driver = factory(root);
      return {
        fs: implementation === 'Rust' ? driver : createLoopback(driver),
        cleanup: async () => {
          if (implementation === 'Rust') await driver.shutdown();
          // This is an owned host directory, never a mountpoint.
          await rm(root, { recursive: true, force: true });
        },
      };
    },
  });
}
const combinations = [['memory', 'memory'], ['sqlite', 'sqlite']];
if (process.env.PGLITE_DATABASE_URL) {
  combinations.push(['pglite', 'pglite'], ['pglite', 'sqlite'], ['sqlite', 'pglite']);
}
let nextVolume = 0;
const runId = `upstream-${process.pid}-${Date.now()}`;
for (const [metadataKind, blockKind] of combinations) {
  const options = () => {
    const key = `${runId}-${++nextVolume}`;
    const provider = (kind, role) => kind === 'pglite'
      ? { kind, uri: process.env.PGLITE_DATABASE_URL, key: `${key}-${role}` }
      : kind === 'sqlite' ? { kind, uri: ':memory:' } : { kind };
    return {
      metadata: provider(metadataKind, 'metadata'),
      blocks: provider(blockKind, 'blocks'),
      chunkSize: 7,
    };
  };
  const probe = await binding.createChunkedDriver(options());
  const capabilities = probe.capabilities;
  await probe.shutdown();
  conformance({
    name: `Rust chunked ${metadataKind}/${blockKind} via napi-rs`,
    capabilities,
    setup: async () => {
      const fs = await binding.createChunkedDriver(options());
      return { fs, cleanup: () => fs.shutdown() };
    },
  });
}
