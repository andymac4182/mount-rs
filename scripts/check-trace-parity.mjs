import {execFileSync} from 'node:child_process';
import {existsSync} from 'node:fs';
import {resolve} from 'node:path';
import {isDeepStrictEqual} from 'node:util';
import {pathToFileURL, fileURLToPath} from 'node:url';

const repo = fileURLToPath(new URL('..', import.meta.url));
const ORACLE_REPOSITORY = 'https://github.com/pithings/mountx';
const ORACLE_REVISION = '85361a8212ff9bff8e69f62fa8993ef2c2ec51e8';
const REPORT_SCHEMA = 'mount-rs/w01.3-trace-evidence@1';
const DEFAULT_SEEDS = [4182, 1, 42, 65535, 0xffffffff];
const DEFAULT_BACKENDS = [
  'memory',
  'sqlite',
  'object-store',
  'chunked-memory',
  'chunked-sqlite',
  'chunked-object-store',
];
const ALLOWED_BACKENDS = new Set([
  ...DEFAULT_BACKENDS,
  'pglite',
  'chunked-pglite',
  'r2',
]);
const FAILURE_CONTEXT_RADIUS = 2;

function textFrom(value) {
  if (value === undefined || value === null) return '';
  return Buffer.isBuffer(value) ? value.toString('utf8') : String(value);
}

function tail(value, maxLines = 8, maxChars = 1200) {
  const lines = textFrom(value).trim().split(/\r?\n/).filter(Boolean);
  const result = lines.slice(-maxLines).join('\n');
  return result.length > maxChars ? `…${result.slice(-maxChars)}` : result;
}

function parseSeeds() {
  const raw = process.env.MOUNT_RS_TRACE_SEEDS ?? DEFAULT_SEEDS.join(',');
  const tokens = raw.split(',').map(token => token.trim());
  if (tokens.length === 0 || tokens.some(token => token.length === 0)) {
    throw new Error('MOUNT_RS_TRACE_SEEDS must be a non-empty comma-separated list');
  }
  return tokens.map((token, index) => {
    if (!/^\d+$/.test(token)) {
      throw new Error(`MOUNT_RS_TRACE_SEEDS[${index}] is not a decimal unsigned 32-bit integer: ${JSON.stringify(token)}`);
    }
    const seed = Number(token);
    if (!Number.isSafeInteger(seed) || seed < 0 || seed > 0xffffffff) {
      throw new Error(`MOUNT_RS_TRACE_SEEDS[${index}] is outside the unsigned 32-bit range: ${JSON.stringify(token)}`);
    }
    return seed;
  });
}

function parseBackends() {
  const configured = process.env.MOUNT_RS_TRACE_BACKENDS;
  let backends;
  if (configured === undefined) {
    backends = [...DEFAULT_BACKENDS];
    if (process.env.PGLITE_DATABASE_URL) backends.push('pglite', 'chunked-pglite');
    if (process.env.MOUNT_RS_TRACE_R2 === '1') backends.push('r2');
  } else {
    const tokens = configured.split(',').map(token => token.trim());
    if (tokens.length === 0 || tokens.some(token => token.length === 0)) {
      throw new Error('MOUNT_RS_TRACE_BACKENDS must be a non-empty comma-separated list');
    }
    const unknown = tokens.filter(backend => !ALLOWED_BACKENDS.has(backend));
    if (unknown.length > 0) {
      throw new Error(`MOUNT_RS_TRACE_BACKENDS contains unsupported backend(s): ${unknown.join(', ')}`);
    }
    backends = tokens;
  }
  return backends;
}

function baseReport({status, source, seeds, backends, reason}) {
  return {
    schema: REPORT_SCHEMA,
    status,
    reason: reason ?? null,
    oracle: {
      repository: ORACLE_REPOSITORY,
      expectedRevision: ORACLE_REVISION,
      source: source ?? null,
      observedRevision: null,
    },
    seeds,
    backends,
    operationCount: null,
    results: [],
  };
}

function emitReport(report) {
  console.log(`TRACE_EVIDENCE_JSON ${JSON.stringify(report)}`);
}

