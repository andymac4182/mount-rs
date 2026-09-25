import { createFileRoute } from '@tanstack/react-router'
import { ProviderPage, providerSpecs } from '../../../components/provider-doc'

export const Route = createFileRoute('/docs/providers/slatedb')({
  head: () => ({ meta: [{ title: 'SlateDB — mount-rs docs' }] }),
  component: () => <ProviderPage provider={providerSpecs.slatedb} />,
})
