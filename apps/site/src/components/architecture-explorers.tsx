import { useState } from 'react'
import { benchRow } from '../content/benchmarks'

const fmt = (ms: number) => ms.toFixed(2)

/* ---------- Inside one node ---------- */

type Stage = {
  id: string
  label: string
  hint: string
  title: string
  body: string
  facts: string[]
}

const entryStages: Stage[] = [
  {
    id: 'proxy',
    label: 'TLS proxy / load balancer',
    hint: 'in front of every node',
    title: 'Any node will do',
    body: 'A proxy or load balancer terminates TLS and hands the request to whichever node it likes. It needs no knowledge of drives, so ordinary round robin and health checks are enough.',
    facts: ['Health check: /readyz', 'No sticky sessions needed', 'Stopping a node withdraws its routes at once'],
  },
  {
    id: 'listener',
    label: 'HTTP listener',
    hint: 'HTTP/1.1 or h2c',
    title: 'One listener, the same on every node',
    body: 'The listener accepts keep-alive client connections with TCP_NODELAY set, so small metadata responses are not held back waiting for more bytes.',
    facts: ['HTTP/1.1 or cleartext HTTP/2', 'Keep-alive connections', 'TCP_NODELAY on'],
  },
  {
    id: 'token',
    label: 'Drive token check',
    hint: 'one bearer token per drive',
    title: 'Authenticated once, at the edge of the node',
    body: 'Each drive has its own bearer token. The node checks it before doing anything else. If the request is forwarded, the owner checks the same token again.',
    facts: ['One token per drive', 'Checked on the ingress', 'Re-checked by the owner'],
  },
  {
    id: 'route',
    label: 'Own this drive?',
    hint: 'the only branch',
    title: 'The one decision that differs between nodes',
    body: 'Every node runs the same code. The only question is whether this process holds the drive’s fenced writer lease. If it does, the request never leaves the process unless it changes something.',
    facts: ['Exactly one owner per drive', 'Assigned by the operator, or taken over by a failover candidate', 'Owners publish their drives as routes'],
  },
]

const ownerStages: Stage[] = [
  {
    id: 'namespace',
    label: 'In-memory namespace',
    hint: 'stat · head · list',
    title: 'Metadata reads are answered from memory',
    body: 'The owner keeps the drive’s paths, attributes and block lists in memory. While its lease holds, that copy is authoritative, so metadata reads make no database call.',
    facts: [
      `Local stat p99 ${fmt(benchRow('exclusive', 'local', 'stat').p99)} ms`,
      '240 s lease, renewed every 30 s in the background',
      '60 s serving window, no per-operation renewal',
    ],
  },
  {
    id: 'blockcache',
    label: 'Block caches',
    hint: 'RAM, then disk',
    title: 'Hot bytes stay close',
    body: 'File bytes live in immutable blocks. A bounded RAM cache sits in front of a disk cache, which sits in front of the object store. Because blocks never change, a cached block never goes stale.',
    facts: ['RAM tier bounded by bytes and entries', 'Disk tier survives restarts', 'No invalidation needed'],
  },
  {
    id: 'stores',
    label: 'Metadata + block stores',
    hint: 'shared, replaceable',
    title: 'Two stores, two jobs',
    body: 'The metadata store holds namespaces, revisions and leases. The block store holds immutable bytes. They are chosen independently, and blocks always land before metadata refers to them.',
    facts: ['Metadata: TiDB, SQLite, FoundationDB, PGlite', 'Blocks: RustFS, S3, R2, local files', 'One fenced commit per durable write'],
  },
]

