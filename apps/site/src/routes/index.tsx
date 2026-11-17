import { Link, createFileRoute } from '@tanstack/react-router'
import { CodeBlock } from '../components/code-block'
import { StackComposer } from '../components/stack-composer'
import { benchRow } from '../content/benchmarks'

const fmt = (ms: number) => ms.toFixed(2)

export const Route = createFileRoute('/')({
  head: () => ({
    meta: [
      { title: 'mount-rs — a real filesystem for your app, on any storage' },
      {
        name: 'description',
        content:
          'mount-rs gives your product paths, handles and directories through one contract. Use it in-process, mount it, or serve it over HTTP, on the metadata and block stores you choose.',
      },
    ],
  }),
  component: HomePage,
})

const benefits = [
  {
    tone: 'blue',
    kicker: 'No mount required',
    title: 'File semantics inside your process.',
    body: 'Call mount-rs from Rust or Node. There is no kernel module, daemon or root permission between your product and its files.',
    to: '/docs/rust',
    link: 'Explore the API',
  },
  {
    tone: 'lilac',
    kicker: 'Mount it when you want one',
    title: 'The same drive, as a real directory.',
    body: 'Expose a drive through FUSE or NFS for tools that expect a path, or over HTTP and WebDAV when the edge is a network.',
    to: '/docs/transports',
    link: 'Compare the edges',
  },
  {
    tone: 'cream',
    kicker: 'Storage you can swap',
    title: 'SQLite today. TiDB and S3 later.',
    body: 'Names and bytes are separate choices. Move either one to a different store and the file API your product uses does not change.',
    to: '/docs/providers',
    link: 'See every provider',
  },
  {
    tone: 'green',
    kicker: 'Fast where it counts',
    title: 'Metadata in well under a millisecond.',
    body: 'Each drive’s namespace is served from memory by its owner. The numbers come from a benchmark that runs in CI, not from estimates.',
    to: '/benchmarks',
    link: 'Read the benchmarks',
  },
  {
    tone: 'blue',
    kicker: 'Scale out simply',
    title: 'Any server can take any request.',
    body: 'A plain load balancer is enough. A node that does not own the drive forwards the request once, over HTTP/3, to the node that does.',
    to: '/architecture',
    link: 'Watch a request',
  },
  {
    tone: 'lilac',
    kicker: 'Safe by construction',
    title: 'Bytes first, then the name.',
    body: 'Blocks reach storage before metadata refers to them, and every commit carries a writer fence. If ownership is lost, writes fail closed.',
    to: '/architecture',
    link: 'See the write path',
  },
] as const

