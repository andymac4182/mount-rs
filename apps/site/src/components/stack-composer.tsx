import { useState } from 'react'
import { Link } from '@tanstack/react-router'

// Pick an edge, a metadata store and a block store to see one mount-rs stack.
// Maturity labels match the provider and transport guides.

type Maturity = 'Validated' | 'Preview' | 'Experimental' | 'Planned' | 'Library'

type Choice = {
  id: string
  name: string
  maturity: Maturity
  note: string
  to: string
}

const edges: Choice[] = [
  { id: 'rust', name: 'Rust API', maturity: 'Library', note: 'call it in-process from Rust, with no mount and no daemon', to: '/docs/rust' },
  { id: 'node', name: 'Node API', maturity: 'Library', note: 'call it in-process from Node through native bindings', to: '/docs/node' },
  { id: 'http', name: 'HTTP API', maturity: 'Preview', note: 'serve many drives over HTTP from one or more servers', to: '/docs/transports/http' },
  { id: 'webdav', name: 'WebDAV', maturity: 'Preview', note: 'let existing WebDAV clients browse and edit files', to: '/docs/transports/webdav' },
  { id: 'fuse', name: 'FUSE', maturity: 'Preview', note: 'mount it as a real directory through the kernel’s FUSE interface', to: '/docs/transports/fuse' },
  { id: 'nfs', name: 'NFS', maturity: 'Preview', note: 'mount it with the operating system’s own NFS client', to: '/docs/transports/nfs' },
  { id: '9p', name: '9P', maturity: 'Experimental', note: 'mount it over the lightweight 9P2000.L protocol', to: '/docs/transports/9p' },
  { id: 'fskit', name: 'FSKit', maturity: 'Planned', note: 'mount it on macOS through Apple’s FSKit extension point', to: '/docs/transports/fskit' },
]

const metadataStores: Choice[] = [
  { id: 'memory', name: 'Memory', maturity: 'Validated', note: 'kept in the process, gone when it exits', to: '/docs/providers/memory' },
  { id: 'sqlite', name: 'SQLite', maturity: 'Validated', note: 'in one local file', to: '/docs/providers/sqlite' },
  { id: 'pglite', name: 'PGlite', maturity: 'Preview', note: 'over the PostgreSQL wire protocol', to: '/docs/providers/pglite' },
  { id: 'tidb', name: 'TiDB', maturity: 'Preview', note: 'in distributed SQL shared by many servers', to: '/docs/providers/tidb' },
  { id: 'foundationdb', name: 'FoundationDB', maturity: 'Experimental', note: 'in a transactional key-value cluster', to: '/docs/providers/foundationdb' },
  { id: 'slatedb', name: 'SlateDB', maturity: 'Experimental', note: 'in a database that itself lives on object storage', to: '/docs/providers/slatedb' },
]

const blockStores: Choice[] = [
  { id: 'memory', name: 'Memory', maturity: 'Validated', note: 'in the process', to: '/docs/providers/memory' },
  { id: 'sqlite', name: 'SQLite', maturity: 'Validated', note: 'in a local SQLite file', to: '/docs/providers/sqlite' },
  { id: 's3', name: 'AWS S3', maturity: 'Validated', note: 'as objects in an S3 bucket', to: '/docs/providers/aws-s3' },
  { id: 'r2', name: 'Cloudflare R2', maturity: 'Validated', note: 'as objects in R2 or another S3-compatible store', to: '/docs/providers/r2' },
  { id: 'rustfs', name: 'RustFS', maturity: 'Validated', note: 'as objects in a self-hosted S3-compatible service', to: '/docs/providers/rustfs' },
  { id: 'pglite', name: 'PGlite', maturity: 'Preview', note: 'as rows over the PostgreSQL wire protocol', to: '/docs/providers/pglite' },
  { id: 'tidb', name: 'TiDB', maturity: 'Preview', note: 'as rows in distributed SQL', to: '/docs/providers/tidb' },
  { id: 'foundationdb', name: 'FoundationDB', maturity: 'Experimental', note: 'as keys in a transactional key-value cluster', to: '/docs/providers/foundationdb' },
  { id: 'ozone', name: 'Apache Ozone', maturity: 'Experimental', note: 'as objects behind Ozone’s S3 gateway', to: '/docs/providers/ozone' },
]

