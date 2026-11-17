import { useEffect, useMemo, useRef, useState } from 'react'
import { benchRow } from '../content/benchmarks'

// An interactive three-node cluster. Geometry is carried by SVG presentation
// attributes and classes only: the site's CSP forbids inline style attributes.

type Pt = [number, number]
type Op = 'stat' | 'mkdir-durable' | 'mkdir-writeback' | 'upload'
type Landing = 'auto' | 0 | 1 | 2
type Service = 'redis' | 'tidb' | 'rustfs'

// The cluster's state. A drive is not tied to a node: its owner is whichever
// candidate holds the storage writer lease and has published a route for it.
// `stale` names a crashed owner whose route is still in Redis; `home` is the
// node a drive is drawn on, which is its last owner while it has none.
type Cluster = {
  up: boolean[]
  owner: (number | null)[]
  stale: (number | null)[]
  home: number[]
  fence: number[]
}

type Step = {
  title: string
  body: string
  path?: Pt[]
  actors: string[]
  afterAck?: boolean
  // Applied to the cluster when the step completes. Failover steps only.
  effect?: (cluster: Cluster) => Cluster
}

type Scenario = {
  kind: 'request' | 'failover'
  kicker: string
  steps: Step[]
  // The drive highlighted while the scenario plays.
  target: number | null
  // Shown once the last step has played.
  note: string | null
  // Node counters to bump when a request completes.
  served: number | null
  forwarded: number | null
}

const initialCluster: Cluster = {
  up: [true, true, true],
  owner: [0, 1, 2],
  stale: [null, null, null],
  home: [0, 1, 2],
  fence: [1, 1, 1],
}

const nodeX = [170, 500, 830]
const nodeTop = 250
const nodeBottom = 374
const nodeMid = 312
const busY = 428
const serviceTop = 470
const serviceX: Record<Service, number> = { redis: 170, tidb: 500, rustfs: 830 }
const drives = ['drive-a', 'drive-b', 'drive-c']
const nodeName = (node: number) => `Node ${node + 1}`

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

function buildRequest(cluster: Cluster, drive: number, op: Op, ingress: number | null, warm: boolean): Scenario {
  const name = drives[drive]
  const option = ops.find((o) => o.id === op)
  const verb = option?.verb ?? 'request'
  const label = option?.label ?? 'Request'
  const steps: Step[] = []

  steps.push({
    title: 'The client sends its request',
    body: `A ${verb} for ${name}, carrying that drive’s token, goes to the cluster’s one address. The client does not know which server owns the drive.`,
    path: clientToLb,
    actors: ['client', 'lb'],
  })

  if (ingress === null) {
    steps.push({
      title: 'No node is running',
      body: 'Every node is stopped, so the load balancer has no healthy node to send the request to. Restart a node to bring the cluster back.',
      actors: ['lb'],
    })
    return { kind: 'request', kicker: `${label} · ${name} · no node running`, steps, target: drive, note: null, served: null, forwarded: null }
  }

  const n = nodeName(ingress)
  const toIngress = [...clientToLb, ...lbToNode(ingress)]
  steps.push({
    title: `The load balancer picks ${n}`,
    body: 'It only spreads connections across healthy nodes. It has no drive table and needs none: any node can take any request.',
    path: lbToNode(ingress),
    actors: ['lb', `n${ingress}`],
  })

  const owner = cluster.owner[drive]
  const stale = cluster.stale[drive]

  if (owner === null && stale !== null) {
    // The owner crashed moments ago and its route has not lapsed yet.
    const gone = nodeName(stale)
    steps.push({
      title: `The route still names ${gone}`,
      body: `${n} checks the token and looks up ${name}. ${gone} stopped without withdrawing its route, and a route stays published for up to 15 s after a crash.`,
      path: warm ? undefined : thereAndBack(toService(ingress, 'redis')),
      actors: warm ? [`n${ingress}`] : [`n${ingress}`, 'redis'],
    })
    steps.push({
      title: `${gone} does not answer`,
      body: `${n} forwards the request over HTTP/3, but nothing is listening. It waits for an answer for up to its 30 s operation timeout.`,
      path: hop(ingress, stale),
      actors: [`n${ingress}`, `n${stale}`],
    })
    steps.push({
      title: '502: the outcome is unknown',
      body: `${n} answers 502 with x-mount-dispatch: unknown. It cannot tell whether the owner acted, so the request is never replayed. A read may be retried; a write has to be reconciled by the client, for example with If-Match.`,
      path: reverse(toIngress),
      actors: ['client', 'lb', `n${ingress}`],
    })
    return { kind: 'request', kicker: `${label} · ${name} · via ${n} → ${gone} (stopped)`, steps, target: drive, note: null, served: null, forwarded: null }
  }

  if (owner === null) {
    steps.push({
      title: 'No route is published',
      body: `${n} checks the token and reads the route for ${name} from the registry. There is none: the old owner’s route has lapsed and no standby has published one yet.`,
      path: thereAndBack(toService(ingress, 'redis')),
      actors: [`n${ingress}`, 'redis'],
    })
    steps.push({
      title: '503: retry in a second',
      body: `${n} answers 503 with x-mount-dispatch: not-dispatched and Retry-After: 1. Nothing was executed, so the client can retry safely, and the retry succeeds once a standby has published itself.`,
      path: reverse(toIngress),
      actors: ['client', 'lb', `n${ingress}`],
    })
    return { kind: 'request', kicker: `${label} · ${name} · via ${n} · no owner`, steps, target: drive, note: null, served: null, forwarded: null }
  }

  const o = nodeName(owner)
  const forwarded = ingress !== owner

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

  const inbound = forwarded ? [...toIngress, [nodeX[ingress], nodeMid] as Pt, ...hop(ingress, owner)] : toIngress
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

  return {
    kind: 'request',
    kicker: `${label} · ${name} · via ${n}${forwarded ? ` → ${o}` : ' (the owner)'}`,
    steps,
    target: drive,
    note: measured(op, forwarded),
    served: owner,
    forwarded: forwarded ? ingress : null,
  }
}

