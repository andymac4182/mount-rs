export function SiteFooter() {
  return (
    <footer className="site-footer">
      <div className="page-frame footer-grid">
        <div>
          <p className="footer-label">mount-rs / Apache-2.0</p>
          <p className="footer-note">
            A Rust filesystem contract and storage composition layer for
            processes that cannot—or should not—mount a filesystem.
          </p>
        </div>
        <nav className="footer-links" aria-label="Project links">
          <a href="https://github.com/andymac4182/mount-rs/blob/main/LICENSE">
            License
          </a>
          <a href="https://github.com/andymac4182/mount-rs/blob/main/README.md">
            README
          </a>
          <a href="https://github.com/andymac4182/mount-rs/blob/main/PORTING_STATUS.md">
            Porting status
          </a>
          <a href="https://github.com/andymac4182/mount-rs/blob/main/REQUIREMENTS.md">
            Requirements
          </a>
          <a href="https://github.com/andymac4182/mount-rs/blob/main/WORK_TRACKER.md">
            Work tracker
          </a>
          <a href="https://github.com/andymac4182/mount-rs/issues">Issues</a>
        </nav>
      </div>
    </footer>
  )
}
