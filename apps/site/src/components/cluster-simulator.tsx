import { useEffect, useMemo, useRef, useState } from 'react'
import { benchRow } from '../content/benchmarks'

// An interactive three-node cluster. Geometry is carried by SVG presentation
// attributes and classes only: the site's CSP forbids inline style attributes.

type Pt = [number, number]
type Op = 'stat' | 'mkdir-durable' | 'mkdir-writeback' | 'upload'
type Landing = 'auto' | 0 | 1 | 2
type Service = 'redis' | 'tidb' | 'rustfs'

type Step = {
  title: string
  body: string
  path?: Pt[]
  actors: string[]
  afterAck?: boolean
}

type Request = {
  drive: number
  op: Op
  ingress: number
  warm: boolean
  steps: Step[]
}

const nodeX = [170, 500, 830]
const nodeTop = 250
const nodeBottom = 374
const nodeMid = 312
const busY = 428
const serviceTop = 470
const serviceX: Record<Service, number> = { redis: 170, tidb: 500, rustfs: 830 }
const drives = ['drive-a', 'drive-b', 'drive-c']

const ops: { id: Op; label: string; verb: string }[] = [
  { id: 'stat', label: 'Read (stat)', verb: 'stat' },
  { id: 'mkdir-durable', label: 'mkdir, durable', verb: 'mkdir' },
  { id: 'mkdir-writeback', label: 'mkdir, writeback', verb: 'mkdir' },
  { id: 'upload', label: 'Upload a file', verb: 'file upload' },
]

const clientToLb: Pt[] = [
  [500, 76],
  [500, 122],
]

function lbToNode(node: number): Pt[] {
  return [
    [500, 178],
    [500, 212],
    [nodeX[node], 212],
    [nodeX[node], nodeTop],
  ]
}

function hop(from: number, to: number): Pt[] {
  if (Math.abs(from - to) === 1) {
    const dir = to > from ? 1 : -1
    return [
      [nodeX[from] + dir * 120, nodeMid],
      [nodeX[to] - dir * 120, nodeMid],
    ]
  }
  const dir = to > from ? 1 : -1
  return [
    [nodeX[from] + dir * 60, nodeBottom],
    [nodeX[from] + dir * 60, 400],
    [nodeX[to] - dir * 60, 400],
    [nodeX[to] - dir * 60, nodeBottom],
  ]
}

function toService(node: number, service: Service): Pt[] {
  const out: Pt[] = [
    [nodeX[node], nodeBottom],
    [nodeX[node], busY],
    [serviceX[service], busY],
    [serviceX[service], serviceTop],
  ]
  return out
}

function thereAndBack(path: Pt[]): Pt[] {
  return [...path, ...path.slice(0, -1).reverse()]
}

function reverse(path: Pt[]): Pt[] {
  return [...path].reverse()
}