function HomePage() {
  return (
    <>
      <section className="hero home-hero">
        <div className="page-frame home-hero-grid">
          <div className="home-hero-copy">
            <p className="eyebrow">Open-source file infrastructure · Rust and Node</p>
            <h1>
              A real filesystem for your app. <span className="grad-text">Any storage underneath.</span>
            </h1>
            <p className="home-hero-lede">
              mount-rs gives your product paths, handles and directories through
              one small contract. Use it in-process, mount it with FUSE or NFS,
              or serve it over HTTP. Keep names and file bytes in the stores you
              choose, and change them later without rewriting the product.
            </p>
            <div className="button-row">
              <a className="button button-primary" href="#flexible">Build your stack</a>
              <Link className="button button-secondary" to="/architecture">See how it works</Link>
              <Link className="text-link" to="/downloads">Get the CLI <span aria-hidden="true">→</span></Link>
            </div>
            <dl className="home-hero-stats">
              <div>
                <dt>{fmt(benchRow('durable-refresh', 'forwarded', 'stat').p99)} ms</dt>
                <dd>p99 metadata read, forwarded between servers</dd>
              </div>
              <div>
                <dt>7</dt>
                <dd>ways in: two APIs, three mounts, HTTP and WebDAV</dd>
              </div>
              <div>
                <dt>10</dt>
                <dd>storage providers, from memory to S3</dd>
              </div>
              <div>
                <dt>Apache-2.0</dt>
                <dd>open source, prerelease, evidence-led</dd>
              </div>
            </dl>
          </div>
          <div className="hero-stack" role="img" aria-label="Your application reaches mount-rs through an API, a mount or HTTP; mount-rs keeps metadata and immutable blocks in separate stores">
            <div className="hero-stack-card">
              <span className="hero-stack-kicker">Your application</span>
              <div className="hero-pills">
                <span>Rust</span><span>Node</span><span>FUSE</span><span>NFS</span><span>9P</span><span>HTTP</span><span>WebDAV</span>
              </div>
            </div>
            <span className="hero-stack-flow" aria-hidden="true" />
            <div className="hero-stack-card hero-stack-core">
              <span className="hero-stack-kicker">mount-rs</span>
              <strong>One file contract</strong>
              <small>paths · handles · directories · sync</small>
            </div>
            <span className="hero-stack-flow hero-stack-flow-split" aria-hidden="true" />
            <div className="hero-stack-planes">
              <div className="hero-stack-card">
                <span className="hero-stack-kicker">Metadata</span>
                <div className="hero-pills">
                  <span>SQLite</span><span>TiDB</span><span>FoundationDB</span><span>PGlite</span>
                </div>
              </div>
              <div className="hero-stack-card">
                <span className="hero-stack-kicker">Immutable blocks</span>
                <div className="hero-pills">
                  <span>S3</span><span>R2</span><span>RustFS</span><span>SQLite</span>
                </div>
              </div>
            </div>
          </div>
        </div>
      </section>

      <section className="page-frame why-section" id="why">
        <div className="section-heading split-heading">
          <div>
            <p className="eyebrow">Why mount-rs</p>
            <h2>File features, without the infrastructure lock-in.</h2>
          </div>
          <p>
            A mount can work on one laptop and still be the wrong boundary for a
            container, a serverless process or a desktop app. mount-rs keeps the
            file contract in your code and makes everything behind it a choice.
          </p>
        </div>
        <div className="benefit-grid">
          {benefits.map((benefit) => (
            <article className={`benefit-card benefit-${benefit.tone}`} key={benefit.kicker}>
              <span className="benefit-kicker">{benefit.kicker}</span>
              <h3>{benefit.title}</h3>
              <p>{benefit.body}</p>
              <Link to={benefit.to}>{benefit.link} <span aria-hidden="true">→</span></Link>
            </article>
          ))}
        </div>
      </section>

      <section className="page-frame flexible-section" id="flexible">
        <div className="section-heading split-heading">
          <div>
            <p className="eyebrow">Flexible by design</p>
            <h2>Compose the stack you need today. Change it tomorrow.</h2>
          </div>
          <p>
            Three independent choices: how your application reaches files, where
            the namespace lives, and where the bytes live. Try a few
            combinations.
          </p>
        </div>
        <StackComposer />
      </section>

      <section className="ink-section arch-fast" id="fast">
        <div className="page-frame">
          <div className="section-heading solution-heading">
            <p className="eyebrow eyebrow-light">Fast by design</p>
            <h2>Sub-millisecond metadata, even one server away.</h2>
            <p>
              Each drive has one owner that keeps its namespace in memory. Any
              server can take a request and forward it in one HTTP/3 hop, so a
              load balancer never needs to know where a drive lives.
            </p>
          </div>
          <div className="home-fast-grid">
            <article className="arch-fast-card">
              <span className="solution-card-number">{fmt(benchRow('durable-refresh', 'forwarded', 'stat').p99)} ms</span>
              <h3>Forwarded read p99</h3>
              <p>Durable TiDB drive, 100 drives across three servers.</p>
            </article>
            <article className="arch-fast-card">
              <span className="solution-card-number">{fmt(benchRow('exclusive', 'forwarded', 'rename').p99)} ms</span>
              <h3>Forwarded rename p99</h3>
              <p>Writeback drive: applied in memory, published afterwards.</p>
            </article>
            <article className="arch-fast-card">
              <span className="solution-card-number">{fmt(benchRow('exclusive', 'local', 'stat').p99)} ms</span>
              <h3>Local read p99</h3>
              <p>Answered from the owner’s in-memory namespace.</p>
            </article>
            <article className="arch-fast-card">
              <span className="solution-card-number">1 hop</span>
              <h3>Any node, any drive</h3>
              <p>Routes cached and refreshed in the background; pinned, pooled QUIC peers.</p>
            </article>
          </div>
          <div className="home-fast-links">
            <Link className="button button-warm" to="/architecture">See the architecture</Link>
            <Link className="button button-ghost" to="/benchmarks">Read the benchmarks</Link>
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
        <div className="use-links"><Link className="button button-primary" to="/docs/cli">Start with the CLI</Link><Link className="button button-secondary" to="/docs/rust">Embed with Rust</Link><Link className="button button-secondary" to="/docs/node">Use Node</Link><Link className="button button-secondary" to="/docs/transports">Mounts and transports</Link><Link className="text-link" to="/downloads">Download the CLI preview <span aria-hidden="true">↗</span></Link></div>
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
