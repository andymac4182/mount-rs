import { Link } from '@tanstack/react-router'

export function SiteHeader() {
  return (
    <header className="site-header">
      <div className="page-frame header-inner">
        <Link className="brand" to="/" aria-label="mount-rs home">
          <span className="brand-mark" aria-hidden="true">
            <span />
            <span />
          </span>
          <span className="brand-copy">
            <span className="brand-name">mount-rs</span>
            <span className="brand-caption">storage at the boundary</span>
          </span>
        </Link>
        <nav className="primary-nav" aria-label="Primary navigation">
          <Link to="/docs" activeOptions={{ exact: false }}>
            Docs
          </Link>
          <Link to="/downloads" activeOptions={{ exact: true }}>
            Downloads
          </Link>
          <a href="https://github.com/andymac4182/mount-rs">GitHub</a>
          <span className="status-chip">
            <span className="status-dot" aria-hidden="true" />
            prerelease
          </span>
        </nav>
      </div>
    </header>
  )
}
