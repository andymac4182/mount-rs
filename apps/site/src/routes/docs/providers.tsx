import { Outlet, createFileRoute } from '@tanstack/react-router'

export const Route = createFileRoute('/docs/providers')({
  head: () => ({
    meta: [
      { title: 'Providers — mount-rs docs' },
      {
        name: 'description',
        content:
          'Provider roles, layouts, inspection examples, durability boundaries, and maturity labels for mount-rs storage backends.',
      },
    ],
  }),
  component: ProvidersLayout,
})

function ProvidersLayout() {
  return <Outlet />
}
