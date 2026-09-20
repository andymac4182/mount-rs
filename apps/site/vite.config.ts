import { tanstackStart } from '@tanstack/react-start/plugin/vite'
import { defineConfig } from 'vite'
import viteReact from '@vitejs/plugin-react'
import { nitro } from 'nitro/vite'

const securityHeaders = {
  'Content-Security-Policy':
    "default-src 'self'; base-uri 'self'; connect-src 'self'; frame-ancestors 'none'; frame-src 'none'; form-action 'self'; img-src 'self' data:; object-src 'none'; script-src 'self' 'unsafe-inline'; script-src-attr 'none'; style-src 'self'; style-src-attr 'none'; font-src 'self'; worker-src 'self'",
  'Permissions-Policy': 'camera=(), geolocation=(), microphone=(), payment=(), usb=()',
  'Referrer-Policy': 'strict-origin-when-cross-origin',
  'X-Content-Type-Options': 'nosniff',
  'X-Frame-Options': 'DENY',
  'Cross-Origin-Opener-Policy': 'same-origin',
  'Cross-Origin-Resource-Policy': 'same-origin',
  'X-DNS-Prefetch-Control': 'off',
  'X-Permitted-Cross-Domain-Policies': 'none',
  'Strict-Transport-Security': 'max-age=31536000; includeSubDomains',
}

export default defineConfig({
  plugins: [
    tanstackStart({
      prerender: {
        enabled: true,
        autoStaticPathsDiscovery: true,
        crawlLinks: true,
        concurrency: 4,
        failOnError: true,
      },
    }),
    viteReact(),
    nitro({
      routeRules: {
        '/assets/**': { headers: securityHeaders },
        '/**': { headers: securityHeaders },
      },
    }),
  ],
})