// Failover. In this simulation every node is a candidate for every drive.
// The real ranking is a rendezvous hash of (drive, node); a fixed order
// stands in for it here, so that the outcome is the same on every visit.
function successor(cluster: Cluster, drive: number): number | null {
  for (let i = 0; i < 3; i += 1) {
    const node = (drive + i) % 3
    if (cluster.up[node]) return node
  }
  return null
}

function listNames(group: number[]) {
  const names = group.map((d) => drives[d])
  return names.length < 2 ? names.join('') : `${names.slice(0, -1).join(', ')} and ${names[names.length - 1]}`
}

function withAll<T>(values: T[], group: number[], value: (previous: T) => T): T[] {
  return values.map((previous, index) => (group.includes(index) ? value(previous) : previous))
}

// The two steps by which a node becomes the owner of a group of drives:
// acquire each storage writer lease, and only then publish the routes.
function takeoverSteps(cluster: Cluster, node: number, group: number[], why: string): Step[] {
  const s = nodeName(node)
  const names = listNames(group)
  const fences = group.map((d) => `${drives[d]} is now at fence ${cluster.fence[d] + 1}`).join(', ')
  return [
    {
      title: `${s} acquires the writer lease`,
      body: `${why} That succeeds only when the metadata store grants the exclusive writer lease, and it will not while the crashed process’s lease is still valid: 240 s by default. Until then the attempt fails with EAGAIN and is retried. Each acquisition raises the fence: ${fences}.`,
      path: thereAndBack(toService(node, 'tidb')),
      actors: [`n${node}`, 'tidb'],
      effect: (c) => ({ ...c, fence: withAll(c.fence, group, (fence) => fence + 1) }),
    },
    {
      title: `${s} publishes the route`,
      body: `Only now, with the lease held and ${names} open, does ${s} publish itself as the owner under the new fence. Redis will not replace a live route with a lower or equal fence, so routes only move forward. Other nodes see the new owner within about a second.`,
      path: thereAndBack(toService(node, 'redis')),
      actors: [`n${node}`, 'redis'],
      effect: (c) => ({
        ...c,
        owner: withAll(c.owner, group, () => node),
        home: withAll(c.home, group, () => node),
      }),
    },
  ]
}

const failoverMeasured =
  'Measured with real binaries, SQLite metadata and a 20 s lease: about 21 s from kill -9 to the new route. With the default 240 s lease it is about four minutes. A clean stop releases the lease and handed over in 0.1 to 1 s.'