const forwardStages: Stage[] = [
  {
    id: 'cache',
    label: 'Route cache',
    hint: 'drive → owner',
    title: 'The owner is usually already known',
    body: 'The ingress caches which node owns each drive. Entries last one second, and routes still in use are re-confirmed in the background before they expire, so requests rarely wait on Redis.',
    facts: ['Up to 1,024 routes, 1 s entries', 'Refresh stops after 5 idle lifetimes', 'Concurrent misses share one lookup'],
  },
  {
    id: 'quic',
    label: 'HTTP/3 hop',
    hint: 'pooled QUIC, mutual TLS',
    title: 'One hop over a connection that is already open',
    body: 'The request streams to the owner over one of four pooled QUIC connections per peer. Both ends authenticate with pinned public keys from the cluster membership file.',
    facts: [
      `Forwarded stat p99 ${fmt(benchRow('durable-refresh', 'forwarded', 'stat').p99)} ms`,
      '4 connections per peer by default',
      'Never a second hop, never a client redirect',
    ],
  },
  {
    id: 'peer',
    label: 'Owner node',
    hint: 'verifies, then serves',
    title: 'The owner decides',
    body: 'The owner re-checks the token, that the route still names it, and its fencing generation, then serves the request exactly as if it had arrived directly. An uncertain forwarded write is reported as an error and never replayed.',
    facts: ['Token, route and fence all checked', 'Response streams back through the ingress', 'Unknown outcomes fail closed'],
  },
]

const allStages = [...entryStages, ...ownerStages, ...forwardStages]

function StageButton({ stage, selected, onSelect }: { stage: Stage; selected: boolean; onSelect: (id: string) => void }) {
  return (
    <button className="anat-stage" type="button" aria-pressed={selected} onClick={() => onSelect(stage.id)}>
      <strong>{stage.label}</strong>
      <small>{stage.hint}</small>
    </button>
  )
}

export function NodeAnatomy() {
  const [selected, setSelected] = useState('route')
  const stage = allStages.find((item) => item.id === selected) ?? entryStages[0]
  return (
    <div className="anat">
      <div className="anat-flow">
        <p className="anat-lane-label">Every request</p>
        <div className="anat-chain">
          {entryStages.map((item) => (
            <StageButton key={item.id} stage={item} selected={selected === item.id} onSelect={setSelected} />
          ))}
        </div>
        <div className="anat-branches">
          <div className="anat-branch anat-branch-owner">
            <p className="anat-lane-label">Yes: serve it here</p>
            <div className="anat-chain">
              {ownerStages.map((item) => (
                <StageButton key={item.id} stage={item} selected={selected === item.id} onSelect={setSelected} />
              ))}
            </div>
          </div>
          <div className="anat-branch anat-branch-forward">
            <p className="anat-lane-label">No: forward it once</p>
            <div className="anat-chain">
              {forwardStages.map((item) => (
                <StageButton key={item.id} stage={item} selected={selected === item.id} onSelect={setSelected} />
              ))}
            </div>
          </div>
        </div>
      </div>
      <div className="anat-detail" aria-live="polite">
        <p className="eyebrow">{stage.label}</p>
        <h3>{stage.title}</h3>
        <p>{stage.body}</p>
        <ul>
          {stage.facts.map((fact) => (
            <li key={fact}>{fact}</li>
          ))}
        </ul>
      </div>
    </div>
  )
}

/* ---------- Caching layers ---------- */

type Layer = { name: string; where: string; detail: string; saves: string }

const layers: Layer[] = [
  {
    name: 'Route cache',
    where: 'Ingress node, per drive',
    detail: 'One-second entries, re-confirmed in the background while a drive is in use.',
    saves: 'A Redis read before each forwarded request.',
  },
  {
    name: 'Owner serving window',
    where: 'Owner node, per drive',
    detail: 'A 240-second lease renewed every 30 seconds; the owner serves within a 60-second window.',
    saves: 'A lease round trip to the metadata store on every operation.',
  },
  {
    name: 'In-memory namespace',
    where: 'Owner node, per drive',
    detail: 'Paths, attributes and block lists for the whole drive.',
    saves: 'A database query for stat, head and list.',
  },
  {
    name: 'Covering revision index',
    where: 'TiDB',
    detail: 'An index over the revision, mode and lease columns.',
    saves: 'Reading the full namespace row to see whether it changed: 4.76 KB of TiKV traffic instead of 1.74 MB.',
  },
  {
    name: 'RAM block cache',
    where: 'Owner node',
    detail: 'Bounded by bytes and by entries.',
    saves: 'An object-store GET for hot file bytes.',
  },
  {
    name: 'Disk block cache',
    where: 'Owner node',
    detail: 'Scoped to the volume generation.',
    saves: 'An object-store GET after RAM eviction or a restart.',
  },
  {
    name: 'Object store',
    where: 'RustFS · S3 · R2 · local files',
    detail: 'Authoritative, immutable blocks.',
    saves: 'Nothing: this is the source of truth every cache above fills from.',
  },
]

