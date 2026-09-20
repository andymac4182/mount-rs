import { createFileRoute } from '@tanstack/react-router'
import { ProviderPage, providerSpecs } from '../../../components/provider-doc'

export const Route = createFileRoute('/docs/providers/rustfs')({
  head: () => ({ meta: [{ title: 'RustFS — mount-rs docs' }] }),
  component: () => <ProviderPage provider={providerSpecs.rustfs} />,
})
