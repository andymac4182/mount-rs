import { createFileRoute } from '@tanstack/react-router'
import { TransportPage, transportSpecs } from '../../../components/transport-doc'

export const Route = createFileRoute('/docs/transports/9p')({
  head: () => ({ meta: [{ title: '9P2000.L transport — mount-rs docs' }] }),
  component: () => <TransportPage transport={transportSpecs['9p']} />,
})
