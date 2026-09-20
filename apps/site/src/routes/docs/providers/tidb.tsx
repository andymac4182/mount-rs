import { createFileRoute } from '@tanstack/react-router'
import { ProviderPage, providerSpecs } from '../../../components/provider-doc'

export const Route = createFileRoute('/docs/providers/tidb')({
  head: () => ({ meta: [{ title: 'TiDB provider — mount-rs docs' }] }),
  component: () => <ProviderPage provider={providerSpecs.tidb} />,
})
