/// <reference types="vite/client" />

import type { ReactNode } from 'react'
import {
  HeadContent,
  Link,
  Outlet,
  Scripts,
  createRootRoute,
} from '@tanstack/react-router'
import type { ErrorComponentProps } from '@tanstack/react-router'
import appCss from '../styles/app.css?url'
import { SiteFooter } from '../components/site-footer'
import { SiteHeader } from '../components/site-header'

export const Route = createRootRoute({
  head: () => ({
    meta: [
      { charSet: 'utf-8' },
      {
        name: 'viewport',
        content: 'width=device-width, initial-scale=1',
      },
      {
        title: 'mount-rs — storage at the boundary',
      },
      {
        name: 'description',
        content:
          'A Rust and Node filesystem contract for separate metadata, immutable blocks, and mount-free storage.',
      },
      { name: 'theme-color', content: '#14243b' },
    ],
    links: [
      { rel: 'stylesheet', href: appCss },
      { rel: 'icon', type: 'image/svg+xml', href: '/favicon.svg' },
    ],
  }),
  notFoundComponent: NotFound,
  errorComponent: RouteError,
  component: RootComponent,
})

function RootComponent() {
  return (
    <RootDocument>
      <a className="skip-link" href="#main-content">
        Skip to content
      </a>
      <SiteHeader />
      <main id="main-content">
        <Outlet />
      </main>
      <SiteFooter />
    </RootDocument>
  )
}

function RootDocument({ children }: Readonly<{ children: ReactNode }>) {
  return (
    <html lang="en">
      <head>
        <HeadContent />
      </head>
      <body>{children}<Scripts /></body>
    </html>
  )
}

function NotFound() {
  return (
    <section className="page-frame empty-state">
      <p className="eyebrow">404 / not found</p>
      <h1>That path is not in the namespace.</h1>
      <p>Try the home page or browse the API guide.</p>
      <div className="button-row">
        <Link className="button button-primary" to="/">
          Return home
        </Link>
        <Link className="button button-secondary" to="/docs">
          Read the docs
        </Link>
      </div>
    </section>
  )
}

function RouteError({ error }: ErrorComponentProps) {
  void error

  return (
    <section className="page-frame empty-state">
      <p className="eyebrow">Request error</p>
      <h1>The route could not be rendered.</h1>
      <p>Something went wrong while rendering this page. Try again or browse the API guide.</p>
      <div className="button-row">
        <Link className="button button-primary" to="/">
          Return home
        </Link>
        <Link className="button button-secondary" to="/docs">
          Read the docs
        </Link>
      </div>
    </section>
  )
}
