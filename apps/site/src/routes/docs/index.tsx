import { Link, createFileRoute } from '@tanstack/react-router'

export const Route = createFileRoute('/docs/')({
  component: DocsOverview,
})

function DocsOverview() {
  return (
    <article className="doc-article">
      <p className="eyebrow">Documentation / overview</p>
      <h1>The boundary is the product.</h1>
      <p className="doc-lede">
        mount-rs gives Rust and Node applications a filesystem-shaped contract
        while keeping persistence, chunking, and mounting as explicit choices.
      </p>

      <div className="callout callout-blue">
        <strong>Practical reading order</strong>
        <p>
          Start with the direct contract, then look at composition. The API
          guide describes what exists in this repository today; acceptance
          requirements describe what is not yet a blanket guarantee.
        </p>
      </div>

      <h2>Three layers, one observable contract</h2>
      <div className="layer-list">
        <div className="layer-row">
          <span className="layer-label">Core</span>
          <div><strong><code>FsDriver</code> / <code>FileHandle</code></strong><p>Paths, handles, stats, directory operations, and explicit sync behavior.</p></div>
        </div>
        <div className="layer-row">
          <span className="layer-label">Storage</span>
          <div><strong><code>MetadataStore</code> + <code>BlockStore</code></strong><p>Namespace publication and immutable bytes are independent contracts.</p></div>
        </div>
        <div className="layer-row">
          <span className="layer-label">Edge</span>
          <div><strong>Transports and bindings</strong><p>FUSE, NFS, 9P, WebDAV, S3, auto-mount, CLI, and napi-rs are integration surfaces.</p></div>
        </div>
      </div>

      <h2>What the docs do—and do not—promise</h2>
      <p>
        The project is prerelease. Examples point to real exported types and
        repository paths, but a working example is not proof that every
        backend, platform, journal mode, or failure scenario has passed the
        full acceptance suite. The overall project is in progress and is not
        release-ready.
      </p>
      <div className="doc-card-grid">
        <Link className="doc-card" to="/docs/rust">
          <span className="eyebrow">01 / Rust</span>
          <h3>Core and split-store APIs</h3>
          <p>Loopback access, handles, fixed-size chunking, and provider composition.</p>
          <span className="card-arrow" aria-hidden="true">→</span>
        </Link>
        <Link className="doc-card" to="/docs/node">
          <span className="eyebrow">02 / Node</span>
          <h3>napi-rs factories</h3>
          <p>The current <code>createChunkedDriver</code> shape and lifecycle expectations.</p>
          <span className="card-arrow" aria-hidden="true">→</span>
        </Link>
        <Link className="doc-card" to="/docs/providers">
          <span className="eyebrow">03 / Providers</span>
          <h3>Storage roles and maturity</h3>
          <p>See where metadata lives, how bytes are laid out, and what the evidence actually covers.</p>
          <span className="card-arrow" aria-hidden="true">→</span>
        </Link>
        <Link className="doc-card" to="/docs/transports">
          <span className="eyebrow">04 / Transports</span>
          <h3>Mount-free and native edges</h3>
          <p>Choose FUSE, NFS, 9P, FSKit, HTTP, or WebDAV with platform limits in view.</p>
          <span className="card-arrow" aria-hidden="true">→</span>
        </Link>
      </div>

      <div className="source-note">
        <span className="source-note-mark" aria-hidden="true">↗</span>
        <p>
          Read the source alongside this guide: <a href="https://github.com/andymac4182/mount-rs/blob/main/ARCHITECTURE.md">architecture</a>, <a href="https://github.com/andymac4182/mount-rs/blob/main/PORTING_STATUS.md">porting evidence</a>, <a href="https://github.com/andymac4182/mount-rs/blob/main/REQUIREMENTS.md">requirements</a>, and <a href="https://github.com/andymac4182/mount-rs/blob/main/WORK_TRACKER.md">work tracker</a>.
        </p>
      </div>
    </article>
  )
}
