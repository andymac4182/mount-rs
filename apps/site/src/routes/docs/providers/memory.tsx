import { createFileRoute } from '@tanstack/react-router'
import { ProviderPage, providerSpecs } from '../../../components/provider-doc'

export const Route = createFileRoute('/docs/providers/memory')({
  head: () => ({ meta: [{ title: 'Memory / memfs — mount-rs docs' }] }),
  component: () => <ProviderPage provider={providerSpecs.memory} />,
})
