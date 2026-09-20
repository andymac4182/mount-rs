import { readFile, writeFile } from 'node:fs/promises'
import { fileURLToPath } from 'node:url'

const outputRoot = fileURLToPath(new URL('../.vercel/output/', import.meta.url))
const configPath = `${outputRoot}config.json`

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

const config = JSON.parse(await readFile(configPath, 'utf8'))
const fallback = config.routes?.find((route) => route.dest === '/__server')

if (config.version !== 3 || !fallback) {
  throw new Error('Vercel output configuration could not be normalized: expected Build Output API v3 and an SSR fallback')
}

config.routes = [
  {
    src: '/downloads/(.*)',
    continue: true,
    headers: {
      ...securityHeaders,
      'content-disposition': 'attachment',
      'content-encoding': 'identity',
      'content-type': 'application/octet-stream',
    },
  },
  {
    src: '/assets/(.*)',
    continue: true,
    headers: {
      ...securityHeaders,
      'cache-control': 'public, max-age=31536000, immutable',
    },
  },
  {
    src: '/(.*)',
    continue: true,
    headers: securityHeaders,
  },
  { handle: 'filesystem' },
  {
    src: '/assets/(.*)',
    status: 404,
    headers: {
      ...securityHeaders,
      'cache-control': 'no-store',
    },
  },
  fallback,
]

await writeFile(configPath, `${JSON.stringify(config, null, 2)}\n`)
console.log('Normalized Vercel output routes: security headers, filesystem handling, SSR fallback, and missing-asset no-store policy')
