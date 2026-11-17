import type { ReactNode } from 'react'

// Diagrams are inline SVG styled by classes in app.css (`dg-*`), so they
// follow the light and dark themes and need no inline style attributes,
// which the site's Content-Security-Policy forbids.

type Tone = 'paper' | 'blue' | 'cream' | 'lilac' | 'ink' | 'green'

type BoxProps = {
  x: number
  y: number
  w: number
  h: number
  title: string
  lines?: string[]
  tone?: Tone
  tag?: string
}

function Box({ x, y, w, h, title, lines = [], tone = 'paper', tag }: BoxProps) {
  const textX = x + 16
  const firstLine = y + (tag ? 44 : 30)
  return (
    <g>
      <rect className={`dg-box dg-${tone}`} x={x} y={y} width={w} height={h} rx={10} />
      {tag ? (
        <text className="dg-tag" x={textX} y={y + 20}>
          {tag}
        </text>
      ) : null}
      <text className={tone === 'ink' ? 'dg-title dg-title-light' : 'dg-title'} x={textX} y={firstLine}>
        {title}
      </text>
      {lines.map((line, index) => (
        <text
          key={line}
          className={tone === 'ink' ? 'dg-sub dg-sub-light' : 'dg-sub'}
          x={textX}
          y={firstLine + 20 + index * 18}
        >
          {line}
        </text>
      ))}
    </g>
  )
}

type ArrowProps = {
  d: string
  label?: string
  labelX?: number
  labelY?: number
  dashed?: boolean
  strong?: boolean
  anchor?: 'start' | 'middle' | 'end'
}

function Arrow({ d, label, labelX, labelY, dashed, strong, anchor = 'start' }: ArrowProps) {
  const className = ['dg-arrow', dashed ? 'dg-dashed' : '', strong ? 'dg-strong' : '']
    .filter(Boolean)
    .join(' ')
  return (
    <g>
      <path className={className} d={d} markerEnd={strong ? 'url(#dg-head-strong)' : 'url(#dg-head)'} />
      {label && labelX !== undefined && labelY !== undefined ? (
        <text className="dg-label" x={labelX} y={labelY} textAnchor={anchor}>
          {label}
        </text>
      ) : null}
    </g>
  )
}

function Step({ x, y, n }: { x: number; y: number; n: number }) {
  return (
    <g>
      <circle className="dg-step" cx={x} cy={y} r={12} />
      <text className="dg-step-text" x={x} y={y + 4} textAnchor="middle">
        {n}
      </text>
    </g>
  )
}

function Markers() {
  return (
    <defs>
      <marker id="dg-head" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
        <path className="dg-head" d="M 0 0 L 10 5 L 0 10 z" />
      </marker>
      <marker id="dg-head-strong" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="6" markerHeight="6" orient="auto-start-reverse">
        <path className="dg-head-strong" d="M 0 0 L 10 5 L 0 10 z" />
      </marker>
    </defs>
  )
}

function Figure({
  id,
  title,
  caption,
  viewBox,
  minWidth,
  children,
}: {
  id: string
  title: string
  caption: ReactNode
  viewBox: string
  minWidth: 'narrow' | 'wide'
  children: ReactNode
}) {
  return (
    <figure className="dg-figure" aria-labelledby={`${id}-title`}>
      <div className={`dg-scroll dg-scroll-${minWidth}`}>
        <svg className="dg" viewBox={viewBox} role="img" aria-labelledby={`${id}-title ${id}-desc`}>
          <title id={`${id}-title`}>{title}</title>
          <desc id={`${id}-desc`}>{typeof caption === 'string' ? caption : title}</desc>
          <Markers />
          {children}
        </svg>
      </div>
      <figcaption>{caption}</figcaption>
    </figure>
  )
}

