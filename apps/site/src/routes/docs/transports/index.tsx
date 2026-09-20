import { createFileRoute } from '@tanstack/react-router'
import { TransportIndex } from '../../../components/transport-doc'

export const Route = createFileRoute('/docs/transports/')({
  component: TransportIndex,
})
