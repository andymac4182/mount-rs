import { createFileRoute } from '@tanstack/react-router'
import { TransportPage, transportSpecs } from '../../../components/transport-doc'

export const Route = createFileRoute('/docs/transports/nfs')({
  head: () => ({ meta: [{ title: 'NFS transport — mount-rs docs' }] }),
  component: () => <TransportPage transport={transportSpecs.nfs} />,
})
