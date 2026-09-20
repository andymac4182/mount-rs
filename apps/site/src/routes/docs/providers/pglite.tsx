import { createFileRoute } from '@tanstack/react-router'
import { ProviderPage, providerSpecs } from '../../../components/provider-doc'

export const Route = createFileRoute('/docs/providers/pglite')({
  head: () => ({ meta: [{ title: 'PGlite provider — mount-rs docs' }] }),
  component: () => <ProviderPage provider={providerSpecs.pglite} />,
})
