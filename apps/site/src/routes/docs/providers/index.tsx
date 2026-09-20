import { createFileRoute } from '@tanstack/react-router'
import { ProviderIndex } from '../../../components/provider-doc'

export const Route = createFileRoute('/docs/providers/')({
  component: ProviderIndex,
})