export function ServerDiagram() {
  return (
    <Figure
      id="dg-server"
      title="How one serve-http node handles a request"
      caption="A request is authenticated once, then either served in-process by the drive's owner or forwarded in one hop to the node that owns it. Owners answer metadata reads from the namespace they hold in memory."
      viewBox="0 0 760 900"
      minWidth="narrow"
    >
      <Box x={230} y={16} w={300} h={64} title="Client" lines={['one keep-alive connection per client']} tone="lilac" />
      <Arrow d="M 380 80 L 380 108" />
      <Box x={230} y={110} w={300} h={64} title="TLS proxy or load balancer" lines={['terminates TLS; may pick any node']} tone="paper" />
      <Arrow d="M 380 174 L 380 216" />

      <rect className="dg-frame" x={16} y={196} width={728} height={690} rx={14} />
      <text className="dg-frame-label" x={36} y={222}>
        serve-http node
      </text>

      <Box x={230} y={232} w={300} h={64} title="HTTP listener" lines={['HTTP/1.1 or h2c · TCP_NODELAY on']} tone="blue" />
      <Arrow d="M 380 296 L 380 322" />
      <Box x={230} y={324} w={300} h={64} title="Drive token check" lines={['one bearer token per drive']} tone="blue" />
      <Arrow d="M 380 388 L 380 414" />

      <path className="dg-box dg-cream" d="M 380 416 L 560 452 L 380 488 L 200 452 Z" />
      <text className="dg-title" x={380} y={448} textAnchor="middle">
        Does this node
      </text>
      <text className="dg-title" x={380} y={468} textAnchor="middle">
        own the drive?
      </text>

      <Arrow d="M 200 452 L 150 452 L 150 528" label="yes" labelX={162} labelY={444} />
      <Arrow d="M 560 452 L 610 452 L 610 528" label="no" labelX={598} labelY={444} anchor="end" />

      <Box x={40} y={530} w={300} h={64} title="Owner router" lines={['serves the drive in this process']} tone="green" />
      <Arrow d="M 190 594 L 190 620" />
      <Box
        x={40}
        y={622}
        w={300}
        h={98}
        title="Chunked filesystem"
        lines={['namespace held in memory', 'stat · head · list answered here', 'lease serving window, no per-op renewal']}
        tone="green"
      />
      <Arrow d="M 120 720 L 120 746" />
      <Arrow d="M 260 720 L 260 746" />
      <Box x={40} y={748} w={140} h={118} title="Metadata" lines={['TiDB · SQLite', 'FoundationDB', 'PGlite']} tone="paper" />
      <Box x={200} y={748} w={140} h={118} title="Blocks" lines={['RAM cache', '→ disk cache', '→ RustFS · S3', '· R2 · local']} tone="paper" />

      <Box
        x={420}
        y={530}
        w={300}
        h={98}
        title="Ingress forwarder"
        lines={['route cache: drive → owner node', 'refreshed in the background', 'pooled QUIC connections per owner']}
        tone="cream"
      />
      <Arrow d="M 570 628 L 570 668" strong label="HTTP/3 · mutual TLS" labelX={584} labelY={654} />
      <Box
        x={420}
        y={670}
        w={300}
        h={196}
        title="Owner node (another server)"
        lines={[
          'checks the original drive token',
          'checks the route is still its own',
          'checks its current fencing generation',
          'serves from its in-memory namespace',
          'streams the response back',
        ]}
        tone="lilac"
        tag="one hop, never more"
      />
    </Figure>
  )
}

