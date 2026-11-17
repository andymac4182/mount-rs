// Measured by the Metadata latency workflow (.github/workflows/metadata-latency.yml)
// on a GitHub-hosted runner. Regenerate from the run's results-*.json artifact.

export type BenchMode = 'durable' | 'durable-reads' | 'durable-refresh' | 'exclusive' | 'sqlite'
export type BenchRoute = 'local' | 'forwarded'
export type BenchOp = 'ping' | 'stat' | 'head' | 'list' | 'mkdir' | 'rename' | 'delete'

export type BenchRow = {
  mode: BenchMode
  route: BenchRoute
  op: BenchOp
  count: number
  p50: number
  p90: number
  p99: number
  p999: number
  under1ms: number
}

export const benchRun = {
  id: '44015339854',
  url: 'https://github.com/andymac4182/mount-rs/actions/runs/44015339854',
  date: '17 November 2026',
  runner: '4 vCPU Intel Xeon 6973P-C, 15 GiB (GitHub ubuntu-latest)',
  drives: 100,
  nodes: 3,
  ratePerClient: 10,
  secondsPerMode: 20,
} as const

export const benchModes: Record<BenchMode, { title: string; detail: string }> = {
  durable: {
    title: 'Durable TiDB, reads and writes mixed',
    detail: 'Every mutation is committed to TiDB before the response. Route refresh off.',
  },
  'durable-reads': {
    title: 'Durable TiDB, reads only, route refresh off',
    detail: 'The ingress looks a route up in Redis whenever its one-second cache entry expires.',
  },
  'durable-refresh': {
    title: 'Durable TiDB, reads only, route refresh on',
    detail: 'The default: routes in use are confirmed in the background before they expire.',
  },
  exclusive: {
    title: 'Writeback (exclusive ownership)',
    detail: 'Mutations are acknowledged from memory and published to TiDB afterwards.',
  },
  sqlite: {
    title: 'SQLite metadata',
    detail: 'Local SQLite metadata on each owner, no TiDB round trips.',
  },
}

