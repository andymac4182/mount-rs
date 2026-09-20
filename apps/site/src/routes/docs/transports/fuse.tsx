import { createFileRoute } from '@tanstack/react-router'
import { TransportPage, transportSpecs } from '../../../components/transport-doc'

export const Route = createFileRoute('/docs/transports/fuse')({
  head: () => ({ meta: [{ title: 'FUSE transport — mount-rs docs' }] }),
  component: () => <TransportPage transport={transportSpecs.fuse} />,
})