function errorMessage(error) {
  return textFrom(error?.message || error).trim();
}

function inspectOracle(sourceValue) {
  if (!sourceValue) return {status: 'SKIP', source: null, observedRevision: null};

  const source = resolve(sourceValue);
  if (!existsSync(source)) {
    throw new Error(`MOUNTX_SOURCE does not exist: ${source}`);
  }
  let revision;
  try {
    revision = execFileSync('git', ['-C', source, 'rev-parse', 'HEAD'], {
      cwd: repo,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'pipe'],
    }).trim();
  } catch (error) {
    throw new Error(`cannot read the mountx revision from ${source}: ${tail(error?.stderr) || errorMessage(error)}`);
  }
  if (revision !== ORACLE_REVISION) {
    throw new Error(`MOUNTX_SOURCE revision mismatch: expected ${ORACLE_REVISION}, observed ${revision || '(empty)'}`);
  }
  for (const relativePath of ['src/drivers/memory.ts', 'src/harness.ts']) {
    if (!existsSync(resolve(source, relativePath))) {
      throw new Error(`pinned mountx source is missing ${relativePath}: ${source}`);
    }
  }
  return {status: 'RUN', source, observedRevision: revision};
}

function makeTrace(initialSeed, createMemoryDriver, createLoopback) {
  const fs = createLoopback(createMemoryDriver({uid: 0, gid: 0}));
  const commands = [
    ['mkdir', '/dir'],
    ['write', '/dir/file', 'data'],
    ['symlink', 'dir', '/alias'],
    ['mkdir_recursive', '/new/deep'],
    ['mkdir_recursive', '/new/deep'],
    ['mkdir_recursive', '/alias/nested/leaf'],
    ['mkdir_recursive', '/'],
  ];
  let seed = initialSeed;
  // Use high bits: low LCG bits alternate predictably and under-exercise operations.
  const next = n => {
    seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0;
    return Math.floor(seed / 0x100000000 * n);
  };
  const paths = [
    '/a',
    '/b',
    '/dir',
    '/dir/file',
    '/dir/a',
    '/alias/file',
    '/missing/x',
    '/z',
    '/',
    '/dir/../a',
    '/alias/../b',
    '/dir//file',
    'relative/file',
    '/new/deep',
    '/new/deep/leaf',
    '/alias',
  ];
  const ops = [
    'write',
    'mkdir',
    'rename',
    'link',
    'symlink',
    'unlink',
    'rmdir',
    'truncate',
    'read',
    'list',
    'stat',
    'lstat',
    'mkdir_recursive',
    'readlink',
    'chmod',
    'chown',
  ];
  for (let i = 0; i < 500; i++) {
    const op = ops[next(ops.length)];
    const path = paths[next(paths.length)];
    commands.push([
      op,
      path,
      op === 'write'
        ? `value-${i}`
        : op === 'truncate'
          ? next(12)
          : op === 'chmod'
            ? next(0o10000)
            : op === 'chown'
              ? next(65536)
              : paths[next(paths.length)],
    ]);
    if (i % 10 === 0) commands.push(['list', '/']);
  }
  // Reconcile every candidate path at the end, including failed mutations whose
  // return code matched but which might have left different filesystem state.
  for (const path of paths) {
    for (const op of ['read', 'list', 'lstat', 'readlink']) commands.push([op, path]);
  }
  return {commands, fs};
}

