import { createFileRoute } from '@tanstack/react-router'
import { ProviderPage, providerSpecs } from '../../../components/provider-doc'

export const Route = createFileRoute('/docs/providers/foundationdb')({
  head: () => ({ meta: [{ title: 'FoundationDB provider — mount-rs docs' }] }),
  component: () => <ProviderPage provider={providerSpecs.foundationdb} />,
})
