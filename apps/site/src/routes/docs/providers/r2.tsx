import { createFileRoute } from '@tanstack/react-router'
import { ProviderPage, providerSpecs } from '../../../components/provider-doc'

export const Route = createFileRoute('/docs/providers/r2')({
  head: () => ({ meta: [{ title: 'Cloudflare R2 — mount-rs docs' }] }),
  component: () => <ProviderPage provider={providerSpecs.r2} />,
})