const presets: { label: string; edge: string; metadata: string; blocks: string }[] = [
  { label: 'Prototype on a laptop', edge: 'rust', metadata: 'memory', blocks: 'memory' },
  { label: 'One small server', edge: 'http', metadata: 'sqlite', blocks: 'sqlite' },
  { label: 'Mounted workspace', edge: 'fuse', metadata: 'sqlite', blocks: 's3' },
  { label: 'Multi-server service', edge: 'http', metadata: 'tidb', blocks: 'rustfs' },
]

const find = (list: Choice[], id: string) => list.find((item) => item.id === id) ?? list[0]

function Picker({ legend, list, value, onChange }: { legend: string; list: Choice[]; value: string; onChange: (id: string) => void }) {
  return (
    <fieldset className="seg composer-picker">
      <legend>{legend}</legend>
      {list.map((item) => (
        <button key={item.id} type="button" aria-pressed={value === item.id} onClick={() => onChange(item.id)}>
          {item.name}
        </button>
      ))}
    </fieldset>
  )
}

function Tier({ kicker, choice }: { kicker: string; choice: Choice }) {
  return (
    <div className="composer-tier">
      <span className="composer-tier-kicker">{kicker}</span>
      <strong>{choice.name}</strong>
      <span className={`composer-badge composer-badge-${choice.maturity.toLowerCase()}`}>{choice.maturity}</span>
    </div>
  )
}

export function StackComposer() {
  const [edge, setEdge] = useState('http')
  const [metadata, setMetadata] = useState('tidb')
  const [blocks, setBlocks] = useState('rustfs')
  const e = find(edges, edge)
  const m = find(metadataStores, metadata)
  const b = find(blockStores, blocks)

  return (
    <div className="composer">
      <div className="composer-controls">
        <div className="composer-presets">
          <span>Start from</span>
          {presets.map((preset) => (
            <button
              key={preset.label}
              type="button"
              aria-pressed={preset.edge === edge && preset.metadata === metadata && preset.blocks === blocks}
              onClick={() => {
                setEdge(preset.edge)
                setMetadata(preset.metadata)
                setBlocks(preset.blocks)
              }}
            >
              {preset.label}
            </button>
          ))}
        </div>
        <Picker legend="1 · How your app reaches files" list={edges} value={edge} onChange={setEdge} />
        <Picker legend="2 · Where names and structure live" list={metadataStores} value={metadata} onChange={setMetadata} />
        <Picker legend="3 · Where file bytes live" list={blockStores} value={blocks} onChange={setBlocks} />
      </div>

      <div className="composer-result" aria-live="polite">
        <div className="composer-stack">
          <div className="composer-tier composer-tier-app">
            <span className="composer-tier-kicker">Your application</span>
            <strong>paths · handles · directories</strong>
          </div>
          <span className="composer-link" aria-hidden="true" />
          <Tier kicker="Edge" choice={e} />
          <span className="composer-link" aria-hidden="true" />
          <div className="composer-tier composer-tier-core">
            <span className="composer-tier-kicker">mount-rs</span>
            <strong>one file contract</strong>
          </div>
          <span className="composer-link composer-link-split" aria-hidden="true" />
          <div className="composer-planes">
            <Tier kicker="Metadata" choice={m} />
            <Tier kicker="Blocks" choice={b} />
          </div>
        </div>
        <p className="composer-sentence">
          You {e.note}. Names, sizes and revisions are kept {m.note}. File bytes are stored as
          immutable blocks {b.note}.
        </p>
        <p className="composer-links">
          <a href={e.to}>{e.name} guide</a>
          <a href={m.to}>{m.name} metadata</a>
          <a href={b.to}>{b.name} blocks</a>
        </p>
        <p className="composer-caveat">
          Change any one choice and the other two stay put. Not every pairing has been qualified
          together: the badges show each part’s own maturity, and the{' '}
          <Link to="/docs/providers">provider guide</Link> lists the evidence.
        </p>
      </div>
    </div>
  )
}
