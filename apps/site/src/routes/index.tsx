import { Link, createFileRoute } from '@tanstack/react-router'
import { CodeBlock } from '../components/code-block'

export const Route = createFileRoute('/')({
  head: () => ({
    meta: [
      { title: 'mount-rs — storage at the boundary' },
      {
        name: 'description',
        content:
          'Separate metadata from immutable blocks. Use the same filesystem contract from Rust or Node, with or without a mount.',
      },
    ],
  }),
  component: HomePage,
})

function HomePage() {
  return (
    <>
      <section className="hero-section">
        <div className="page-frame hero-grid">
          <div className="hero-copy">
            <p className="eyebrow eyebrow-light">Rust core / Node bindings / mount-free</p>
            <h1>
              Put bytes where your process needs them.
              <span> Keep metadata where your invariants live.</span>
            </h1>
            <p className="hero-lede">
              mount-rs is a filesystem contract and storage composition layer
              for applications that need files without turning a mount into
              their only boundary.
            </p>
            <div className="button-row">
              <Link className="button button-warm" to="/docs">
                Explore the API
              </Link>
              <a
                className="button button-ghost"
                href="https://github.com/andymac4182/mount-rs"
              >
                View source <span aria-hidden="true">↗</span>
              </a>
            </div>
            <p className="hero-footnote">
              Apache-2.0 · prerelease · not release-ready
            </p>
          </div>
          <div className="hero-diagram" aria-label="Separated metadata and block storage diagram">
            <div className="diagram-kicker">one contract / two planes</div>
            <div className="diagram-node diagram-node-top">
              <span className="diagram-index">01</span>
              <strong>metadata</strong>
              <small>namespace · revisions · leases</small>
            </div>
            <div className="diagram-connector" aria-hidden="true"><span /></div>
            <div className="diagram-node diagram-node-bottom">
              <span className="diagram-index">02</span>
              <strong>immutable blocks</strong>
              <small>fixed chunks · bytes · barriers</small>
            </div>
            <div className="diagram-note">
              composed through <code>ChunkedFs</code>
            </div>
          </div>
        </div>
      </section>

      <section className="page-frame intro-section">
        <div className="section-heading split-heading">
          <div>
            <p className="eyebrow">The shape</p>
            <h2>A filesystem API that keeps the storage decision explicit.</h2>
          </div>
          <p>
            The core stays focused on paths, handles, metadata, and observable
            behavior. Backends live in separate crates and can be composed
            independently.
          </p>
        </div>
        <div className="feature-grid">
          <article className="feature-card feature-card-blue">
            <span className="card-number">01</span>
            <h3>Use files without a mount</h3>
            <p>
              Call the same loopback and handle contract directly from Rust or
              the napi-rs Node package when a FUSE, NFS, or FSKit mount is not
              available.
            </p>
            <Link to="/docs/rust">Rust surface <span aria-hidden="true">→</span></Link>
          </article>
          <article className="feature-card feature-card-cream">
            <span className="card-number">02</span>
            <h3>Split metadata from blocks</h3>
            <p>
              <code>MetadataStore</code> publishes namespace and revisions;
              <code> BlockStore</code> owns immutable bytes. The orchestration
              layer flushes blocks before publishing references.
            </p>
            <Link to="/docs">Composition notes <span aria-hidden="true">→</span></Link>
          </article>
          <article className="feature-card feature-card-lilac">
            <span className="card-number">03</span>
            <h3>Keep the boundary portable</h3>
            <p>
              The API is designed for memory, SQLite, PGlite, R2, and future
              providers without putting a cloud SDK or database client in the
              core crate.
            </p>
            <Link to="/docs/node">Node surface <span aria-hidden="true">→</span></Link>
          </article>
        </div>
      </section>

      <section className="ink-section">
        <div className="page-frame api-preview">
          <div className="api-preview-copy">
            <p className="eyebrow eyebrow-light">A small contract</p>
            <h2>Readable in Rust. Familiar from Node.</h2>
            <p>
              Start with the direct API, then choose the transport or provider
              that belongs at the edge of your deployment.
            </p>
            <Link className="text-link text-link-light" to="/docs">
              See the real APIs <span aria-hidden="true">↗</span>
            </Link>
          </div>
          <CodeBlock label="Rust / loopback contract">
            {`use mount_rs::{Loopback, MemoryFs, MemoryOptions};

let fs = Loopback::new(MemoryFs::new(MemoryOptions::default()));
fs.write_file("/hello", b"hello").await?;
let bytes = fs.read_file("/hello").await?;`}
          </CodeBlock>
        </div>
      </section>

      <section className="page-frame status-section">
        <div className="section-heading">
          <p className="eyebrow">Status, in plain language</p>
          <h2>Useful now. Deliberately not called finished.</h2>
          <p className="section-lede">
            This is a prerelease project. The repository is the source of truth
            for evidence, requirements, and remaining work. It is not
            release-ready.
          </p>
        </div>
        <div className="status-grid">
          <div className="status-column status-tested">
            <div className="status-heading"><span className="status-check">✓</span><h3>In the tree</h3></div>
            <ul>
              <li>Core Rust <code>FsDriver</code> and <code>FileHandle</code> contracts.</li>
              <li>Independent metadata and immutable block provider APIs.</li>
              <li>Node bindings expose memory, SQLite, PGlite, and R2 factories; transport integrations are also present.</li>
            </ul>
          </div>
          <div className="status-column status-planned">
            <div className="status-heading"><span className="status-pending">→</span><h3>Still being qualified</h3></div>
            <ul>
              <li>Windows runtime qualification and revision-matched platform CI.</li>
              <li>Remaining live Cloudflare R2 lanes and full mounted-SQLite acceptance.</li>
              <li>FSKit integration, broader API/transport parity, durability, failure-recovery, and benchmark gates.</li>
            </ul>
          </div>
        </div>
        <div className="status-links">
          <a href="https://github.com/andymac4182/mount-rs/blob/main/PORTING_STATUS.md">Read porting evidence <span aria-hidden="true">↗</span></a>
          <a href="https://github.com/andymac4182/mount-rs/blob/main/REQUIREMENTS.md">Read requirements <span aria-hidden="true">↗</span></a>
          <a href="https://github.com/andymac4182/mount-rs/blob/main/WORK_TRACKER.md">Follow the tracker <span aria-hidden="true">↗</span></a>
        </div>
      </section>
    </>
  )
}