export function ForwardingDiagram() {
  return (
    <Figure
      id="dg-forwarding"
      title="Forwarding a request across a three-node cluster"
      caption="Any node accepts any drive's requests. Owners register their drives in Redis; ingresses cache those routes and forward over pooled, pinned HTTP/3 connections. Storage is shared, so a drive can move to another owner after its lease ends."
      viewBox="0 0 1000 640"
      minWidth="wide"
    >
      <Box x={24} y={24} w={190} h={72} title="Client" lines={['drive d042, token t042']} tone="lilac" />
      <Arrow d="M 119 96 L 119 168" label="HTTPS to any node" labelX={132} labelY={140} />
      <Step x={100} y={132} n={1} />

      <Box x={400} y={24} w={220} h={92} title="Redis route registry" lines={['d042 → node B', 'node, boot id, generation']} tone="cream" />

      <rect className="dg-frame" x={24} y={170} width={300} height={290} rx={14} />
      <text className="dg-frame-label" x={44} y={196}>
        Node A · ingress
      </text>
      <Box x={44} y={210} w={260} h={76} title="Route cache" lines={['1 s entries, refreshed while used', 'up to 1,024 routes']} tone="blue" />
      <Box x={44} y={300} w={260} h={76} title="Peer pool" lines={['4 QUIC connections per owner', 'each with its own UDP socket']} tone="blue" />
      <Box x={44} y={390} w={260} h={56} title="Local drives (if any)" lines={[]} tone="paper" />

      <rect className="dg-frame" x={676} y={170} width={300} height={290} rx={14} />
      <text className="dg-frame-label" x={696} y={196}>
        Node B · owner of d042
      </text>
      <Box x={696} y={210} w={260} h={76} title="Peer listener" lines={['mutual TLS, SPKI-pinned peers', 'from the membership file']} tone="green" />
      <Box x={696} y={300} w={260} h={76} title="Owner checks" lines={['token · route · fencing generation']} tone="green" />
      <Box x={696} y={390} w={260} h={56} title="d042 namespace in memory" lines={[]} tone="green" />

      <Arrow d="M 304 248 C 360 200 380 140 400 100" dashed label="lookup on a miss" labelX={380} labelY={196} />
      <Step x={326} y={224} n={2} />
      <Arrow d="M 696 246 C 650 190 640 140 622 100" dashed label="registers its drives" labelX={700} labelY={150} />

      <Arrow d="M 304 324 L 694 324" strong label="HTTP/3 over QUIC, request streamed" labelX={500} labelY={312} anchor="middle" />
      <Step x={500} y={344} n={3} />
      <Arrow d="M 694 420 L 306 420" label="response streamed back" labelX={500} labelY={410} anchor="middle" />
      <Step x={500} y={440} n={4} />

      <rect className="dg-frame" x={24} y={490} width={300} height={124} rx={14} />
      <text className="dg-frame-label" x={44} y={516}>
        Node C · owns other drives
      </text>
      <text className="dg-sub" x={44} y={544}>
        same peer pool and registry;
      </text>
      <text className="dg-sub" x={44} y={562}>
        A forwards d017 here instead
      </text>

      <Box
        x={350}
        y={512}
        w={300}
        h={102}
        title="Shared storage"
        lines={['TiDB metadata · RustFS / S3 blocks', 'one writer lease per drive', 'fences stop a stale owner writing']}
        tone="paper"
      />
      <Arrow d="M 780 460 L 654 552" label="commits" labelX={724} labelY={520} />
    </Figure>
  )
}

const cacheLayers: { name: string; where: [string, string]; saves: [string, string]; tone: Tone }[] = [
  {
    name: 'Route cache',
    where: ['Ingress, per drive', '1 s entries, refreshed while in use'],
    saves: ['a Redis read before each', 'forwarded request'],
    tone: 'blue',
  },
  {
    name: 'Owner serving window',
    where: ['Owner, per drive', '240 s lease, renewed every 30 s'],
    saves: ['a lease round trip to the', 'metadata store per operation'],
    tone: 'blue',
  },
  {
    name: 'In-memory namespace',
    where: ['Owner, per drive', 'paths, attributes, block lists'],
    saves: ['a database query for', 'stat, head and list'],
    tone: 'green',
  },
  {
    name: 'Covering revision index',
    where: ['TiDB', 'revision, mode and lease columns'],
    saves: ['reading the full namespace', 'row to see if it changed'],
    tone: 'green',
  },
  {
    name: 'RAM block cache',
    where: ['Owner', 'bounded by bytes and entries'],
    saves: ['an object-store GET', 'for hot file bytes'],
    tone: 'cream',
  },
  {
    name: 'Disk block cache',
    where: ['Owner', 'scoped to the volume generation'],
    saves: ['an object-store GET after', 'RAM eviction or a restart'],
    tone: 'cream',
  },
  {
    name: 'Object store',
    where: ['RustFS · S3 · R2 · local files', 'authoritative, immutable blocks'],
    saves: ['the source of truth;', 'every cache above fills from it'],
    tone: 'paper',
  },
]

