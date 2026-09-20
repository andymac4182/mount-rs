import { createFileRoute } from '@tanstack/react-router'
import { ProviderPage, providerSpecs } from '../../../components/provider-doc'

export const Route = createFileRoute('/docs/providers/sqlite')({
  head: () => ({ meta: [{ title: 'SQLite provider — mount-rs docs' }] }),
  component: () => <ProviderPage provider={providerSpecs.sqlite} />,
})
