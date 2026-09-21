import { useEffect, useState } from 'react'
import { Link } from '@tanstack/react-router'

type Theme = 'light' | 'dark'

export function SiteHeader() {
  const [theme, setTheme] = useState<Theme>('light')

  useEffect(() => {
    const currentTheme = document.documentElement.dataset.theme === 'dark' ? 'dark' : 'light'
    setTheme(currentTheme)
  }, [])

  function toggleTheme() {
    const nextTheme: Theme = theme === 'dark' ? 'light' : 'dark'
    document.documentElement.dataset.theme = nextTheme
    try {
      localStorage.setItem('mount-rs-theme', nextTheme)
    } catch {}
    setTheme(nextTheme)
  }

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
          <a href="/#problem">
            Why mount-rs
          </a>
          <a href="/#how-it-works">
            How it works
          </a>
          <Link to="/docs" activeOptions={{ exact: false }}>
            Docs
          </Link>
          <Link to="/downloads" activeOptions={{ exact: true }}>
            Downloads
          </Link>
          <a href="https://github.com/andymac4182/mount-rs">GitHub</a>
          <button
            className="theme-toggle"
            type="button"
            aria-pressed={theme === 'dark'}
            aria-label={`Switch to ${theme === 'dark' ? 'light' : 'dark'} mode`}
            title={`Switch to ${theme === 'dark' ? 'light' : 'dark'} mode`}
            onClick={toggleTheme}
          >
            <span className="theme-toggle-icon" aria-hidden="true">
              {theme === 'dark' ? '☼' : '☾'}
            </span>
            <span>{theme === 'dark' ? 'Light' : 'Dark'} mode</span>
          </button>
          <span className="status-chip">
            <span className="status-dot" aria-hidden="true" />
            prerelease
          </span>
        </nav>
      </div>
    </header>
  )
}
