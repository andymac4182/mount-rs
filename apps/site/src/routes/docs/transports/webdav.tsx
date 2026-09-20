import { createFileRoute } from '@tanstack/react-router'
import { TransportPage, transportSpecs } from '../../../components/transport-doc'

export const Route = createFileRoute('/docs/transports/webdav')({
  head: () => ({ meta: [{ title: 'WebDAV transport — mount-rs docs' }] }),
  component: () => <TransportPage transport={transportSpecs.webdav} />,
})
