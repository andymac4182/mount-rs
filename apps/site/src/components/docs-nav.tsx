import { Link } from '@tanstack/react-router'

export function DocsNav() {
  return (
    <nav className="docs-nav" aria-label="Documentation navigation">
      <p className="eyebrow">Start here</p>
      <Link to="/docs" activeOptions={{ exact: true }}>
        Overview
      </Link>
      <Link to="/docs/cli" activeOptions={{ exact: true }}>
        CLI / run it
      </Link>
      <Link to="/docs/rust" activeOptions={{ exact: true }}>
        Rust API
      </Link>
      <Link to="/docs/node" activeOptions={{ exact: true }}>
        Node API
      </Link>
      <p className="eyebrow">Understand</p>
      <Link to="/docs/providers" activeOptions={{ exact: false }}>
        Providers
      </Link>
      <Link to="/docs/transports" activeOptions={{ exact: false }}>
        Transports
      </Link>
      <Link to="/downloads" activeOptions={{ exact: true }}>
        CLI downloads
      </Link>
      <p className="eyebrow">Evidence</p>
      <a href="https://github.com/andymac4182/mount-rs/blob/main/ARCHITECTURE.md">
        Architecture in GitHub
      </a>
      <a href="https://github.com/andymac4182/mount-rs/blob/main/PORTING_STATUS.md">
        Porting evidence
      </a>
      <a href="https://github.com/andymac4182/mount-rs/blob/main/REQUIREMENTS.md">
        Acceptance requirements
      </a>
      <a href="https://github.com/andymac4182/mount-rs/blob/main/WORK_TRACKER.md">
        Work tracker
      </a>
    </nav>
  )
}