type LayerState = 'hit' | 'pass' | 'skip'

type Scenario = { id: string; label: string; summary: string; states: LayerState[] }

const scenarios: Scenario[] = [
  {
    id: 'forwarded-stat',
    label: 'Forwarded stat',
    summary: `The ingress finds the owner in its route cache, and the owner answers from memory. Nothing below layer 3 is touched. Measured p99: ${fmt(benchRow('durable-refresh', 'forwarded', 'stat').p99)} ms.`,
    states: ['hit', 'hit', 'hit', 'skip', 'skip', 'skip', 'skip'],
  },
  {
    id: 'local-stat',
    label: 'Stat on the owner',
    summary: `No forwarding, so no route lookup. The owner is inside its serving window and answers from memory. Measured p99: ${fmt(benchRow('exclusive', 'local', 'stat').p99)} ms.`,
    states: ['skip', 'hit', 'hit', 'skip', 'skip', 'skip', 'skip'],
  },
  {
    id: 'revalidate',
    label: 'Check for changes',
    summary: 'When a node must confirm that its copy of a namespace is still current, it probes the covering index. If the revision is unchanged, the large namespace row is never read.',
    states: ['skip', 'skip', 'skip', 'hit', 'skip', 'skip', 'skip'],
  },
  {
    id: 'hot-read',
    label: 'Read a hot file',
    summary: 'The namespace supplies the file’s block list from memory, and the bytes come from the RAM cache. The object store is not contacted.',
    states: ['skip', 'hit', 'hit', 'skip', 'hit', 'skip', 'skip'],
  },
  {
    id: 'restart-read',
    label: 'Read after a restart',
    summary: 'The RAM cache is empty after a restart, but the disk cache still holds the blocks, so the read is served from local disk.',
    states: ['skip', 'hit', 'hit', 'skip', 'pass', 'hit', 'skip'],
  },
  {
    id: 'cold-read',
    label: 'Read a cold file',
    summary: 'Neither cache has the blocks, so they are fetched from the object store once and kept in both caches for the next reader.',
    states: ['skip', 'hit', 'hit', 'skip', 'pass', 'pass', 'hit'],
  },
]

const stateLabel: Record<LayerState, string> = { hit: 'answers here', pass: 'miss, continue', skip: 'not needed' }

export function LayerExplorer() {
  const [scenarioId, setScenarioId] = useState(scenarios[0].id)
  const [open, setOpen] = useState<number | null>(null)
  const scenario = scenarios.find((item) => item.id === scenarioId) ?? scenarios[0]
  const deepest = scenario.states.lastIndexOf('hit')
  return (
    <div className="layers">
      <div className="layers-side">
        <fieldset className="seg seg-stack">
          <legend>Follow a request</legend>
          {scenarios.map((item) => (
            <button
              key={item.id}
              type="button"
              aria-pressed={scenarioId === item.id}
              onClick={() => {
                setScenarioId(item.id)
                setOpen(null)
              }}
            >
              {item.label}
            </button>
          ))}
        </fieldset>
        <p className="layers-summary" aria-live="polite">
          {scenario.summary}
        </p>
      </div>
      <ol className="layers-stack">
        {layers.map((layer, index) => {
          const state = scenario.states[index]
          const expanded = open === index
          return (
            <li key={layer.name} className={`layer layer-${state}${index === deepest ? ' layer-final' : ''}`}>
              <button type="button" aria-expanded={expanded} onClick={() => setOpen(expanded ? null : index)}>
                <span className="layer-index">{String(index + 1).padStart(2, '0')}</span>
                <span className="layer-name">
                  <strong>{layer.name}</strong>
                  <small>{layer.where}</small>
                </span>
                <span className="layer-state">{stateLabel[state]}</span>
              </button>
              {expanded ? (
                <div className="layer-detail">
                  <p>{layer.detail}</p>
                  <p>
                    <strong>A hit saves:</strong> {layer.saves}
                  </p>
                </div>
              ) : null}
            </li>
          )
        })}
      </ol>
    </div>
  )
}