function buildRequest(drive: number, op: Op, ingress: number, warm: boolean): Request {
  const owner = drive
  const name = drives[drive]
  const verb = ops.find((o) => o.id === op)?.verb ?? 'request'
  const n = `Node ${ingress + 1}`
  const o = `Node ${owner + 1}`
  const forwarded = ingress !== owner
  const steps: Step[] = []

  steps.push({
    title: 'The client sends its request',
    body: `A ${verb} for ${name}, carrying that drive’s token, goes to the cluster’s one address. The client does not know which server owns the drive.`,
    path: clientToLb,
    actors: ['client', 'lb'],
  })
  steps.push({
    title: `The load balancer picks ${n}`,
    body: 'It only spreads connections across healthy nodes. It has no drive table and needs none: any node can take any request.',
    path: lbToNode(ingress),
    actors: ['lb', `n${ingress}`],
  })

  if (!forwarded) {
    steps.push({
      title: `${n} owns ${name}`,
      body: `${n} checks the token and finds it holds this drive’s fenced writer lease, so the request stays in this process. No hop at all.`,
      actors: [`n${ingress}`],
    })
  } else {
    if (warm) {
      steps.push({
        title: `${n} already knows the owner`,
        body: `${n} checks the token, then its route cache: ${name} lives on ${o}. Routes in use are re-confirmed in the background, so there is no Redis call on the request path.`,
        actors: [`n${ingress}`],
      })
    } else {
      steps.push({
        title: 'Route cache miss: one Redis lookup',
        body: `${n} reads the route for ${name} from the registry and learns that ${o} owns it. Concurrent misses for the same drive share this one lookup, and the answer is cached.`,
        path: thereAndBack(toService(ingress, 'redis')),
        actors: [`n${ingress}`, 'redis'],
      })
    }
    steps.push({
      title: `One HTTP/3 hop to ${o}`,
      body: 'The request streams over one of four pooled QUIC connections to that peer, authenticated in both directions with pinned keys. There is never a second hop.',
      path: hop(ingress, owner),
      actors: [`n${ingress}`, `n${owner}`],
    })
    steps.push({
      title: `${o} verifies before serving`,
      body: 'The owner re-checks the original drive token, that the route still names it, and its current fencing generation.',
      actors: [`n${owner}`],
    })
  }

  if (op === 'stat') {
    steps.push({
      title: 'Answered from memory',
      body: `While its lease holds, ${o}’s in-memory namespace is authoritative, so stat, head and list never wait on the database.`,
      actors: [`n${owner}`],
    })
  } else if (op === 'mkdir-durable') {
    steps.push({
      title: 'One fenced commit to the metadata store',
      body: `${o} commits the new revision with a single conditional update that checks both the revision and the writer fence. Nothing is locked across drives.`,
      path: thereAndBack(toService(owner, 'tidb')),
      actors: [`n${owner}`, 'tidb'],
    })
  } else if (op === 'upload') {
    steps.push({
      title: 'Blocks stream to the object store',
      body: 'File bytes are written as immutable blocks while the upload is still arriving. A barrier confirms they are stored before anything refers to them.',
      path: thereAndBack(toService(owner, 'rustfs')),
      actors: [`n${owner}`, 'rustfs'],
    })
    steps.push({
      title: 'Then one fenced commit',
      body: `Only now does ${o} publish the file: one conditional update that checks the revision and the writer fence.`,
      path: thereAndBack(toService(owner, 'tidb')),
      actors: [`n${owner}`, 'tidb'],
    })
  } else {
    steps.push({
      title: 'Applied in memory',
      body: `On a writeback drive ${o} applies the change to its namespace and acknowledges straight away. It is not yet durable.`,
      actors: [`n${owner}`],
    })
  }

  const inbound = forwarded
    ? [...clientToLb, ...lbToNode(ingress), [nodeX[ingress], nodeMid] as Pt, ...hop(ingress, owner)]
    : [...clientToLb, ...lbToNode(ingress)]
  steps.push({
    title: 'The response returns',
    body: forwarded
      ? `${o} streams the response back through ${n}, the way it came. The client saw one address and one request.`
      : 'Straight back through the load balancer. The client saw one address and one request.',
    path: reverse(inbound),
    actors: forwarded ? ['client', 'lb', `n${ingress}`, `n${owner}`] : ['client', 'lb', `n${ingress}`],
  })

  if (op === 'mkdir-writeback') {
    steps.push({
      title: 'Afterwards: publish',
      body: `${o} commits the change with the same fenced update on the next sync or drain. A crash before then loses it, which is the trade writeback makes.`,
      path: toService(owner, 'tidb'),
      actors: [`n${owner}`, 'tidb'],
      afterAck: true,
    })
  }

  return { drive, op, ingress, warm, steps }
}

const fmt = (ms: number) => ms.toFixed(2)

