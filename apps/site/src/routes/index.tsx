import { Link, createFileRoute } from '@tanstack/react-router'
import { CodeBlock } from '../components/code-block'

export const Route = createFileRoute('/')({
  head: () => ({
    meta: [
      { title: 'mount-rs — the file boundary for modern applications' },
      {
        name: 'description',
        content:
          'Build file features without making a platform mount, metadata database, or object store your application boundary.',
      },
    ],
  }),
  component: HomePage,
})

function HomePage() {
  return (
    <>
      <section className="hero-section business-hero">
        <div className="page-frame hero-grid">
          <div className="hero-copy">
            <p className="eyebrow eyebrow-light">The file boundary for modern applications</p>
            <h1>
              Files are simple.
              <span> Storage boundaries are not.</span>
            </h1>
            <p className="hero-lede">
              When your application needs file semantics, mount-rs gives you
              one filesystem-shaped contract without forcing your product to
              depend on a kernel mount, a single database, or one object store.
            </p>
            <div className="button-row">
              <a className="button button-warm" href="#how-it-works">See how it works</a>
              <Link className="button button-ghost" to="/docs">Read the technical guide</Link>
            </div>
            <p className="hero-footnote">Open source · Apache-2.0 · prerelease · evidence-led</p>
          </div>
          <div className="business-diagram" aria-label="Application, mount-rs, and explicit storage planes">
            <p className="business-diagram-kicker">one contract / explicit choices</p>
            <div className="business-diagram-node business-diagram-app">
              <span className="diagram-index">01</span>
              <strong>Your application</strong>
              <small>reads · writes · file semantics</small>
            </div>
            <div className="business-diagram-arrow" aria-hidden="true">↓</div>
            <div className="business-diagram-node business-diagram-core">
              <span className="diagram-index">02</span>
              <strong>mount-rs contract</strong>
              <small>Rust core · Node bindings · mount-free</small>
            </div>
            <div className="business-diagram-arrow" aria-hidden="true">↓</div>
            <div className="business-diagram-split">
              <div><span className="diagram-index">03A</span><strong>Metadata</strong><small>namespace · revisions · leases</small></div>
              <div><span className="diagram-index">03B</span><strong>Immutable blocks</strong><small>chunks · bytes · barriers</small></div>
            </div>
          </div>
        </div>
      </section>

      <section className="page-frame problem-section" id="problem">
        <div className="section-heading split-heading">
          <div>
            <p className="eyebrow">The problem</p>
            <h2>File features turn infrastructure assumptions into product risk.</h2>
          </div>
          <p>
            A mount can work on one laptop and still be the wrong boundary for
            a container, serverless process, desktop app, or Node service. The
            hard part is not reading a file. It is keeping namespace, bytes,
            lifecycles, and deployment choices coherent.
          </p>
        </div>
        <div className="problem-grid">
          <article className="problem-card"><span className="problem-card-number">01 / deployment</span><h3>Mounts are a runtime dependency.</h3><p>FUSE, NFS, FSKit, permissions, daemons, and platform-specific setup become part of the application you are trying to ship.</p></article>
          <article className="problem-card"><span className="problem-card-number">02 / correctness</span><h3>Metadata and bytes do different jobs.</h3><p>Namespace and revisions need publication and fencing. Object or block storage needs immutable bytes and durable barriers. Mixing the two makes failure behavior difficult to reason about.</p></article>
          <article className="problem-card"><span className="problem-card-number">03 / change</span><h3>Infrastructure choices become API choices.</h3><p>Switching from local files to SQLite, a database service, or an object store should not require rewriting every file operation in the product.</p></article>
        </div>
      </section>

      <section className="ink-section solution-section" id="how-it-works">
        <div className="page-frame">
          <div className="section-heading solution-heading">
            <p className="eyebrow eyebrow-light">The mount-rs answer</p>
            <h2>One observable file contract. Three decisions you can change independently.</h2>
            <p>mount-rs keeps the core small and makes the boundary visible: choose how your process talks to files, where namespace metadata lives, and where immutable blocks are stored.</p>
          </div>
          <div className="solution-grid">
            <article className="solution-card"><span className="solution-card-number">01</span><h3>Contract</h3><p>Use paths, handles, stats, directory operations, and explicit sync behavior from Rust or Node.</p><Link to="/docs/rust">Explore the core API <span aria-hidden="true">→</span></Link></article>
            <article className="solution-card"><span className="solution-card-number">02</span><h3>Storage planes</h3><p>Compose a metadata provider with an immutable block provider, then flush blocks before publishing references.</p><Link to="/docs/providers">Compare providers <span aria-hidden="true">→</span></Link></article>
            <article className="solution-card"><span className="solution-card-number">03</span><h3>Edge</h3><p>Add a mount or transport only where the environment needs one. The application contract stays the same.</p><Link to="/docs/transports">Choose a transport <span aria-hidden="true">→</span></Link></article>
          </div>
        </div>
      </section>

      <section className="page-frame use-section" id="use-mount-rs">
        <div className="section-heading split-heading">
          <div><p className="eyebrow">How to use it</p><h2>Start local. Keep the boundary when the deployment changes.</h2></div>
          <p>Begin with a direct loopback contract, choose the storage planes your workload needs, and add a transport only at the edge. The same sequence works while the evidence grows.</p>
        </div>
        <div className="step-grid">
          <article className="step-card"><span className="step-number">01</span><h3>Define the file contract</h3><p>Give your application file operations without requiring a kernel mount.</p></article>
          <article className="step-card"><span className="step-number">02</span><h3>Select the planes</h3><p>Keep namespace and immutable bytes explicit so each provider has one job.</p></article>
          <article className="step-card"><span className="step-number">03</span><h3>Add the edge you need</h3><p>Expose FUSE, NFS, HTTP, WebDAV, 9P, or a direct binding when the environment calls for it.</p></article>
        </div>
        <div className="use-code-grid">
          <CodeBlock label="Rust / direct contract">{`let fs = Loopback::new(MemoryFs::new(MemoryOptions::default()));
fs.write_file("/hello", b"hello").await?;
let bytes = fs.read_file("/hello").await?;`}</CodeBlock>
          <CodeBlock label="Node / split-store factory">{`const fs = await createChunkedDriver({
  metadata: { kind: 'sqlite', uri: './metadata.sqlite' },
  blocks: { kind: 'sqlite', uri: './blocks.sqlite' },
  chunkSize: 65536,
});
await fs.writeFile('/hello', Buffer.from('hello'));`}</CodeBlock>
        </div>
        <div className="use-links"><Link className="button button-primary" to="/docs/rust">Start with Rust</Link><Link className="button button-secondary" to="/docs/node">Use Node</Link><Link className="text-link" to="/downloads">Download the CLI preview <span aria-hidden="true">↗</span></Link></div>
      </section>

      <section className="page-frame value-section">
        <div className="value-strip">
          <div><p className="eyebrow">What changes for your team</p><h2>Infrastructure can move without rewriting the product boundary.</h2></div>
          <div className="value-list"><div><strong>Ship sooner</strong><span>Use memory or local providers while the product takes shape.</span></div><div><strong>Change deliberately</strong><span>Move metadata, blocks, or edge transport as separate decisions.</span></div><div><strong>Explain the risk</strong><span>Read provider maturity and acceptance evidence before production claims.</span></div></div>
        </div>
      </section>

      <section className="page-frame status-section">
        <div className="section-heading"><p className="eyebrow">Proof, not promises</p><h2>Know what is usable now—and what still needs qualification.</h2><p className="section-lede">mount-rs is a prerelease project. The docs separate repository implementation, local tests, and live provider evidence so you can make an informed adoption decision.</p></div>
        <div className="status-grid">
          <div className="status-column status-tested"><div className="status-heading"><span className="status-check">✓</span><h3>In the tree</h3></div><ul><li>Core Rust <code>FsDriver</code> and <code>FileHandle</code> contracts.</li><li>Independent metadata and immutable block provider APIs.</li><li>Rust and Node entry points plus transport integrations.</li></ul></div>
          <div className="status-column status-planned"><div className="status-heading"><span className="status-pending">→</span><h3>Still being qualified</h3></div><ul><li>Windows runtime qualification and revision-matched platform CI.</li><li>Durability, failure recovery, broader provider lanes, and native mount gates.</li><li>Production operations, release coverage, and deployment-specific guarantees.</li></ul></div>
        </div>
        <div className="status-links"><Link to="/docs/providers">Provider maturity <span aria-hidden="true">↗</span></Link><a href="https://github.com/andymac4182/mount-rs/blob/main/PORTING_STATUS.md">Read porting evidence <span aria-hidden="true">↗</span></a><a href="https://github.com/andymac4182/mount-rs/blob/main/REQUIREMENTS.md">Read requirements <span aria-hidden="true">↗</span></a></div>
      </section>

      <section className="business-cta"><div className="page-frame business-cta-inner"><p className="eyebrow eyebrow-light">Make the boundary explicit</p><h2>Build the file feature. Keep your infrastructure options open.</h2><div className="button-row"><Link className="button button-warm" to="/docs">Read the guide</Link><a className="button button-ghost" href="https://github.com/andymac4182/mount-rs">View the source <span aria-hidden="true">↗</span></a></div></div></section>
    </>
  )
}
