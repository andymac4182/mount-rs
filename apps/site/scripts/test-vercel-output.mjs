import { access, readFile } from 'node:fs/promises'
import { fileURLToPath } from 'node:url'

const siteRoot = fileURLToPath(new URL('..', import.meta.url))
const outputRoot = `${siteRoot}/.vercel/output`

async function readJson(path) {
  return JSON.parse(await readFile(`${outputRoot}/${path}`, 'utf8'))
}

async function requireFile(path) {
  try {
    await access(`${outputRoot}/${path}`)
  } catch {
    throw new Error(`Vercel artifact test failed: missing ${path}`)
  }
}

const config = await readJson('config.json')
const nitro = await readJson('nitro.json')
const functionConfig = await readJson('functions/__server.func/.vc-config.json')

if (config.version !== 3) {
  throw new Error(`Vercel artifact test failed: expected output version 3, got ${config.version}`)
}
if (nitro.preset !== 'vercel') {
  throw new Error(`Vercel artifact test failed: expected Nitro preset vercel, got ${nitro.preset}`)
}
if (nitro.serverEntry !== 'functions/__server.func/index.mjs') {
  throw new Error(`Vercel artifact test failed: unexpected server entry ${nitro.serverEntry}`)
}
if (functionConfig.runtime !== 'nodejs24.x') {
  throw new Error(`Vercel artifact test failed: expected nodejs24.x, got ${functionConfig.runtime}`)
}
if (!config.routes.some((route) => route.handle === 'filesystem')) {
  throw new Error('Vercel artifact test failed: filesystem handling route is missing')
}
if (!config.routes.some((route) => route.dest === '/__server')) {
  throw new Error('Vercel artifact test failed: SSR fallback function route is missing')
}
const assetRoute = config.routes.find((route) => route.src === '/assets/(.*)')
if (assetRoute?.headers?.['cache-control'] !== 'public, max-age=31536000, immutable') {
  throw new Error('Vercel artifact test failed: hashed asset cache policy is missing')
}
const requiredSecurityHeaders = [
  'Content-Security-Policy',
  'Permissions-Policy',
  'Referrer-Policy',
  'X-Content-Type-Options',
  'X-Frame-Options',
  'Cross-Origin-Opener-Policy',
  'Cross-Origin-Resource-Policy',
  'X-DNS-Prefetch-Control',
  'X-Permitted-Cross-Domain-Policies',
  'Strict-Transport-Security',
]
const globalHeaderRoute = config.routes.find(
  (route) => route.src === '/(.*)' && route.headers?.['Content-Security-Policy'],
)
for (const header of requiredSecurityHeaders) {
  if (!globalHeaderRoute?.headers?.[header]) {
    throw new Error(`Vercel artifact test failed: missing ${header} from the generated header route`)
  }
}
const missingAssetRoute = config.routes.find(
  (route) => route.src === '/assets/(.*)' && route.status === 404,
)
if (missingAssetRoute?.headers?.['cache-control'] !== 'no-store') {
  throw new Error('Vercel artifact test failed: missing asset route is not no-store')
}

for (const path of [
  'functions/__server.func/index.mjs',
  'static/index.html',
  'static/docs/index.html',
  'static/docs/rust/index.html',
  'static/docs/node/index.html',
  'static/favicon.svg',
]) {
  await requireFile(path)
}

console.log('Vercel artifact check passed: Nitro preset, Node 24 function, prerendered routes, security headers, and asset policies are present')
