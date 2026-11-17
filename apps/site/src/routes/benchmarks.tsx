import { Link, createFileRoute } from '@tanstack/react-router'
import { BenchExplorer } from '../components/bench-explorer'
import { LatencyChart } from '../components/latency-chart'
import type { LatencyBar } from '../components/latency-chart'
import {
  benchModes,
  benchRow,
  benchRows,
  benchRun,
  runnerVariance,
} from '../content/benchmarks'
import type { BenchMode } from '../content/benchmarks'

export const Route = createFileRoute('/benchmarks')({
  head: () => ({
    meta: [
      { title: 'Benchmarks — mount-rs' },
      {
        name: 'description',
        content:
          'Metadata latency for 100 drives on three mount-rs servers with TiDB, RustFS and Redis, measured on GitHub-hosted runners.',
      },
    ],
  }),
  component: BenchmarksPage,
})

const fmt = (ms: number) => ms.toFixed(2)

const readBars: LatencyBar[] = [
  { label: 'Writeback stat, local', detail: 'client connected to the owner', ...pick('exclusive', 'local', 'stat') },
  { label: 'SQLite stat, local', detail: 'client connected to the owner', ...pick('sqlite', 'local', 'stat') },
  { label: 'Durable stat, forwarded', detail: 'route refresh on (default)', ...pick('durable-refresh', 'forwarded', 'stat') },
  { label: 'Durable list, forwarded', detail: 'route refresh on (default)', ...pick('durable-refresh', 'forwarded', 'list') },
  { label: 'Durable stat, forwarded', detail: 'route refresh off', ...pick('durable-reads', 'forwarded', 'stat') },
  { label: 'SQLite stat, forwarded', detail: 'one HTTP/3 hop', ...pick('sqlite', 'forwarded', 'stat') },
]

const writeBars: LatencyBar[] = [
  { label: 'Writeback mkdir, local', detail: 'acknowledged from memory', ...pick('exclusive', 'local', 'mkdir') },
  { label: 'Writeback rename, local', detail: 'acknowledged from memory', ...pick('exclusive', 'local', 'rename') },
  { label: 'Writeback mkdir, forwarded', detail: 'one HTTP/3 hop', ...pick('exclusive', 'forwarded', 'mkdir') },
  { label: 'Writeback rename, forwarded', detail: 'one HTTP/3 hop', ...pick('exclusive', 'forwarded', 'rename') },
  { label: 'Writeback delete, forwarded', detail: 'one HTTP/3 hop', ...pick('exclusive', 'forwarded', 'delete') },
]

function pick(mode: BenchMode, route: 'local' | 'forwarded', op: 'stat' | 'list' | 'mkdir' | 'rename' | 'delete') {
  const row = benchRow(mode, route, op)
  return { p50: row.p50, p99: row.p99 }
}

const headline = [
  { value: fmt(benchRow('durable-refresh', 'forwarded', 'stat').p99), unit: 'ms', label: 'p99 forwarded stat, durable TiDB drive' },
  { value: fmt(benchRow('exclusive', 'forwarded', 'rename').p99), unit: 'ms', label: 'p99 forwarded rename, writeback drive' },
  { value: fmt(benchRow('exclusive', 'local', 'stat').p99), unit: 'ms', label: 'p99 local stat on the owner' },
  { value: '0', unit: 'errors', label: `${benchRows.reduce((sum, row) => sum + row.count, 0).toLocaleString('en')} measured requests` },
]

const modeOrder: BenchMode[] = ['durable-refresh', 'durable-reads', 'exclusive', 'sqlite', 'durable']

