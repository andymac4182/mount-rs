// Horizontal p50/p99 latency bars with a target line. SVG presentation
// attributes carry the geometry, so no inline style is needed under the CSP.

export type LatencyBar = {
  label: string
  detail: string
  p50: number
  p99: number
}

type LatencyChartProps = {
  id: string
  title: string
  bars: LatencyBar[]
  maxMs: number
  targetMs?: number
  ticks: number[]
}

export function LatencyChart({ id, title, bars, maxMs, targetMs, ticks }: LatencyChartProps) {
  const left = 300
  const right = 950
  const top = 40
  const rowHeight = 58
  const width = right - left
  const height = top + bars.length * rowHeight + 34
  const x = (ms: number) => left + (Math.min(ms, maxMs) / maxMs) * width

  return (
    <figure className="dg-figure chart-figure" aria-labelledby={`${id}-title`}>
      <div className="dg-scroll dg-scroll-wide">
        <svg className="dg chart" viewBox={`0 0 1000 ${height}`} role="img" aria-labelledby={`${id}-title ${id}-desc`}>
          <title id={`${id}-title`}>{title}</title>
          <desc id={`${id}-desc`}>
            {bars.map((bar) => `${bar.label}: p50 ${bar.p50} ms, p99 ${bar.p99} ms`).join('; ')}
          </desc>
          {ticks.map((tick) => (
            <g key={tick}>
              <line className="chart-grid" x1={x(tick)} x2={x(tick)} y1={top - 8} y2={height - 30} />
              <text className="chart-tick" x={x(tick)} y={height - 10} textAnchor="middle">
                {tick} ms
              </text>
            </g>
          ))}
          {bars.map((bar, index) => {
            const y = top + index * rowHeight
            return (
              <g key={bar.label}>
                <text className="dg-title" x={0} y={y + 18}>
                  {bar.label}
                </text>
                <text className="dg-sub" x={0} y={y + 38}>
                  {bar.detail}
                </text>
                <rect className="chart-bar-p99" x={left} y={y + 4} width={x(bar.p99) - left} height={18} rx={3} />
                <rect className="chart-bar-p50" x={left} y={y + 26} width={x(bar.p50) - left} height={18} rx={3} />
                <text className="chart-value" x={x(bar.p99) + 8} y={y + 18}>
                  p99 {bar.p99.toFixed(2)}
                </text>
                <text className="chart-value chart-value-muted" x={x(bar.p50) + 8} y={y + 40}>
                  p50 {bar.p50.toFixed(2)}
                </text>
              </g>
            )
          })}
          {targetMs !== undefined ? (
            <g>
              <line className="chart-target" x1={x(targetMs)} x2={x(targetMs)} y1={top - 18} y2={height - 30} />
              <text className="chart-target-label" x={x(targetMs) + 6} y={top - 22}>
                {targetMs} ms target
              </text>
            </g>
          ) : null}
        </svg>
      </div>
      <figcaption>
        <span className="chart-key chart-key-p99" aria-hidden="true" /> p99{' '}
        <span className="chart-key chart-key-p50" aria-hidden="true" /> p50 · request to response,
        measured by the client
      </figcaption>
    </figure>
  )
}