async function expectedTrace(initialSeed, createMemoryDriver, createLoopback) {
  const {commands, fs} = makeTrace(initialSeed, createMemoryDriver, createLoopback);
  const expected = [];
  for (const [op, path, arg] of commands) {
    try {
      let value = null;
      switch (op) {
        case 'write': await fs.writeFile(path, arg); break;
        case 'mkdir': await fs.mkdir(path, {mode: 0o755}); break;
        case 'mkdir_recursive': value = (await fs.mkdir(path, {mode: 0o755, recursive: true})) ?? null; break;
        case 'rename': await fs.rename(path, arg); break;
        case 'link': await fs.link(path, arg); break;
        case 'symlink': await fs.symlink(path, arg); break;
        case 'readlink': value = await fs.readlink(path); break;
        case 'chmod': await fs.chmod(path, arg); break;
        case 'chown': await fs.chown(path, arg, 5678); break;
        case 'unlink': await fs.unlink(path); break;
        case 'rmdir': await fs.rmdir(path); break;
        case 'truncate': await fs.truncate(path, arg); break;
        case 'read': value = Array.from(await fs.readFile(path)); break;
        case 'list': value = (await fs.readdir(path)).map(entry => entry.name); break;
        case 'stat':
        case 'lstat': {
          const stats = await fs[op](path);
          value = [stats.mode, stats.size, stats.nlink, stats.uid, stats.gid];
          break;
        }
      }
      expected.push({ok: value});
    } catch (error) {
      expected.push({error: error.code});
    }
  }
  return {commands, expected};
}

function failureContext({commands, expected, actual, operationIndex, backend, seed, detail}) {
  const index = operationIndex ?? Math.min(actual.length, expected.length, commands.length - 1);
  const start = Math.max(0, index - FAILURE_CONTEXT_RADIUS);
  const end = Math.min(commands.length, index + FAILURE_CONTEXT_RADIUS + 1);
  return {
    backend,
    seed,
    operationIndex: operationIndex ?? null,
    operationCount: commands.length,
    command: commands[index] ?? null,
    expected: expected[index] ?? null,
    actual: actual[index] ?? null,
    nearby: commands.slice(start, end).map((command, offset) => {
      const operation = start + offset;
      return {
        operationIndex: operation,
        command,
        expected: expected[operation] ?? null,
        actual: actual[operation] ?? null,
      };
    }),
    ...(detail ? {detail} : {}),
  };
}

function parseBackendOutput(raw, {commands, expected, backend, seed}) {
  const text = textFrom(raw).trim();
  const lines = text === '' ? [] : text.split(/\r?\n/);
  const actual = [];
  for (let lineIndex = 0; lineIndex < lines.length; lineIndex++) {
    try {
      actual.push(JSON.parse(lines[lineIndex]));
    } catch (error) {
      return {
        status: 'FAIL',
        backend,
        seed,
        operationCount: commands.length,
        failure: failureContext({
          commands,
          expected,
          actual,
          operationIndex: lineIndex,
          backend,
          seed,
          detail: {
            kind: 'invalid-json',
            outputLine: lineIndex,
            output: tail(lines[lineIndex], 1, 500),
            message: errorMessage(error),
          },
        }),
      };
    }
  }
  if (actual.length !== expected.length) {
    return {
      status: 'FAIL',
      backend,
      seed,
      operationCount: commands.length,
      failure: failureContext({
        commands,
        expected,
        actual,
        operationIndex: Math.min(actual.length, expected.length),
        backend,
        seed,
        detail: {
          kind: 'operation-count-mismatch',
          expectedOperationCount: expected.length,
          actualOperationCount: actual.length,
        },
      }),
    };
  }
  for (let operationIndex = 0; operationIndex < commands.length; operationIndex++) {
    if (!isDeepStrictEqual(actual[operationIndex], expected[operationIndex])) {
      return {
        status: 'FAIL',
        backend,
        seed,
        operationCount: commands.length,
        failure: failureContext({commands, expected, actual, operationIndex, backend, seed}),
      };
    }
  }
  return {status: 'PASS', backend, seed, operationCount: commands.length};
}

function runBackend(backend, seed, commands, expected) {
  try {
    const output = execFileSync('cargo', ['run', '--quiet', '--locked', '--example', 'trace_oracle', '--', backend], {
      cwd: repo,
      input: `${commands.map(command => JSON.stringify(command)).join('\n')}\n`,
      encoding: 'utf8',
      maxBuffer: 8 * 1024 * 1024,
    });
    return parseBackendOutput(output, {commands, expected, backend, seed});
  } catch (error) {
    return {
      status: 'FAIL',
      backend,
      seed,
      operationCount: commands.length,
      failure: failureContext({
        commands,
        expected,
        actual: [],
        operationIndex: null,
        backend,
        seed,
        detail: {
          kind: 'backend-process',
          message: errorMessage(error),
          stderr: tail(error?.stderr),
          stdout: tail(error?.stdout),
        },
      }),
    };
  }
}