function buildStop(cluster: Cluster, node: number): { next: Cluster; scenario: Scenario } {
  const x = nodeName(node)
  const lost = drives.map((_, d) => d).filter((d) => cluster.owner[d] === node)
  const next: Cluster = {
    ...cluster,
    up: cluster.up.map((up, index) => (index === node ? false : up)),
    owner: withAll(cluster.owner, lost, () => null),
    stale: withAll(cluster.stale, lost, () => node),
    home: withAll(cluster.home, lost, () => node),
  }

  if (lost.length === 0) {
    return {
      next,
      scenario: {
        kind: 'failover',
        kicker: `${x} stopped · it owned no drive`,
        steps: [
          {
            title: `${x} stops`,
            body: `${x} was a standby and held no drive, so nothing has to move. The load balancer’s health check fails and it stops sending requests here.`,
            actors: [`n${node}`, 'lb'],
          },
        ],
        target: null,
        note: null,
        served: null,
        forwarded: null,
      },
    }
  }

  const names = listNames(lost)
  const steps: Step[] = [
    {
      title: `${x} stops without warning`,
      body: `The process is killed, as by kill -9 or a power loss. It cannot withdraw its route for ${names} or release the writer lease, so both stay in place until they expire. The load balancer’s health check fails and it stops sending requests here.`,
      actors: [`n${node}`, 'lb'],
    },
    {
      title: `The route for ${names} lapses`,
      body: `An owner refreshes its route every 5 s and a route lives for 15 s. Nobody refreshes this one now, so Redis drops it within 15 s. Until it does, other nodes still forward to ${x} and those requests end in 502.`,
      actors: ['redis'],
      effect: (c) => ({ ...c, stale: withAll(c.stale, lost, () => null) }),
    },
  ]

  const takers = [0, 1, 2].filter((candidate) => lost.some((d) => successor(next, d) === candidate))
  for (const taker of takers) {
    const group = lost.filter((d) => successor(next, d) === taker)
    const s = nodeName(taker)
    const groupNames = listNames(group)
    steps.push({
      title: `${s} reads no route for ${groupNames}`,
      body: `A standby reads each drive’s route once a second. ${s} ranks first among the running candidates for ${groupNames}, so it acts at once; a lower-ranked standby waits 2 s per rank. A registry that cannot be read is never taken to mean “no route”.`,
      path: thereAndBack(toService(taker, 'redis')),
      actors: [`n${taker}`, 'redis'],
    })
    steps.push(...takeoverSteps(next, taker, group, `${s} opens ${groupNames}.`))
  }

  if (takers.length === 0) {
    steps.push({
      title: 'No standby is running',
      body: `Every candidate for ${names} is stopped, so nothing can take over. Requests get 503 until a node is restarted and acquires the lease.`,
      actors: [],
    })
  }

  const moves = takers.map((taker) => `${listNames(lost.filter((d) => successor(next, d) === taker))} → ${nodeName(taker)}`)
  return {
    next,
    scenario: {
      kind: 'failover',
      kicker: `${x} stopped · ${takers.length ? moves.join(' · ') : `${names} ${lost.length > 1 ? 'have' : 'has'} no owner`}`,
      steps,
      target: lost.length === 1 ? lost[0] : null,
      note: takers.length ? failoverMeasured : null,
      served: null,
      forwarded: null,
    },
  }
}