/* ---------- Write paths ---------- */

type WriteStep = { title: string; body: string; kind?: 'ack' | 'late' }

const writeModes: {
  id: 'durable' | 'writeback'
  label: string
  tagline: string
  steps: WriteStep[]
  numbers: string
}[] = [
  {
    id: 'durable',
    label: 'Durable (default)',
    tagline: 'The response means the change is committed.',
    steps: [
      { title: 'Request', body: 'A PUT, mkdir, rename or delete reaches the drive’s owner.' },
      { title: 'Write blocks', body: 'File bytes are stored as immutable blocks, streamed as they arrive.' },
      { title: 'Block barrier', body: 'The owner waits until the blocks are durable before anything refers to them.' },
      { title: 'One fenced commit', body: 'A single conditional update checks the revision and the writer fence.' },
      { title: '200 OK', body: 'Acknowledged only now. The change survives a crash.', kind: 'ack' },
    ],
    numbers: `Local mkdir p50 ${fmt(benchRow('durable', 'local', 'mkdir').p50)} ms, nearly all of it the TiDB commit.`,
  },
  {
    id: 'writeback',
    label: 'Writeback',
    tagline: 'The response means the owner has applied the change.',
    steps: [
      { title: 'Request', body: 'A mkdir, rename, delete or write reaches the drive’s owner.' },
      { title: 'Apply in memory', body: 'The owner updates its namespace under its exclusive lease.' },
      { title: '200 OK', body: 'Acknowledged straight away. Not yet durable.', kind: 'ack' },
      { title: 'Publish later', body: 'On sync or drain: blocks, barrier, then the same fenced commit.', kind: 'late' },
    ],
    numbers: `Local mkdir p99 ${fmt(benchRow('exclusive', 'local', 'mkdir').p99)} ms; forwarded ${fmt(benchRow('exclusive', 'forwarded', 'mkdir').p99)} ms.`,
  },
]

export function WritePathExplorer() {
  const [modeId, setModeId] = useState<'durable' | 'writeback'>('durable')
  const [current, setCurrent] = useState(0)
  const mode = writeModes.find((item) => item.id === modeId) ?? writeModes[0]
  const step = mode.steps[Math.min(current, mode.steps.length - 1)]
  return (
    <div className="wp">
      <div className="wp-head">
        <fieldset className="seg">
          <legend>Write mode</legend>
          {writeModes.map((item) => (
            <button
              key={item.id}
              type="button"
              aria-pressed={modeId === item.id}
              onClick={() => {
                setModeId(item.id)
                setCurrent(0)
              }}
            >
              {item.label}
            </button>
          ))}
        </fieldset>
        <p className="wp-tagline">{mode.tagline}</p>
      </div>
      <ol className="wp-steps">
        {mode.steps.map((item, index) => (
          <li
            key={item.title}
            className={`wp-step${item.kind ? ` wp-step-${item.kind}` : ''}${index === current ? ' is-current' : ''}${index < current ? ' is-done' : ''}`}
          >
            <button type="button" aria-current={index === current ? 'step' : undefined} onClick={() => setCurrent(index)}>
              <span className="wp-step-index">{index + 1}</span>
              <strong>{item.title}</strong>
              {item.kind === 'ack' ? <small>client is answered</small> : null}
              {item.kind === 'late' ? <small>after the response</small> : null}
            </button>
          </li>
        ))}
      </ol>
      <div className="wp-detail" aria-live="polite">
        <p>
          <strong>{step.title}.</strong> {step.body}
        </p>
        <div className="wp-nav">
          <button className="button button-secondary" type="button" disabled={current === 0} onClick={() => setCurrent(current - 1)}>
            Back
          </button>
          <button
            className="button button-primary"
            type="button"
            disabled={current >= mode.steps.length - 1}
            onClick={() => setCurrent(current + 1)}
          >
            Next step
          </button>
          <span className="wp-numbers">{mode.numbers}</span>
        </div>
      </div>
      <p className="wp-foot">
        Both paths fail closed: if the lease or fence is lost, the owner stops accepting operations
        and a replacement reloads authoritative metadata. An uncertain commit is reported as an
        error and never replayed.
      </p>
    </div>
  )
}