function measured(op: Op, forwarded: boolean): string {
  const route = forwarded ? 'forwarded' : 'local'
  if (op === 'stat') {
    return forwarded
      ? `Measured: forwarded stat p99 ${fmt(benchRow('durable-refresh', 'forwarded', 'stat').p99)} ms on a durable TiDB drive.`
      : `Measured: local stat p99 ${fmt(benchRow('durable', 'local', 'stat').p99)} ms on a durable TiDB drive, with writes running.`
  }
  if (op === 'mkdir-durable') {
    return `Measured: ${route} durable mkdir p50 ${fmt(benchRow('durable', route, 'mkdir').p50)} ms, nearly all of it the TiDB commit.`
  }
  if (op === 'mkdir-writeback') {
    return `Measured: ${route} writeback mkdir p99 ${fmt(benchRow('exclusive', route, 'mkdir').p99)} ms.`
  }
  return 'Uploads depend on file size and the object store, so the metadata benchmark does not time them.'
}

function pathLength(path: Pt[]) {
  let total = 0
  for (let i = 1; i < path.length; i += 1) {
    total += Math.hypot(path[i][0] - path[i - 1][0], path[i][1] - path[i - 1][1])
  }
  return total
}

function pointAt(path: Pt[], t: number): Pt {
  const total = pathLength(path)
  let remaining = Math.min(Math.max(t, 0), 1) * total
  for (let i = 1; i < path.length; i += 1) {
    const segment = Math.hypot(path[i][0] - path[i - 1][0], path[i][1] - path[i - 1][1])
    if (remaining <= segment && segment > 0) {
      const k = remaining / segment
      return [
        path[i - 1][0] + (path[i][0] - path[i - 1][0]) * k,
        path[i - 1][1] + (path[i][1] - path[i - 1][1]) * k,
      ]
    }
    remaining -= segment
  }
  return path[path.length - 1]
}

const points = (path: Pt[]) => path.map((p) => p.join(',')).join(' ')

