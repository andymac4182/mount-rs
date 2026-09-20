import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
const cwd = fileURLToPath(new URL('..', import.meta.url));
const options = {cwd, env: process.env, encoding: 'utf8'};
const rust = JSON.parse(execFileSync('cargo', ['run','--locked','--quiet','--example','flags_parity'], options));
const upstream = JSON.parse(execFileSync('node', ['scripts/mountx-flags-oracle.mjs'], options));
assert.deepStrictEqual(rust, upstream);
console.log('mountx numeric flags and positional IO parity: PASS');
