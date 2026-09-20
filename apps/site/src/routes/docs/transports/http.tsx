import { createFileRoute } from '@tanstack/react-router'
import { TransportPage, transportSpecs } from '../../../components/transport-doc'

export const Route = createFileRoute('/docs/transports/http')({
  head: () => ({ meta: [{ title: 'HTTP multi-drive API — mount-rs docs' }] }),
  component: () => <TransportPage transport={transportSpecs.http} />,
})