export const benchRows: BenchRow[] = [
  { mode: 'durable', route: 'local', op: 'ping', count: 2801, p50: 0.125, p90: 0.199, p99: 0.534, p999: 1.162, under1ms: 99.9 },
  { mode: 'durable', route: 'local', op: 'stat', count: 2900, p50: 0.137, p90: 0.206, p99: 0.538, p999: 0.97, under1ms: 99.9 },
  { mode: 'durable', route: 'local', op: 'head', count: 2900, p50: 0.142, p90: 0.208, p99: 0.366, p999: 0.695, under1ms: 100.0 },
  { mode: 'durable', route: 'local', op: 'list', count: 2900, p50: 0.166, p90: 0.245, p99: 0.459, p999: 1.201, under1ms: 99.8 },
  { mode: 'durable', route: 'local', op: 'mkdir', count: 2899, p50: 3.249, p90: 5.616, p99: 47.509, p999: 74.847, under1ms: 0.0 },
  { mode: 'durable', route: 'local', op: 'rename', count: 2800, p50: 3.198, p90: 5.02, p99: 30.647, p999: 69.591, under1ms: 0.0 },
  { mode: 'durable', route: 'local', op: 'delete', count: 2800, p50: 2.968, p90: 4.629, p99: 8.581, p999: 63.567, under1ms: 0.0 },
  { mode: 'durable', route: 'forwarded', op: 'ping', count: 2801, p50: 0.134, p90: 0.256, p99: 1.593, p999: 3.55, under1ms: 98.5 },
  { mode: 'durable', route: 'forwarded', op: 'stat', count: 2900, p50: 0.447, p90: 0.723, p99: 2.504, p999: 6.507, under1ms: 96.3 },
  { mode: 'durable', route: 'forwarded', op: 'head', count: 2900, p50: 0.46, p90: 0.727, p99: 1.674, p999: 3.165, under1ms: 96.8 },
  { mode: 'durable', route: 'forwarded', op: 'list', count: 2900, p50: 0.499, p90: 0.855, p99: 1.515, p999: 2.913, under1ms: 94.0 },
  { mode: 'durable', route: 'forwarded', op: 'mkdir', count: 2899, p50: 3.69, p90: 6.831, p99: 179.484, p999: 230.13, under1ms: 0.0 },
  { mode: 'durable', route: 'forwarded', op: 'rename', count: 2800, p50: 3.844, p90: 6.987, p99: 54.48, p999: 191.313, under1ms: 0.0 },
  { mode: 'durable', route: 'forwarded', op: 'delete', count: 2800, p50: 3.46, p90: 6.747, p99: 328.45, p999: 383.668, under1ms: 0.0 },
  { mode: 'durable-reads', route: 'forwarded', op: 'ping', count: 5000, p50: 0.13, p90: 0.184, p99: 0.308, p999: 0.567, under1ms: 100.0 },
  { mode: 'durable-reads', route: 'forwarded', op: 'stat', count: 5000, p50: 0.441, p90: 0.65, p99: 0.869, p999: 2.785, under1ms: 99.7 },
  { mode: 'durable-reads', route: 'forwarded', op: 'head', count: 5000, p50: 0.433, p90: 0.585, p99: 0.87, p999: 1.518, under1ms: 99.5 },
  { mode: 'durable-reads', route: 'forwarded', op: 'list', count: 5000, p50: 0.444, p90: 0.622, p99: 0.857, p999: 1.356, under1ms: 99.7 },
  { mode: 'durable-refresh', route: 'forwarded', op: 'ping', count: 5000, p50: 0.126, p90: 0.183, p99: 0.329, p999: 0.961, under1ms: 99.9 },
  { mode: 'durable-refresh', route: 'forwarded', op: 'stat', count: 5000, p50: 0.414, p90: 0.526, p99: 0.722, p999: 1.147, under1ms: 99.8 },
  { mode: 'durable-refresh', route: 'forwarded', op: 'head', count: 5000, p50: 0.425, p90: 0.533, p99: 0.726, p999: 1.525, under1ms: 99.8 },
  { mode: 'durable-refresh', route: 'forwarded', op: 'list', count: 5000, p50: 0.433, p90: 0.547, p99: 0.761, p999: 1.202, under1ms: 99.8 },
  { mode: 'exclusive', route: 'local', op: 'stat', count: 3399, p50: 0.132, p90: 0.184, p99: 0.26, p999: 0.595, under1ms: 100.0 },
  { mode: 'exclusive', route: 'local', op: 'head', count: 3300, p50: 0.134, p90: 0.188, p99: 0.261, p999: 0.455, under1ms: 100.0 },
  { mode: 'exclusive', route: 'local', op: 'list', count: 3300, p50: 0.148, p90: 0.208, p99: 0.308, p999: 0.641, under1ms: 100.0 },
  { mode: 'exclusive', route: 'local', op: 'mkdir', count: 3300, p50: 0.157, p90: 0.219, p99: 0.329, p999: 0.625, under1ms: 100.0 },
  { mode: 'exclusive', route: 'local', op: 'rename', count: 3301, p50: 0.16, p90: 0.215, p99: 0.312, p999: 0.513, under1ms: 100.0 },
  { mode: 'exclusive', route: 'local', op: 'delete', count: 3400, p50: 0.147, p90: 0.205, p99: 0.308, p999: 1.073, under1ms: 99.9 },
  { mode: 'exclusive', route: 'forwarded', op: 'stat', count: 3399, p50: 0.404, p90: 0.501, p99: 0.7, p999: 1.207, under1ms: 99.7 },
  { mode: 'exclusive', route: 'forwarded', op: 'head', count: 3300, p50: 0.408, p90: 0.501, p99: 0.685, p999: 0.918, under1ms: 100.0 },
  { mode: 'exclusive', route: 'forwarded', op: 'list', count: 3300, p50: 0.422, p90: 0.524, p99: 0.721, p999: 1.011, under1ms: 99.9 },
  { mode: 'exclusive', route: 'forwarded', op: 'mkdir', count: 3300, p50: 0.442, p90: 0.543, p99: 0.754, p999: 0.993, under1ms: 99.9 },
  { mode: 'exclusive', route: 'forwarded', op: 'rename', count: 3301, p50: 0.446, p90: 0.557, p99: 0.805, p999: 1.373, under1ms: 99.7 },
  { mode: 'exclusive', route: 'forwarded', op: 'delete', count: 3400, p50: 0.409, p90: 0.51, p99: 0.771, p999: 1.375, under1ms: 99.6 },
  { mode: 'sqlite', route: 'local', op: 'ping', count: 9999, p50: 0.115, p90: 0.167, p99: 0.252, p999: 0.925, under1ms: 99.9 },
  { mode: 'sqlite', route: 'local', op: 'stat', count: 10000, p50: 0.128, p90: 0.177, p99: 0.268, p999: 0.652, under1ms: 100.0 },
  { mode: 'sqlite', route: 'forwarded', op: 'ping', count: 10000, p50: 0.123, p90: 0.169, p99: 0.279, p999: 0.6, under1ms: 100.0 },
  { mode: 'sqlite', route: 'forwarded', op: 'stat', count: 10000, p50: 0.408, p90: 0.493, p99: 0.674, p999: 1.142, under1ms: 99.8 },
]

export function benchRow(mode: BenchMode, route: BenchRoute, op: BenchOp): BenchRow {
  const row = benchRows.find((r) => r.mode === mode && r.route === route && r.op === op)
  if (!row) throw new Error(`no benchmark row for ${mode} ${route} ${op}`)
  return row
}

// Forwarded stat p99 with route refresh on, on three different runner CPUs.
export const runnerVariance = [
  { runner: 'AMD EPYC 9V45', run: '43944670208', refreshOn: 0.652, refreshOff: 1.262, sqlite: 0.63 },
  { runner: 'AMD EPYC 9V74', run: '43944776323', refreshOn: 0.825, refreshOff: 0.916, sqlite: 0.741 },
  { runner: 'Intel Xeon 6973P-C', run: '44015339854', refreshOn: 0.722, refreshOff: 0.869, sqlite: 0.674 },
] as const
