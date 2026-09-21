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
            <p className="eyebrow eyebrow-light">File infrastructure for modern applications</p>
            <h1>
              Build file features.
              <span> Keep the storage boundary changeable.</span>
            </h1>
            <p className="hero-lede">
              When your product needs paths, handles, directories, and sync
              semantics, mount-rs gives you a stable Rust and Node contract for
              direct access—or a real mounted filesystem through FUSE, NFS, 9P,
              or FSKit. HTTP and WebDAV provide network edges when a mount is
              not the right fit, while namespace metadata and immutable blocks
              stay explicit and replaceable.
            </p>
            <div className="button-row">
              <a className="button button-warm" href="#storage-model">See the storage model</a>
              <a className="button button-ghost" href="#use-mount-rs">Start with the workflow</a>
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
              <small>Rust core · Node bindings · mountable edges</small>
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
            <p>mount-rs keeps the core small and makes the boundary visible: use the API directly, create a real mount for a host, or put HTTP/WebDAV at the edge. Choose where namespace metadata lives and where immutable blocks are stored independently.</p>
          </div>
          <div className="solution-grid">
            <article className="solution-card"><span className="solution-card-number">01</span><h3>Contract</h3><p>Use paths, handles, stats, directory operations, and explicit sync behavior from Rust or Node.</p><Link to="/docs/rust">Explore the core API <span aria-hidden="true">→</span></Link></article>
            <article className="solution-card"><span className="solution-card-number">02</span><h3>Storage planes</h3><p>Compose a metadata provider with an immutable block provider, then flush blocks before publishing references.</p><Link to="/docs/providers">Compare providers <span aria-hidden="true">→</span></Link></article>
            <article className="solution-card"><span className="solution-card-number">03</span><h3>Mounts and transports</h3><p>Create a real filesystem mount with FUSE, NFS, 9P, or FSKit where the platform integration is available; use HTTP or WebDAV when the edge is networked.</p><Link to="/docs/transports">Choose the edge <span aria-hidden="true">→</span></Link></article>
          </div>
        </div>
      </section>

      <section className="page-frame storage-model-section" id="storage-model">
        <div className="section-heading split-heading">
          <div>
            <p className="eyebrow">The storage model</p>
            <h2>See what happens to one file.</h2>
          </div>
          <p>
            The same file operation crosses a namespace plane and an immutable
            block plane. This conceptual view makes the ordering visible
            without pretending every provider uses the same physical schema.
          </p>
        </div>
        <div className="storage-model-grid">
          <div className="storage-model-record">
            <div className="storage-model-record-header">
              <div>
                <p className="storage-model-record-kicker">Illustrative provider view</p>
                <strong><code>/reports/q3.pdf</code></strong>
              </div>
              <span className="storage-model-badge">schemas vary by provider</span>
            </div>
            <CodeBlock label="Conceptual file record">{`namespace:
  path: /reports/q3.pdf
  revision: 7
  size_bytes: 131072
  blocks: [block_01, block_02]

block_01:
  key: files/7/block_01
  bytes: 65536
  immutable: true`}</CodeBlock>
            <p className="storage-model-note">
              Metadata carries inode attributes, directory entries, and block
              references. It does not carry the file bytes.
            </p>
          </div>
          <ol className="storage-model-timeline">
            <li className="storage-model-step">
              <span className="storage-model-step-index">01</span>
              <div>
                <p className="storage-model-step-meta">Namespace / metadata</p>
                <h3>Describe the file before publishing it.</h3>
                <p>
                  Keep inode attributes, directory entries, and block
                  references in the metadata contract. The namespace points to
                  bytes; it does not become the byte store.
                </p>
              </div>
            </li>
            <li className="storage-model-step">
              <span className="storage-model-step-index">02</span>
              <div>
                <p className="storage-model-step-meta">Immutable blocks</p>
                <h3>Write fixed-size chunks and finish the block barrier.</h3>
                <p>
                  Chunk the file according to its layout, write immutable
                  blocks, and wait for the block-store barrier before metadata
                  can refer to them.
                </p>
              </div>
            </li>
            <li className="storage-model-step">
              <span className="storage-model-step-index">03</span>
              <div>
                <p className="storage-model-step-meta">Publish</p>
                <h3>Commit the reference with a fence.</h3>
                <p>
                  Publish with <code>revision CAS</code> and a monotonic writer
                  fence, then complete the metadata barrier before the write is
                  acknowledged.
                </p>
              </div>
            </li>
          </ol>
        </div>
        <p className="storage-model-warning">
          Operational boundary: ownership loss fails closed. An uncertain
          commit can leave unreferenced blocks; automatic GC and copy-on-write
          are not implemented yet.
        </p>
      </section>

      <section className="page-frame use-section" id="use-mount-rs">
        <div className="section-heading split-heading">
          <div><p className="eyebrow">How to use it</p><h2>Start local. Keep the boundary when the deployment changes.</h2></div>
          <p>Begin with a direct loopback contract, choose the storage planes your workload needs, then either create a real mount for the host or expose a network transport. The application contract stays the same while the edge changes.</p>
        </div>
        <div className="step-grid">
          <article className="step-card"><span className="step-number">01</span><h3>Define the file contract</h3><p>Give your application file operations without requiring a kernel mount.</p></article>
          <article className="step-card"><span className="step-number">02</span><h3>Select the planes</h3><p>Keep namespace and immutable bytes explicit so each provider has one job.</p></article>
          <article className="step-card"><span className="step-number">03</span><h3>Mount or transport it</h3><p>Create a real mount through FUSE, NFS, 9P, or FSKit when the host needs a filesystem; choose HTTP or WebDAV for a network edge.</p></article>
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
        <div className="use-links"><Link className="button button-primary" to="/docs/rust">Start with Rust</Link><Link className="button button-secondary" to="/docs/node">Use Node</Link><Link className="button button-secondary" to="/docs/transports">Mounts and transports</Link><Link className="text-link" to="/downloads">Download the CLI preview <span aria-hidden="true">↗</span></Link></div>
      </section>

      <section className="page-frame fit-section" id="fit">
        <div className="section-heading split-heading">
          <div>
            <p className="eyebrow">Choose the boundary</p>
            <h2>Use the storage model that fits the workload.</h2>
          </div>
          <p>
            mount-rs is a choice for teams that want file semantics without
            making a platform mount or one storage service the product API.
            It is not the right abstraction for every workload.
          </p>
        </div>
        <div className="fit-grid">
          <article className="fit-card fit-card-primary">
            <span className="fit-card-label">Choose mount-rs when</span>
            <h3>Your product needs files, but infrastructure still needs to move.</h3>
            <ul>
              <li>Paths, handles, directory operations, and sync semantics belong in the product.</li>
              <li>Metadata and immutable blocks need separate operational roles.</li>
              <li>Rust, Node, and edge transports should share one contract.</li>
            </ul>
            <Link to="/docs">Read the adoption guide <span aria-hidden="true">→</span></Link>
          </article>
          <article className="fit-card">
            <span className="fit-card-label">Use direct object APIs when</span>
            <h3>The application is object-native.</h3>
            <p>
              If callers already work in buckets, keys, immutable objects, and
              provider-native lifecycle rules, adding filesystem semantics may
              create more surface area than it removes.
            </p>
            <a href="https://github.com/andymac4182/mount-rs/blob/main/ARCHITECTURE.md">Review the storage roles <span aria-hidden="true">↗</span></a>
          </article>
          <article className="fit-card">
            <span className="fit-card-label">Use a real mount when</span>
            <h3>Existing tools expect a mounted path.</h3>
            <p>
              FUSE, NFS, 9P, and FSKit put the filesystem surface back into a
              host when users or existing software expect a mounted path.
              Platform permissions, daemon lifecycle, and qualification remain
              part of that choice.
            </p>
            <Link to="/docs/transports">Compare transports <span aria-hidden="true">→</span></Link>
          </article>
        </div>
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