function BenchmarksPage() {
  const durableMkdir = benchRow('durable', 'local', 'mkdir')
  const durableDelete = benchRow('durable', 'local', 'delete')
  return (
    <>
      <section className="hero arch-hero-wrap">
      <div className="page-frame arch-hero">
        <p className="eyebrow">Benchmarks / metadata latency</p>
        <h1>
          Sub-millisecond metadata, <span className="grad-text">even one hop away.</span>
        </h1>
        <p className="arch-lede">
          {benchRun.drives} drives on {benchRun.nodes} <code>serve-http</code>{' '}
          servers, with TiDB metadata, RustFS blocks and a Redis route registry.
          One client per drive, each on its own connection. Every client either
          talks to its drive’s owner, or to another server that forwards the
          request over HTTP/3.
        </p>
        <div className="bench-stats">
          {headline.map((stat) => (
            <div className="bench-stat" key={stat.label}>
              <strong>
                {stat.value}
                <span>{stat.unit}</span>
              </strong>
              <small>{stat.label}</small>
            </div>
          ))}
        </div>
        <p className="bench-run">
          Run <a href={benchRun.url}>{benchRun.id}</a> · {benchRun.date} · {benchRun.runner}
        </p>
      </div>
      </section>

      <section className="page-frame arch-section" id="explore">
        <div className="section-heading split-heading">
          <div>
            <p className="eyebrow">Explore</p>
            <h2>Every operation, every percentile.</h2>
          </div>
          <p>
            Pick a scenario and a percentile. Blue bars were served by the
            drive’s owner, violet bars were forwarded from another server, and
            amber bars took a millisecond or longer.
          </p>
        </div>
        <BenchExplorer />
      </section>

      <section className="page-frame arch-section">
        <div className="section-heading split-heading">
          <div>
            <p className="eyebrow">01 / reads</p>
            <h2>Read traffic stays under 1 ms at p99, even forwarded.</h2>
          </div>
          <p>
            Local reads are answered from the owner’s memory in about a tenth of
            a millisecond. Forwarding adds roughly 0.3 ms for the HTTP/3 hop,
            and background route refresh keeps the slowest 1% under the target.
            When the same drives also run durable writes, forwarded read p99
            rose to {fmt(benchRow('durable', 'forwarded', 'list').p99)}–
            {fmt(benchRow('durable', 'forwarded', 'stat').p99)} ms on this runner,
            as TiDB competes for its four vCPUs.
          </p>
        </div>
        <LatencyChart
          id="chart-reads"
          title="Metadata read latency, p50 and p99"
          bars={readBars}
          maxMs={1.5}
          targetMs={1}
          ticks={[0, 0.25, 0.5, 0.75, 1, 1.25, 1.5]}
        />
      </section>

      <section className="page-frame arch-section">
        <div className="section-heading split-heading">
          <div>
            <p className="eyebrow">02 / writes</p>
            <h2>Writeback mutations: under 1 ms too.</h2>
          </div>
          <p>
            With writeback, an owner applies mkdir, rename and delete in memory
            and publishes them afterwards. Forwarded mutations land under the
            target as well.
          </p>
        </div>
        <LatencyChart
          id="chart-writes"
          title="Writeback mutation latency, p50 and p99"
          bars={writeBars}
          maxMs={1.5}
          targetMs={1}
          ticks={[0, 0.25, 0.5, 0.75, 1, 1.25, 1.5]}
        />
        <div className="callout callout-amber">
          <strong>Durable mutations are bounded by the database commit.</strong>
          <p>
            A durable drive acknowledges a mutation only after TiDB commits it,
            so it cannot reach 1 ms: local mkdir measured p50{' '}
            {fmt(durableMkdir.p50)} ms and delete p50 {fmt(durableDelete.p50)} ms,
            most of it the TiDB commit. Their p99 tail varied from 13 ms to
            hundreds of milliseconds between runners. The likely cause is the
            runners’ disk sync latency under TiKV; that is not yet confirmed.
          </p>
        </div>
      </section>

      <section className="page-frame arch-section">
        <div className="section-heading split-heading">
          <div>
            <p className="eyebrow">03 / repeatability</p>
            <h2>Same code, three different runner CPUs.</h2>
          </div>
          <p>
            GitHub assigns a different CPU to each run, so every summary records
            it. With route refresh on, the default, forwarded reads met the target
            on all three. With it off, one runner missed.
          </p>
        </div>
        <div className="bench-table-wrap">
          <table className="bench-table">
            <caption>Forwarded stat p99, milliseconds</caption>
            <thead>
              <tr>
                <th scope="col">Runner CPU</th>
                <th scope="col">Durable, refresh on</th>
                <th scope="col">Durable, refresh off</th>
                <th scope="col">SQLite</th>
              </tr>
            </thead>
            <tbody>
              {runnerVariance.map((row) => (
                <tr key={row.run}>
                  <th scope="row">
                    <a href={`https://github.com/andymac4182/mount-rs/actions/runs/${row.run}`}>{row.runner}</a>
                  </th>
                  <td className={row.refreshOn < 1 ? 'bench-good' : 'bench-miss'}>{fmt(row.refreshOn)}</td>
                  <td className={row.refreshOff < 1 ? 'bench-good' : 'bench-miss'}>{fmt(row.refreshOff)}</td>
                  <td className={row.sqlite < 1 ? 'bench-good' : 'bench-miss'}>{fmt(row.sqlite)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </section>

      <section className="page-frame arch-section">
        <div className="section-heading split-heading">
          <div>
            <p className="eyebrow">04 / method</p>
            <h2>How it was measured.</h2>
          </div>
          <p>
            Everything runs on one GitHub-hosted runner, so these numbers include
            the servers, TiDB and the load client competing for four vCPUs.
          </p>
        </div>
        <div className="bench-method">
          <dl>
            <div>
              <dt>Cluster</dt>
              <dd>Three <code>serve-http</code> owners, each owning a third of the drives, forwarding to each other over HTTP/3 with mutual TLS.</dd>
            </div>
            <div>
              <dt>Storage</dt>
              <dd>TiDB v8.5.7 (one PD, one TiKV with production defaults, one TiDB), RustFS for blocks, Redis for routes. A SQLite run swaps TiDB for local SQLite.</dd>
            </div>
            <div>
              <dt>Load</dt>
              <dd>
                One client per drive on its own keep-alive HTTP/1.1 connection, {benchRun.ratePerClient} requests a second each, staggered so
                clients do not fire together. {benchRun.secondsPerMode} seconds measured per mode after a warm-up.
              </dd>
            </div>
            <div>
              <dt>Routes</dt>
              <dd>“Local” clients call their drive’s owner. “Forwarded” clients call the next server, which forwards every request.</dd>
            </div>
            <div>
              <dt>Operations</dt>
              <dd>stat and list are JSON metadata calls, head is <code>HEAD</code> on a file, ping is <code>/readyz</code>; mkdir, rename and delete change the namespace.</dd>
            </div>
          </dl>
        </div>
        <p className="storage-model-warning">
          Lessons from getting these numbers: Docker Desktop on a Mac added about
          0.5 ms to forwarded p99, and a TiKV configured for small test
          machines (one scheduler thread, 64 write latches) turned durable writes’
          p99 into 400 ms. Both were environment, not mount-rs, so the benchmark
          runs on Linux runners with TiKV defaults.
        </p>
      </section>

      <section className="page-frame arch-section">
        <details className="bench-details">
          <summary>All results from run {benchRun.id}</summary>
          {modeOrder.map((mode) => (
            <div className="bench-table-wrap" key={mode}>
              <table className="bench-table">
                <caption>
                  {benchModes[mode].title}
                  <span>{benchModes[mode].detail}</span>
                </caption>
                <thead>
                  <tr>
                    <th scope="col">Route</th>
                    <th scope="col">Op</th>
                    <th scope="col">Requests</th>
                    <th scope="col">p50 ms</th>
                    <th scope="col">p90 ms</th>
                    <th scope="col">p99 ms</th>
                    <th scope="col">p99.9 ms</th>
                    <th scope="col">Under 1 ms</th>
                  </tr>
                </thead>
                <tbody>
                  {benchRows
                    .filter((row) => row.mode === mode)
                    .map((row) => (
                      <tr key={`${row.route}-${row.op}`}>
                        <td>{row.route}</td>
                        <th scope="row">{row.op}</th>
                        <td>{row.count.toLocaleString('en')}</td>
                        <td>{fmt(row.p50)}</td>
                        <td>{fmt(row.p90)}</td>
                        <td className={row.p99 < 1 ? 'bench-good' : 'bench-miss'}>{fmt(row.p99)}</td>
                        <td>{fmt(row.p999)}</td>
                        <td>{row.under1ms.toFixed(1)}%</td>
                      </tr>
                    ))}
                </tbody>
              </table>
            </div>
          ))}
        </details>
      </section>

      <section className="page-frame arch-section arch-links">
        <div className="value-strip">
          <div>
            <p className="eyebrow">Run it yourself</p>
            <h2>The benchmark is in the repository.</h2>
          </div>
          <div className="value-list">
            <div>
              <strong>
                <a href="https://github.com/andymac4182/mount-rs/actions/workflows/metadata-latency.yml">Workflow</a>
              </strong>
              <span>Run “Metadata latency” by hand from GitHub Actions; it also runs when the benchmark changes.</span>
            </div>
            <div>
              <strong>
                <a href="https://github.com/andymac4182/mount-rs/blob/main/scripts/metadata-latency-bench.py">Harness</a>
              </strong>
              <span><code>scripts/metadata-latency-bench.py</code> starts the cluster and drives the load client.</span>
            </div>
            <div>
              <strong>
                <Link to="/architecture">Architecture</Link>
              </strong>
              <span>Why these paths are fast: ownership, forwarding and caching.</span>
            </div>
          </div>
        </div>
      </section>
    </>
  )
}
