import { useEffect, useState } from 'react'
import { Link } from '@tanstack/react-router'

type ThemePref = 'system' | 'light' | 'dark'

const themeOptions: { pref: ThemePref; label: string; icon: string }[] = [
  { pref: 'system', label: 'Auto', icon: '◐' },
  { pref: 'light', label: 'Light', icon: '☼' },
  { pref: 'dark', label: 'Dark', icon: '☾' },
]

function systemPrefersDark() {
  return window.matchMedia('(prefers-color-scheme: dark)').matches
}

function applyTheme(pref: ThemePref) {
  const dark = pref === 'dark' || (pref === 'system' && systemPrefersDark())
  document.documentElement.dataset.theme = dark ? 'dark' : 'light'
  document.documentElement.dataset.themePref = pref
}

export function SiteHeader() {
  // "system" follows the operating system, including live changes.
  const [pref, setPref] = useState<ThemePref>('system')

  useEffect(() => {
    const stored = document.documentElement.dataset.themePref
    if (stored === 'light' || stored === 'dark') setPref(stored)
  }, [])

  useEffect(() => {
    if (pref !== 'system') return
    const query = window.matchMedia('(prefers-color-scheme: dark)')
    const onChange = () => applyTheme('system')
    query.addEventListener('change', onChange)
    return () => query.removeEventListener('change', onChange)
  }, [pref])

  function choose(next: ThemePref) {
    applyTheme(next)
    try {
      if (next === 'system') localStorage.removeItem('mount-rs-theme')
      else localStorage.setItem('mount-rs-theme', next)
    } catch {}
    setPref(next)
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
          <a href="/#why">Why mount-rs</a>
          <Link to="/architecture" activeOptions={{ exact: true }}>
            Architecture
          </Link>
          <Link to="/benchmarks" activeOptions={{ exact: true }}>
            Benchmarks
          </Link>
          <Link to="/docs" activeOptions={{ exact: false }}>
            Docs
          </Link>
          <Link to="/downloads" activeOptions={{ exact: true }}>
            Downloads
          </Link>
          <a href="https://github.com/andymac4182/mount-rs">GitHub</a>
        </nav>
        <div className="header-tools">
          <div className="theme-switch" role="group" aria-label="Colour theme">
            {themeOptions.map((option) => (
              <button
                key={option.pref}
                type="button"
                aria-pressed={pref === option.pref}
                title={option.pref === 'system' ? 'Follow the system theme' : `${option.label} theme`}
                onClick={() => choose(option.pref)}
              >
                <span aria-hidden="true">{option.icon}</span>
                <span className="theme-switch-label">{option.label}</span>
              </button>
            ))}
          </div>
          <span className="status-chip">
            <span className="status-dot" aria-hidden="true" />
            prerelease
          </span>
        </div>
      </div>
    </header>
  )
}