async function main() {
  let seeds;
  let backends;
  try {
    seeds = parseSeeds();
    backends = parseBackends();
  } catch (error) {
    const report = baseReport({status: 'FAIL', source: process.env.MOUNTX_SOURCE, seeds: null, backends: null, reason: errorMessage(error)});
    console.error(`TRACE_EVIDENCE FAIL ${report.reason}`);
    emitReport(report);
    return 1;
  }

  let oracle;
  try {
    oracle = inspectOracle(process.env.MOUNTX_SOURCE);
  } catch (error) {
    const report = baseReport({status: 'FAIL', source: process.env.MOUNTX_SOURCE, seeds, backends, reason: errorMessage(error)});
    console.error(`TRACE_EVIDENCE FAIL ${report.reason}`);
    emitReport(report);
    return 1;
  }

  if (oracle.status === 'SKIP') {
    const report = baseReport({
      status: 'SKIP',
      source: null,
      seeds,
      backends,
      reason: 'MOUNTX_SOURCE is unset; the pinned mountx oracle is unavailable',
    });
    console.log(`TRACE_EVIDENCE SKIP oracle=${ORACLE_REVISION} source=(unset)`);
    emitReport(report);
    return 0;
  }

  const report = baseReport({status: 'PASS', source: oracle.source, seeds, backends});
  report.oracle.observedRevision = oracle.observedRevision;
  console.log(`TRACE_EVIDENCE RUN oracle=${ORACLE_REVISION} source=${oracle.source}`);
  console.log(`TRACE_EVIDENCE SEEDS ${seeds.join(',')} BACKENDS ${backends.join(',')}`);

  let createMemoryDriver;
  let createLoopback;
  try {
    ({createMemoryDriver} = await import(pathToFileURL(resolve(oracle.source, 'src/drivers/memory.ts'))));
    ({createLoopback} = await import(pathToFileURL(resolve(oracle.source, 'src/harness.ts'))));
  } catch (error) {
    report.status = 'FAIL';
    report.reason = `cannot load the pinned mountx memory oracle: ${errorMessage(error)}`;
    console.error(`TRACE_EVIDENCE FAIL ${report.reason}`);
    emitReport(report);
    return 1;
  }

  for (const initialSeed of seeds) {
    let trace;
    try {
      trace = await expectedTrace(initialSeed, createMemoryDriver, createLoopback);
    } catch (error) {
      report.status = 'FAIL';
      const result = {
        status: 'FAIL',
        seed: initialSeed,
        backend: 'oracle-memory',
        operationCount: null,
        failure: {
          kind: 'oracle-process',
          message: errorMessage(error),
        },
      };
      report.results.push(result);
      console.error(`TRACE_EVIDENCE FAIL seed=${initialSeed} backend=oracle-memory ${result.failure.message}`);
      continue;
    }
    report.operationCount ??= trace.commands.length;
    if (report.operationCount !== trace.commands.length) {
      report.status = 'FAIL';
      report.reason = `trace operation count changed between seeds: ${report.operationCount} and ${trace.commands.length}`;
      console.error(`TRACE_EVIDENCE FAIL ${report.reason}`);
      break;
    }
    for (const backend of backends) {
      const result = runBackend(backend, initialSeed, trace.commands, trace.expected);
      report.results.push(result);
      if (result.status === 'PASS') {
        console.log(`TRACE_EVIDENCE PASS seed=${initialSeed} backend=${backend} operations=${trace.commands.length}`);
      } else {
        report.status = 'FAIL';
        console.error(`TRACE_EVIDENCE FAIL seed=${initialSeed} backend=${backend} ${JSON.stringify(result.failure)}`);
      }
    }
  }

  emitReport(report);
  return report.status === 'PASS' ? 0 : 1;
}

process.exitCode = await main();