export function CacheDiagram() {
  const rowHeight = 76
  const top = 50
  const whereX = 380
  const savesX = 690
  return (
    <Figure
      id="dg-cache"
      title="Caching layers from the request down to storage"
      caption="Each layer removes a round trip from the common path. Blocks are immutable and content-identified, so a cached block never goes stale; the namespace and lease layers are owned by one fenced writer, so their memory copies are authoritative while the lease holds."
      viewBox={`0 0 1000 ${top + cacheLayers.length * rowHeight + 10}`}
      minWidth="wide"
    >
      <text className="dg-label" x={46} y={30}>
        layer
      </text>
      <text className="dg-label" x={whereX} y={30}>
        where it lives
      </text>
      <text className="dg-label" x={savesX} y={30}>
        what a hit saves
      </text>
      <path className="dg-arrow" d={`M 12 ${top + 8} L 12 ${top + cacheLayers.length * rowHeight - 18}`} markerEnd="url(#dg-head)" />
      {cacheLayers.map((layer, index) => {
        const y = top + index * rowHeight
        return (
          <g key={layer.name}>
            <rect className={`dg-box dg-${layer.tone}`} x={28} y={y} width={960} height={rowHeight - 12} rx={10} />
            <text className="dg-tag" x={46} y={y + 24}>
              {String(index + 1).padStart(2, '0')}
            </text>
            <text className="dg-title" x={46} y={y + 44}>
              {layer.name}
            </text>
            {layer.where.map((line, lineIndex) => (
              <text key={line} className={lineIndex === 0 ? 'dg-sub dg-sub-strong' : 'dg-sub'} x={whereX} y={y + 27 + lineIndex * 18}>
                {line}
              </text>
            ))}
            {layer.saves.map((line, lineIndex) => (
              <text key={line} className="dg-sub" x={savesX} y={y + 27 + lineIndex * 18}>
                {line}
              </text>
            ))}
          </g>
        )
      })}
    </Figure>
  )
}

export function WritePathDiagram() {
  return (
    <Figure
      id="dg-write"
      title="Durable and writeback mutation paths"
      caption="Durable drives acknowledge only after the blocks and one fenced metadata commit are complete. Writeback drives acknowledge from memory and publish afterwards, trading durability at acknowledgement for sub-millisecond mutations."
      viewBox="0 0 1000 380"
      minWidth="wide"
    >
      <text className="dg-frame-label" x={24} y={34}>
        Durable (default)
      </text>
      <Box x={24} y={50} w={160} h={84} title="Request" lines={['PUT · mkdir', 'rename · delete']} tone="lilac" />
      <Arrow d="M 184 92 L 210 92" />
      <Box x={212} y={50} w={180} h={84} title="Write blocks" lines={['streamed as they', 'arrive; immutable']} tone="cream" />
      <Arrow d="M 392 92 L 418 92" />
      <Box x={420} y={50} w={170} h={84} title="Block barrier" lines={['blocks durable', 'before any reference']} tone="cream" />
      <Arrow d="M 590 92 L 616 92" />
      <Box x={618} y={50} w={200} h={84} title="One fenced commit" lines={['revision CAS + writer', 'fence, one statement']} tone="green" />
      <Arrow d="M 818 92 L 844 92" strong />
      <Box x={846} y={50} w={130} h={84} title="200 OK" lines={['durable']} tone="ink" />

      <text className="dg-frame-label" x={24} y={194}>
        Writeback (exclusive ownership)
      </text>
      <Box x={24} y={210} w={160} h={84} title="Request" lines={['mkdir · rename', 'delete · write']} tone="lilac" />
      <Arrow d="M 184 252 L 210 252" />
      <Box x={212} y={210} w={180} h={84} title="Apply in memory" lines={['owner namespace', 'under its lease']} tone="green" />
      <Arrow d="M 392 252 L 418 252" strong />
      <Box x={420} y={210} w={170} h={84} title="200 OK" lines={['not yet durable']} tone="ink" />
      <Arrow d="M 590 252 L 616 252" dashed />
      <Box x={618} y={210} w={358} h={84} title="Publish later" lines={['blocks, barrier, then the same fenced', 'commit, on sync or drain']} tone="paper" />

      <text className="dg-sub" x={24} y={340}>
        Both paths fail closed: if the lease or fence is lost, the owner stops accepting operations and a replacement reloads
      </text>
      <text className="dg-sub" x={24} y={358}>
        authoritative metadata. An uncertain commit is reported as an error and never replayed.
      </text>
    </Figure>
  )
}
