import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
const cwd = fileURLToPath(new URL('..', import.meta.url));
const options = {cwd, env: process.env, encoding: 'utf8'};
const upstream = JSON.parse(execFileSync('node', ['scripts/mountx-flags-oracle.mjs'], options));
for (const backend of ['memory', 'chunked-memory', 'chunked-sqlite']) {
  const rust = JSON.parse(execFileSync('cargo', ['run','--locked','--quiet','--example','flags_parity','--',backend], options));
  assert.deepStrictEqual(rust, upstream, backend);
  console.log(`mountx numeric flags and positional IO parity (${backend}): PASS`);
}
