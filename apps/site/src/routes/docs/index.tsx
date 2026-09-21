import { Link, createFileRoute } from '@tanstack/react-router'
import { cliRelease } from '../../content/cli-release'

export const Route = createFileRoute('/docs/')({
  component: DocsOverview,
})

function DocsOverview() {
  return (
    <article className="doc-article">
      <p className="eyebrow">Documentation / start here</p>
      <h1>Turn the file boundary into an explicit design choice.</h1>
      <p className="doc-lede">
        The business case is simple: keep file semantics stable while the
        deployment, metadata provider, block store, or transport changes.
        These pages show the contract behind that promise.
      </p>

      <div className="callout callout-blue">
          <strong>Choose your next decision</strong>
          <p>
            Start with Rust or Node if you are integrating the contract. Choose
            providers when you are designing storage. Choose transports when you
            are deciding how the file surface reaches a process. The acceptance
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

      <section className="cli-artifact-card" aria-labelledby="cli-artifact-heading">
        <div className="cli-artifact-copy">
          <p className="eyebrow">GitHub Release · Preview</p>
          <h2 id="cli-artifact-heading">mount-rs 0.1.0</h2>
          <p>
            The repository's only CLI target, built in release mode and
            published as a GitHub prerelease. This artifact is
            platform-specific and does not imply a complete release matrix or
            native-mount qualification.
          </p>
        </div>
        <div className="cli-artifact-meta">
          <div><span>Target</span><code>{cliRelease.target}</code></div>
          <div><span>Artifact</span><code>{cliRelease.artifact}</code></div>
          <div><span>SHA-256</span><code>{cliRelease.sha256}</code></div>
          <Link className="button button-warm" to="/downloads">
            View downloads
          </Link>
          <a className="cli-artifact-checksums" href={cliRelease.releaseUrl} target="_blank" rel="noreferrer">
            GitHub Release + checksum manifest <span aria-hidden="true">↗</span>
          </a>
        </div>
      </section>

      <div className="source-note">
        <span className="source-note-mark" aria-hidden="true">↗</span>
        <p>
          Read the source alongside this guide: <a href="https://github.com/andymac4182/mount-rs/blob/main/ARCHITECTURE.md">architecture</a>, <a href="https://github.com/andymac4182/mount-rs/blob/main/PORTING_STATUS.md">porting evidence</a>, <a href="https://github.com/andymac4182/mount-rs/blob/main/REQUIREMENTS.md">requirements</a>, and <a href="https://github.com/andymac4182/mount-rs/blob/main/WORK_TRACKER.md">work tracker</a>.
        </p>
      </div>
    </article>
  )
}
