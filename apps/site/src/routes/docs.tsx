import { Outlet, createFileRoute } from '@tanstack/react-router'
import { DocsNav } from '../components/docs-nav'

export const Route = createFileRoute('/docs')({
  head: () => ({
    meta: [
      { title: 'Docs — mount-rs' },
      {
        name: 'description',
        content:
          'The mount-rs Rust and Node API surface, provider composition model, and current project status.',
      },
    ],
  }),
  component: DocsLayout,
})

function DocsLayout() {
  return (
    <section className="page-frame docs-shell">
      <aside className="docs-sidebar">
        <DocsNav />
      </aside>
      <div className="docs-content">
        <Outlet />
      </div>
    </section>
  )
}
