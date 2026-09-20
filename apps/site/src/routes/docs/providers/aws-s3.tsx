import { createFileRoute } from '@tanstack/react-router'
import { ProviderPage, providerSpecs } from '../../../components/provider-doc'

export const Route = createFileRoute('/docs/providers/aws-s3')({
  head: () => ({ meta: [{ title: 'AWS S3 — mount-rs docs' }] }),
  component: () => <ProviderPage provider={providerSpecs['aws-s3']} />,
})
