import { createFileRoute } from '@tanstack/react-router'
import { TransportPage, transportSpecs } from '../../../components/transport-doc'

export const Route = createFileRoute('/docs/transports/fskit')({
  head: () => ({ meta: [{ title: 'macOS FSKit — mount-rs docs' }] }),
  component: () => <TransportPage transport={transportSpecs.fskit} />,
})
