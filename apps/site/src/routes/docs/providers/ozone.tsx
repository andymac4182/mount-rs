import { createFileRoute } from '@tanstack/react-router'
import { ProviderPage, providerSpecs } from '../../../components/provider-doc'

export const Route = createFileRoute('/docs/providers/ozone')({
  head: () => ({ meta: [{ title: 'Apache Ozone — mount-rs docs' }] }),
  component: () => <ProviderPage provider={providerSpecs.ozone} />,
})