function buildRestart(cluster: Cluster, node: number): { next: Cluster; scenario: Scenario } {
  const x = nodeName(node)
  const next: Cluster = { ...cluster, up: cluster.up.map((up, index) => (index === node ? true : up)) }
  const orphans = drives.map((_, d) => d).filter((d) => next.owner[d] === null)
  const steps: Step[] = [
    {
      title: `${x} starts`,
      body: `Before it opens any drive, ${x} reads the route registry. The load balancer’s health check passes again, so requests can land here.`,
      path: thereAndBack(toService(node, 'redis')),
      actors: [`n${node}`, 'redis', 'lb'],
    },
  ]

  if (orphans.length) {
    const names = listNames(orphans)
    steps.push(...takeoverSteps(next, node, orphans, `No route is published for ${names} and no other candidate is running, so ${x} opens ${orphans.length > 1 ? 'them' : 'it'}.`))
    return {
      next,
      scenario: {
        kind: 'failover',
        kicker: `${x} restarted · takes ${names}`,
        steps,
        target: orphans.length === 1 ? orphans[0] : null,
        note: null,
        served: null,
        forwarded: null,
      },
    }
  }

  const owners = [0, 1, 2].filter((candidate) => next.owner.includes(candidate))
  steps.push({
    title: `${x} is a standby`,
    body: `Every drive already has a published owner, so ${x} opens nothing and holds no lease. It forwards requests like any other node, and takes a drive only if that drive’s route disappears.`,
    actors: [`n${node}`],
  })
  steps.push({
    title: 'Drives stay where they are',
    body: 'There is no automatic fail-back by default: moving a drive means draining it, and that breaks its open sessions. With fail_back: true an owner hands a drive back once a better-ranked candidate has ranked first for 60 s without interruption.',
    actors: owners.map((owner) => `n${owner}`),
  })
  return {
    next,
    scenario: {
      kind: 'failover',
      kicker: `${x} restarted · standby, no fail-back`,
      steps,
      target: null,
      note: null,
      served: null,
      forwarded: null,
    },
  }
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
const peerPairs: [number, number][] = [
  [0, 1],
  [1, 2],
  [0, 2],
]

export function ClusterSimulator() {
  const [drive, setDrive] = useState(1)
  const [op, setOp] = useState<Op>('stat')
  const [landing, setLanding] = useState<Landing>('auto')
  const [warm, setWarm] = useState(true)
  const [cluster, setCluster] = useState(initialCluster)
  const [scenario, setScenario] = useState<Scenario | null>(null)
  const [cursor, setCursor] = useState(-1)
  const [playing, setPlaying] = useState(false)
  const [traffic, setTraffic] = useState(false)
  const [counts, setCounts] = useState({ served: [0, 0, 0], forwarded: [0, 0, 0] })
  const roundRobin = useRef(0)
  const packet = useRef<SVGCircleElement>(null)
  const settings = useRef({ drive, op, landing, warm })
  settings.current = { drive, op, landing, warm }
  const live = useRef({ scenario, cursor, playing })
  live.current = { scenario, cursor, playing }
  // The cluster is read synchronously when a request or a failover is built.
  const clusterNow = useRef(cluster)
  // A failover whose later steps have not changed the cluster yet.
  const pending = useRef<{ steps: Step[]; next: number } | null>(null)
  // A failover that a request interrupted; it resumes when the request ends.
  const suspended = useRef<{ scenario: Scenario; cursor: number } | null>(null)

  function commit(next: Cluster) {
    clusterNow.current = next
    setCluster(next)
  }

  function applyThrough(index: number) {
    const job = pending.current
    if (!job) return
    let next = clusterNow.current
    for (let i = job.next; i <= index && i < job.steps.length; i += 1) {
      const effect = job.steps[i].effect
      if (effect) next = effect(next)
    }
    job.next = Math.max(job.next, index + 1)
    if (job.next >= job.steps.length) pending.current = null
    commit(next)
  }

  // Finish a failover that is still under way, so the cluster is consistent
  // before something else changes it.
  function settle() {
    const job = pending.current
    if (job) applyThrough(job.steps.length - 1)
    suspended.current = null
  }

  function nextIngress(choice: Landing, up: boolean[]): number | null {
    if (choice !== 'auto' && up[choice]) return choice
    const healthy = [0, 1, 2].filter((node) => up[node])
    if (healthy.length === 0) return null
    const node = healthy[roundRobin.current % healthy.length]
    roundRobin.current += 1
    return node
  }

  function send(randomDrive: boolean) {
    const current = settings.current
    const now = live.current
    if (now.playing && now.scenario?.kind === 'failover' && pending.current?.steps === now.scenario.steps) {
      suspended.current = { scenario: now.scenario, cursor: now.cursor }
    }
    const state = clusterNow.current
    const target = randomDrive ? Math.floor(Math.random() * 3) : current.drive
    const next = buildRequest(state, target, current.op, nextIngress(current.landing, state.up), current.warm)
    setScenario(next)
    setCursor(0)
    setPlaying(true)
  }

  function toggleNode(node: number) {
    setTraffic(false)
    settle()
    const state = clusterNow.current
    const stopping = state.up[node]
    const { next, scenario: story } = stopping ? buildStop(state, node) : buildRestart(state, node)
    commit(next)
    pending.current = { steps: story.steps, next: 0 }
    if (stopping && settings.current.landing === node) setLanding('auto')
    setScenario(story)
    setCursor(0)
    setPlaying(true)
  }

  // Drive the packet along the current step, then advance.
  useEffect(() => {
    if (!playing || !scenario || cursor < 0) return
    const step = scenario.steps[cursor]
    const reduced = window.matchMedia('(prefers-reduced-motion: reduce)').matches
    const speed = traffic ? 3.2 : 1
    const dot = packet.current
    let frame = 0
    let timer = 0

    const finish = () => {
      if (pending.current?.steps === scenario.steps) applyThrough(cursor)
      if (cursor < scenario.steps.length - 1) {
        setCursor(cursor + 1)
        return
      }
      if (scenario.kind === 'request') {
        const { served: servedBy, forwarded: forwardedBy } = scenario
        if (servedBy !== null) {
          setCounts((previous) => {
            const served = [...previous.served]
            const forwarded = [...previous.forwarded]
            served[servedBy] += 1
            if (forwardedBy !== null) forwarded[forwardedBy] += 1
            return { served, forwarded }
          })
        }
        const resume = suspended.current
        if (resume) {
          suspended.current = null
          setScenario(resume.scenario)
          setCursor(resume.cursor)
          return
        }
      }
      setPlaying(false)
    }

    if (!step.path) {
      if (dot) dot.setAttribute('opacity', '0')
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
  }, [playing, scenario, cursor, traffic])

  // In traffic mode, start the next request shortly after one finishes.
  useEffect(() => {
    if (!traffic || playing) return
    const timer = window.setTimeout(() => send(true), scenario ? 350 : 0)
    return () => window.clearTimeout(timer)
  }, [traffic, playing])

  function showStep(index: number) {
    if (!scenario) return
    setTraffic(false)
    setPlaying(false)
    settle()
    setCursor(index)
    const path = scenario.steps[index].path
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
    setScenario(null)
    setCursor(-1)
    setCounts({ served: [0, 0, 0], forwarded: [0, 0, 0] })
    pending.current = null
    suspended.current = null
    commit(initialCluster)
    roundRobin.current = 0
    packet.current?.setAttribute('opacity', '0')
  }

  const step = scenario && cursor >= 0 ? scenario.steps[cursor] : null
  const active = useMemo(() => new Set(step?.actors ?? []), [step])
  const done = scenario !== null && !playing && cursor === scenario.steps.length - 1
  const cls = (id: string, base: string) => (active.has(id) ? `${base} is-active` : base)
  const edge = (base: string, ...nodes: number[]) => (nodes.every((node) => cluster.up[node]) ? base : `${base} is-off`)

  return (
    <div className="sim">
      <figure className="sim-stage" aria-labelledby="sim-title">
        <div className="dg-scroll dg-scroll-wide">
          <svg className="dg sim-svg" viewBox="0 0 1000 560" role="img" aria-labelledby="sim-title sim-desc">
            <title id="sim-title">Three mount-rs nodes behind one load balancer</title>
            <desc id="sim-desc">
              Clients reach a load balancer that sends each request to any running node. Every
              node is connected to the other two over HTTP/3, and all three share a Redis route
              registry, a metadata store and an object store. Each drive is owned by one node at
              a time; when that node stops, a standby takes the drive over. The controls and the
              step list beside the diagram give the same information as text.
            </desc>

            {/* client-facing edges */}
            <polyline className="sim-edge" points={points(clientToLb)} />
            {[0, 1, 2].map((node) => (
              <polyline key={`lb-${node}`} className={edge('sim-edge', node)} points={points(lbToNode(node))} />
            ))}

            {/* peer mesh: every node reaches both others directly */}
            {peerPairs.map(([from, to]) => (
              <polyline key={`peer-${from}-${to}`} className={edge('sim-edge sim-edge-peer', from, to)} points={points(hop(from, to))} />
            ))}
            <text className="dg-tag sim-peer-label" x={500} y={416} textAnchor="middle">
              HTTP/3 peer mesh · mutual TLS · pooled
            </text>

            {/* shared services bus */}
            <line className="sim-edge sim-edge-store" x1={170} x2={830} y1={busY} y2={busY} />
            {nodeX.map((x, node) => (
              <line key={`stub-${x}`} className={edge('sim-edge sim-edge-store', node)} x1={x} x2={x} y1={nodeBottom} y2={busY} />
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
                key={`${cursor}-${scenario?.kicker}`}
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

            {nodeX.map((x, node) => {
              const up = cluster.up[node]
              const held = drives.map((_, d) => d).filter((d) => cluster.home[d] === node)
              const owns = held.some((d) => cluster.owner[d] === node)
              return (
                <g
                  key={x}
                  className={cls(`n${node}`, up ? 'sim-box sim-box-node' : 'sim-box sim-box-node is-down')}
                  onClick={() => toggleNode(node)}
                >
                  <rect x={x - 120} y={nodeTop} width={240} height={nodeBottom - nodeTop} rx={14} />
                  <text className="dg-title" x={x - 102} y={nodeTop + 28}>
                    Node {node + 1}
                  </text>
                  <text className="dg-tag" x={x + 102} y={nodeTop + 27} textAnchor="end">
                    {up ? 'serve-http' : 'stopped'}
                  </text>
                  {held.map((d, slot) => {
                    const lost = cluster.owner[d] === null
                    const target = scenario !== null && scenario.target === d && step !== null
                    return (
                      <g key={drives[d]}>
                        <rect
                          className={`sim-drive${lost ? ' is-lost' : ''}${target ? ' is-target' : ''}`}
                          x={x - 102 + slot * 70}
                          y={nodeTop + 42}
                          width={64}
                          height={30}
                          rx={8}
                        />
                        <text className="dg-sub dg-sub-strong" x={x - 70 + slot * 70} y={nodeTop + 62} textAnchor="middle">
                          {drives[d]}
                        </text>
                      </g>
                    )
                  })}
                  {held.length === 0 ? (
                    <text className="dg-sub" x={x - 102} y={nodeTop + 62}>
                      holds no drive
                    </text>
                  ) : null}
                  <text className="dg-sub" x={x - 102} y={nodeTop + 94}>
                    {!up ? 'not running' : owns ? 'owner · namespace in memory' : 'standby · forwards only'}
                  </text>
                  <text className="dg-label" x={x - 102} y={nodeTop + 113}>
                    served {counts.served[node]} · forwarded {counts.forwarded[node]}
                  </text>
                </g>
              )
            })}

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
          <span className="sim-note">
            Here every node is a failover candidate for every drive. Stop a node, with its button
            or by clicking it, and a standby takes its drives once it holds the storage writer
            lease. A restarted node comes back as a standby: there is no automatic fail-back by
            default.
          </span>
        </figcaption>
      </figure>

      <div className="sim-panel">
        <div className="sim-controls">
          <fieldset className="seg">
            <legend>Drive</legend>
            {drives.map((name, index) => {
              const owner = cluster.owner[index]
              return (
                <button key={name} type="button" aria-pressed={drive === index} onClick={() => setDrive(index)}>
                  {name}
                  <small>{owner === null ? 'no owner' : `${nodeName(owner)} · fence ${cluster.fence[index]}`}</small>
                </button>
              )
            })}
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
                disabled={!cluster.up[node]}
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
          <fieldset className="seg sim-nodes">
            <legend>Failover: stop a node</legend>
            {[0, 1, 2].map((node) => (
              <button
                key={node}
                type="button"
                aria-pressed={!cluster.up[node]}
                aria-label={`Stop Node ${node + 1}`}
                onClick={() => toggleNode(node)}
              >
                Node {node + 1}
                <small>{cluster.up[node] ? 'running · stop it' : 'stopped · restart it'}</small>
              </button>
            ))}
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
          {scenario && step ? (
            <>
              <p className="sim-story-kicker">{scenario.kicker}</p>
              <ol className="sim-steps">
                {scenario.steps.map((item, index) => (
                  <li key={`${index}-${item.title}`} className={index === cursor ? 'is-current' : index < cursor ? 'is-done' : undefined}>
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
              {done && scenario.note ? <p className="sim-measured">{scenario.note}</p> : null}
            </>
          ) : (
            <p className="sim-empty">
              Pick a drive and an operation, then send a request. Choose a node that does not own the
              drive to see forwarding, or run traffic to watch the load balancer share requests
              across all three. Stop a node to watch a standby take over its drive, and send a
              request while the takeover is running to see the retryable 503.
            </p>
          )}
        </div>
      </div>
    </div>
  )
}