export function ClusterSimulator() {
  const [drive, setDrive] = useState(1)
  const [op, setOp] = useState<Op>('stat')
  const [landing, setLanding] = useState<Landing>('auto')
  const [warm, setWarm] = useState(true)
  const [request, setRequest] = useState<Request | null>(null)
  const [cursor, setCursor] = useState(-1)
  const [playing, setPlaying] = useState(false)
  const [traffic, setTraffic] = useState(false)
  const [counts, setCounts] = useState({ served: [0, 0, 0], forwarded: [0, 0, 0] })
  const roundRobin = useRef(0)
  const packet = useRef<SVGCircleElement>(null)
  const settings = useRef({ drive, op, landing, warm })
  settings.current = { drive, op, landing, warm }

  function nextIngress(choice: Landing) {
    if (choice !== 'auto') return choice
    const node = roundRobin.current % 3
    roundRobin.current += 1
    return node
  }

  function send(randomDrive: boolean) {
    const current = settings.current
    const target = randomDrive ? Math.floor(Math.random() * 3) : current.drive
    const next = buildRequest(target, current.op, nextIngress(current.landing), current.warm)
    setRequest(next)
    setCursor(0)
    setPlaying(true)
  }

  // Drive the packet along the current step, then advance.
  useEffect(() => {
    if (!playing || !request || cursor < 0) return
    const step = request.steps[cursor]
    const reduced = window.matchMedia('(prefers-reduced-motion: reduce)').matches
    const speed = traffic ? 3.2 : 1
    const dot = packet.current
    let frame = 0
    let timer = 0

    const finish = () => {
      if (cursor < request.steps.length - 1) {
        setCursor(cursor + 1)
        return
      }
      setPlaying(false)
      setCounts((previous) => {
        const served = [...previous.served]
        const forwarded = [...previous.forwarded]
        served[request.drive] += 1
        if (request.ingress !== request.drive) forwarded[request.ingress] += 1
        return { served, forwarded }
      })
    }

    if (!step.path) {
      timer = window.setTimeout(finish, (reduced ? 1500 : 1300) / speed)
      return () => window.clearTimeout(timer)
    }

    const path = step.path
    if (dot) dot.setAttribute('opacity', '1')
    if (reduced) {
      const end = path[path.length - 1]
      if (dot) {
        dot.setAttribute('cx', String(end[0]))
        dot.setAttribute('cy', String(end[1]))
      }
      timer = window.setTimeout(finish, 1500 / speed)
      return () => window.clearTimeout(timer)
    }

    const duration = Math.max(520, pathLength(path) * 2.1) / speed
    const started = performance.now()
    const tick = (now: number) => {
      const t = Math.min((now - started) / duration, 1)
      const eased = t < 0.5 ? 2 * t * t : 1 - (-2 * t + 2) ** 2 / 2
      const [x, y] = pointAt(path, eased)
      if (dot) {
        dot.setAttribute('cx', x.toFixed(1))
        dot.setAttribute('cy', y.toFixed(1))
      }
      if (t < 1) frame = window.requestAnimationFrame(tick)
      else timer = window.setTimeout(finish, 260 / speed)
    }
    frame = window.requestAnimationFrame(tick)
    return () => {
      window.cancelAnimationFrame(frame)
      window.clearTimeout(timer)
    }
  }, [playing, request, cursor, traffic])

  // In traffic mode, start the next request shortly after one finishes.
  useEffect(() => {
    if (!traffic || playing) return
    const timer = window.setTimeout(() => send(true), request ? 350 : 0)
    return () => window.clearTimeout(timer)
  }, [traffic, playing])

  function showStep(index: number) {
    if (!request) return
    setTraffic(false)
    setPlaying(false)
    setCursor(index)
    const path = request.steps[index].path
    const dot = packet.current
    if (!dot) return
    if (!path) {
      dot.setAttribute('opacity', '0')
      return
    }
    const end = path[path.length - 1]
    dot.setAttribute('opacity', '1')
    dot.setAttribute('cx', String(end[0]))
    dot.setAttribute('cy', String(end[1]))
  }

  function reset() {
    setTraffic(false)
    setPlaying(false)
    setRequest(null)
    setCursor(-1)
    setCounts({ served: [0, 0, 0], forwarded: [0, 0, 0] })
    roundRobin.current = 0
    packet.current?.setAttribute('opacity', '0')
  }

  const step = request && cursor >= 0 ? request.steps[cursor] : null
  const active = useMemo(() => new Set(step?.actors ?? []), [step])
  const done = request !== null && !playing && cursor === request.steps.length - 1
  const cls = (id: string, base: string) => (active.has(id) ? `${base} is-active` : base)

  return (
    <div className="sim">
      <figure className="sim-stage" aria-labelledby="sim-title">
        <div className="dg-scroll dg-scroll-wide">
          <svg className="dg sim-svg" viewBox="0 0 1000 560" role="img" aria-labelledby="sim-title sim-desc">
            <title id="sim-title">Three mount-rs nodes behind one load balancer</title>
            <desc id="sim-desc">
              Clients reach a load balancer that sends each request to any of three nodes. Every
              node is connected to the other two over HTTP/3, and all three share a Redis route
              registry, a metadata store and an object store. Each drive is owned by one node.
            </desc>

            {/* client-facing edges */}
            <polyline className="sim-edge" points={points(clientToLb)} />
            {[0, 1, 2].map((node) => (
              <polyline key={`lb-${node}`} className="sim-edge" points={points(lbToNode(node))} />
            ))}

            {/* peer mesh: every node reaches both others directly */}
            <polyline className="sim-edge sim-edge-peer" points={points(hop(0, 1))} />
            <polyline className="sim-edge sim-edge-peer" points={points(hop(1, 2))} />
            <polyline className="sim-edge sim-edge-peer" points={points(hop(0, 2))} />
            <text className="dg-tag sim-peer-label" x={500} y={416} textAnchor="middle">
              HTTP/3 peer mesh · mutual TLS · pooled
            </text>

            {/* shared services bus */}
            <line className="sim-edge sim-edge-store" x1={170} x2={830} y1={busY} y2={busY} />
            {nodeX.map((x) => (
              <line key={`stub-${x}`} className="sim-edge sim-edge-store" x1={x} x2={x} y1={nodeBottom} y2={busY} />
            ))}
            {(Object.keys(serviceX) as Service[]).map((service) => (
              <line
                key={`svc-${service}`}
                className="sim-edge sim-edge-store"
                x1={serviceX[service]}
                x2={serviceX[service]}
                y1={busY}
                y2={serviceTop}
              />
            ))}

            {step?.path ? (
              <polyline
                key={`${cursor}-${request?.ingress}-${request?.drive}`}
                className={step.afterAck ? 'sim-active sim-active-late' : 'sim-active'}
                points={points(step.path)}
              />
            ) : null}

            <g className={cls('client', 'sim-box sim-box-client')}>
              <rect x={390} y={20} width={220} height={56} rx={12} />
              <text className="dg-title" x={500} y={45} textAnchor="middle">
                Clients
              </text>
              <text className="dg-sub" x={500} y={63} textAnchor="middle">
                one address, a token per drive
              </text>
            </g>

            <g className={cls('lb', 'sim-box sim-box-lb')}>
              <rect x={370} y={122} width={260} height={56} rx={12} />
              <text className="dg-title" x={500} y={147} textAnchor="middle">
                Load balancer
              </text>
              <text className="dg-sub" x={500} y={165} textAnchor="middle">
                any healthy node · no drive table
              </text>
            </g>

            {nodeX.map((x, node) => (
              <g key={x} className={cls(`n${node}`, 'sim-box sim-box-node')}>
                <rect x={x - 120} y={nodeTop} width={240} height={nodeBottom - nodeTop} rx={14} />
                <text className="dg-title" x={x - 102} y={nodeTop + 28}>
                  Node {node + 1}
                </text>
                <text className="dg-tag" x={x + 102} y={nodeTop + 27} textAnchor="end">
                  serve-http
                </text>
                <rect
                  className={request && request.drive === node && step ? 'sim-drive is-target' : 'sim-drive'}
                  x={x - 102}
                  y={nodeTop + 42}
                  width={204}
                  height={30}
                  rx={8}
                />
                <text className="dg-sub dg-sub-strong" x={x - 90} y={nodeTop + 62}>
                  owns {drives[node]} · in memory
                </text>
                <text className="dg-sub" x={x - 102} y={nodeTop + 94}>
                  route cache · block cache
                </text>
                <text className="dg-label" x={x - 102} y={nodeTop + 113}>
                  served {counts.served[node]} · forwarded {counts.forwarded[node]}
                </text>
              </g>
            ))}

            <g className={cls('redis', 'sim-box sim-box-store')}>
              <rect x={50} y={serviceTop} width={240} height={66} rx={12} />
              <text className="dg-title" x={170} y={serviceTop + 28} textAnchor="middle">
                Redis route registry
              </text>
              <text className="dg-sub" x={170} y={serviceTop + 48} textAnchor="middle">
                which node owns each drive
              </text>
            </g>
            <g className={cls('tidb', 'sim-box sim-box-store')}>
              <rect x={380} y={serviceTop} width={240} height={66} rx={12} />
              <text className="dg-title" x={500} y={serviceTop + 28} textAnchor="middle">
                Metadata store
              </text>
              <text className="dg-sub" x={500} y={serviceTop + 48} textAnchor="middle">
                TiDB here: namespaces, leases
              </text>
            </g>
            <g className={cls('rustfs', 'sim-box sim-box-store')}>
              <rect x={710} y={serviceTop} width={240} height={66} rx={12} />
              <text className="dg-title" x={830} y={serviceTop + 28} textAnchor="middle">
                Object store
              </text>
              <text className="dg-sub" x={830} y={serviceTop + 48} textAnchor="middle">
                RustFS here: immutable blocks
              </text>
            </g>

            <circle ref={packet} className="sim-packet" cx={500} cy={76} r={9} opacity={0} />
          </svg>
        </div>
        <figcaption>
          <span className="sim-key sim-key-client" aria-hidden="true" /> client traffic
          <span className="sim-key sim-key-peer" aria-hidden="true" /> node-to-node HTTP/3
          <span className="sim-key sim-key-store" aria-hidden="true" /> shared services
          <span className="sim-scroll-hint">Swipe the diagram sideways to see all three nodes.</span>
        </figcaption>
      </figure>

      <div className="sim-panel">
        <div className="sim-controls">
          <fieldset className="seg">
            <legend>Drive</legend>
            {drives.map((name, index) => (
              <button key={name} type="button" aria-pressed={drive === index} onClick={() => setDrive(index)}>
                {name}
                <small>Node {index + 1}</small>
              </button>
            ))}
          </fieldset>
          <fieldset className="seg">
            <legend>Operation</legend>
            {ops.map((option) => (
              <button key={option.id} type="button" aria-pressed={op === option.id} onClick={() => setOp(option.id)}>
                {option.label}
              </button>
            ))}
          </fieldset>
          <fieldset className="seg">
            <legend>Load balancer sends it to</legend>
            <button type="button" aria-pressed={landing === 'auto'} onClick={() => setLanding('auto')}>
              Round robin
            </button>
            {[0, 1, 2].map((node) => (
              <button
                key={node}
                type="button"
                aria-pressed={landing === node}
                onClick={() => setLanding(node as Landing)}
              >
                Node {node + 1}
              </button>
            ))}
          </fieldset>
          <fieldset className="seg">
            <legend>Route cache on the receiving node</legend>
            <button type="button" aria-pressed={warm} onClick={() => setWarm(true)}>
              Warm
            </button>
            <button type="button" aria-pressed={!warm} onClick={() => setWarm(false)}>
              Cold
            </button>
          </fieldset>
        </div>

        <div className="sim-actions">
          <button
            className="button button-primary"
            type="button"
            onClick={() => {
              setTraffic(false)
              send(false)
            }}
          >
            Send request
          </button>
          <button className="button button-secondary" type="button" aria-pressed={traffic} onClick={() => setTraffic(!traffic)}>
            {traffic ? 'Stop traffic' : 'Run traffic'}
          </button>
          <button className="sim-reset" type="button" onClick={reset}>
            Reset
          </button>
        </div>

        <div className="sim-story" aria-live={traffic ? 'off' : 'polite'}>
          {request && step ? (
            <>
              <p className="sim-story-kicker">
                {ops.find((o) => o.id === request.op)?.label} · {drives[request.drive]} · via Node {request.ingress + 1}
                {request.ingress === request.drive ? ' (the owner)' : ` → Node ${request.drive + 1}`}
              </p>
              <ol className="sim-steps">
                {request.steps.map((item, index) => (
                  <li key={item.title} className={index === cursor ? 'is-current' : index < cursor ? 'is-done' : undefined}>
                    <button type="button" onClick={() => showStep(index)} aria-current={index === cursor ? 'step' : undefined}>
                      <span className="sim-step-index">{index + 1}</span>
                      <span>
                        <strong>{item.title}</strong>
                        {index === cursor ? <span className="sim-step-body">{item.body}</span> : null}
                      </span>
                    </button>
                  </li>
                ))}
              </ol>
              {done ? <p className="sim-measured">{measured(request.op, request.ingress !== request.drive)}</p> : null}
            </>
          ) : (
            <p className="sim-empty">
              Pick a drive and an operation, then send a request. Choose a node that does not own the
              drive to see forwarding, or run traffic to watch the load balancer share requests
              across all three.
            </p>
          )}
        </div>
      </div>
    </div>
  )
}
