import { useState } from 'react'
import { benchModes, benchRows } from '../content/benchmarks'
import type { BenchMode, BenchRow } from '../content/benchmarks'

// Explore every measured row: pick a scenario and a percentile. Bars are SVG
// rects sized by attributes, because the CSP forbids inline styles.

type Percentile = 'p50' | 'p90' | 'p99' | 'p999'

const percentiles: { id: Percentile; label: string }[] = [
  { id: 'p50', label: 'p50' },
  { id: 'p90', label: 'p90' },
  { id: 'p99', label: 'p99' },
  { id: 'p999', label: 'p99.9' },
]

const modeOrder: { id: BenchMode; label: string }[] = [
  { id: 'durable-refresh', label: 'Durable reads' },
  { id: 'durable-reads', label: 'Refresh off' },
  { id: 'exclusive', label: 'Writeback' },
  { id: 'sqlite', label: 'SQLite' },
  { id: 'durable', label: 'Durable mixed' },
]

function niceMax(value: number) {
  const steps = [0.5, 1, 1.5, 2, 3, 5, 10, 20, 50, 100, 200, 400, 800]
  return steps.find((step) => step >= value * 1.08) ?? Math.ceil(value / 100) * 100
}

const fmt = (ms: number) => (ms >= 100 ? ms.toFixed(0) : ms >= 10 ? ms.toFixed(1) : ms.toFixed(2))

export function BenchExplorer() {
  const [mode, setMode] = useState<BenchMode>('durable-refresh')
  const [percentile, setPercentile] = useState<Percentile>('p99')
  const [focus, setFocus] = useState<BenchRow | null>(null)

  const rows = benchRows.filter((row) => row.mode === mode)
  const max = niceMax(Math.max(...rows.map((row) => row[percentile])))
  const left = 190
  const right = 900
  const rowHeight = 34
  const top = 34
  const height = top + rows.length * rowHeight + 36
  const x = (ms: number) => left + (ms / max) * (right - left)
  const ticks = [0, 0.25, 0.5, 0.75, 1].map((k) => k * max)
  const shown = focus && focus.mode === mode ? focus : null

  return (
    <div className="bx">
      <div className="bx-controls">
        <fieldset className="seg">
          <legend>Scenario</legend>
          {modeOrder.map((item) => (
            <button key={item.id} type="button" aria-pressed={mode === item.id} onClick={() => setMode(item.id)}>
              {item.label}
            </button>
          ))}
        </fieldset>
        <fieldset className="seg">
          <legend>Percentile</legend>
          {percentiles.map((item) => (
            <button
              key={item.id}
              type="button"
              aria-pressed={percentile === item.id}
              onClick={() => setPercentile(item.id)}
            >
              {item.label}
            </button>
          ))}
        </fieldset>
      </div>
      <p className="bx-mode">
        <strong>{benchModes[mode].title}.</strong> {benchModes[mode].detail}
      </p>
      <div className="dg-scroll dg-scroll-wide">
        <svg className="dg chart bx-chart" viewBox={`0 0 1000 ${height}`} role="img" aria-label={`${benchModes[mode].title}: ${percentile} latency per operation`}>
          {ticks.map((tick) => (
            <g key={tick}>
              <line className="chart-grid" x1={x(tick)} x2={x(tick)} y1={top - 8} y2={height - 30} />
              <text className="chart-tick" x={x(tick)} y={height - 10} textAnchor="middle">
                {fmt(tick)} ms
              </text>
            </g>
          ))}
          {rows.map((row, index) => {
            const y = top + index * rowHeight
            const value = row[percentile]
            const forwarded = row.route === 'forwarded'
            return (
              <g
                key={`${row.route}-${row.op}`}
                className="bx-row"
                tabIndex={0}
                onMouseEnter={() => setFocus(row)}
                onFocus={() => setFocus(row)}
              >
                <rect className="bx-hit" x={0} y={y} width={1000} height={rowHeight} />
                <text className="dg-title" x={0} y={y + 22}>
                  {row.op}
                </text>
                <text className="dg-sub" x={70} y={y + 22}>
                  {row.route}
                </text>
                <rect
                  className={value < 1 ? (forwarded ? 'bx-bar bx-bar-fwd' : 'bx-bar') : 'bx-bar bx-bar-slow'}
                  x={left}
                  y={y + 8}
                  width={Math.max(x(value) - left, 2)}
                  height={18}
                  rx={4}
                />
                <text className="chart-value" x={x(value) + 8} y={y + 22}>
                  {fmt(value)} ms
                </text>
              </g>
            )
          })}
          {max >= 1 && max <= 5 ? (
            <g>
              <line className="chart-target" x1={x(1)} x2={x(1)} y1={top - 14} y2={height - 30} />
              <text className="chart-target-label" x={x(1) + 6} y={top - 16}>
                1 ms target
              </text>
            </g>
          ) : null}
        </svg>
      </div>
      <p className="bx-foot" aria-live="polite">
        {shown ? (
          <>
            <strong>
              {shown.op}, {shown.route}:
            </strong>{' '}
            p50 {fmt(shown.p50)} · p90 {fmt(shown.p90)} · p99 {fmt(shown.p99)} · p99.9 {fmt(shown.p999)} ms ·{' '}
            {shown.under1ms.toFixed(1)}% under 1 ms · {shown.count.toLocaleString('en')} requests
          </>
        ) : (
          <>
            <span className="chart-key bx-key-local" aria-hidden="true" /> local
            <span className="chart-key bx-key-fwd" aria-hidden="true" /> forwarded
            <span className="chart-key bx-key-slow" aria-hidden="true" /> 1 ms or more · hover or focus a
            row for every percentile
          </>
        )}
      </p>
    </div>
  )
}
