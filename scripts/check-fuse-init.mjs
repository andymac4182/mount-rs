import {execFileSync} from 'node:child_process';
import {pathToFileURL, fileURLToPath} from 'node:url';
import assert from 'node:assert/strict';
const source = process.env.MOUNTX_SOURCE;
if (!source) throw new Error('MOUNTX_SOURCE is required');
const {negotiateInit} = await import(pathToFileURL(`${source}/src/fuse/init.ts`));
const expected = [];
for (const major of [6, 7, 8])
for (const minor of [4, 9, 13, 23, 28, 35, 36, 40, 41, 99])
for (const flags of [0, 0xffffffff])
for (const maxWrite of [0, 4095, 131072, 1048576, 0xffffffff]) {
  const result = negotiateInit({major, minor, flags, flags2: 0xffffffff, maxReadahead: 65536}, {maxWrite});
  if (result.status === 'error') { expected.push('error'); continue; }
  const r = result.reply;
  expected.push([result.status, r.major, r.minor, r.maxReadahead,
    BigInt(r.flags) | BigInt(r.flags2) << 32n, r.maxBackground,
    r.congestionThreshold, r.maxWrite, r.timeGran, r.maxPages, r.maxStackDepth].join(' '));
}
const actual = execFileSync('cargo', ['run', '--quiet', '--locked', '-p', 'mount-rs-fuse', '--example', 'init_oracle'], {
  cwd: fileURLToPath(new URL('..', import.meta.url)), encoding: 'utf8',
}).trim().split('\n');
assert.deepEqual(actual, expected);
console.log(`FUSE initialization TypeScript parity: PASS (${expected.length} cases)`);
