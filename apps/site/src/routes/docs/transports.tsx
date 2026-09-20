import { Outlet, createFileRoute } from '@tanstack/react-router'

export const Route = createFileRoute('/docs/transports')({
  head: () => ({
    meta: [
      { title: 'Transports — mount-rs docs' },
      {
        name: 'description',
        content:
          'Mount-free and native transport boundaries, platform requirements, maturity labels, and current limitations for mount-rs.',
      },
    ],
  }),
  component: TransportsLayout,
})

function TransportsLayout() {
  return <Outlet />
}
