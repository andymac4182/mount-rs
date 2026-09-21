import { createServer } from 'node:net'
import { readFile } from 'node:fs/promises'
import { spawn } from 'node:child_process'

const host = '127.0.0.1'
const requestTimeoutMs = 5_000
const routes = [
  {
    path: '/',
    status: 200,
    contentType: 'text/html',
    expected: ['Put bytes where your process needs them.', 'not release-ready', 'ChunkedFs'],
  },
  {
    path: '/docs',
    status: 200,
    contentType: 'text/html',
    expected: ['The boundary is the product.', 'createChunkedDriver', 'GitHub Release', 'View downloads', 'v0.1.0-cli-preview'],
  },
  { path: '/docs/', status: 307, redirectTo: '/docs' },
  {
    path: '/downloads',
    status: 200,
    contentType: 'text/html',
    expected: ['Get the mount-rs CLI.', 'Current verified preview', 'v0.1.0-cli-preview', 'aarch64-apple-darwin', '437f9068bd1fe274abd3cafe42eb7eca7e7c71f15c7220dd2edd6c3c31a50f18', 'How release publication works'],
  },
  { path: '/downloads/', status: 307, redirectTo: '/downloads' },
  {
    path: '/docs/rust',
    status: 200,
    contentType: 'text/html',
    expected: ['FsDriver', 'ChunkedFs', 'shutdown'],
  },
  { path: '/docs/rust/', status: 307, redirectTo: '/docs/rust' },
  {
    path: '/docs/node',
    status: 200,
    contentType: 'text/html',
    expected: ['createChunkedDriver', 'R2 is block-only here', 'shutdown'],
  },
  { path: '/docs/node/', status: 307, redirectTo: '/docs/node' },
  {
    path: '/docs/providers',
    status: 200,
    contentType: 'text/html',
    expected: ['Cloudflare R2 / S3-compatible blocks', 'Validated', 'How to read maturity'],
  },
  { path: '/docs/providers/', status: 307, redirectTo: '/docs/providers' },
  {
    path: '/docs/providers/memory',
    status: 200,
    contentType: 'text/html',
    expected: ['Memory / memfs', 'durable()', 'metadata.versions', 'Validated'],
  },
  {
    path: '/docs/providers/sqlite',
    status: 200,
    contentType: 'text/html',
    expected: ['SQLite', 'mount_rs_metadata', 'mount_rs_versions', 'Legacy snapshot schema', 'sqlite3'],
  },
  {
    path: '/docs/providers/pglite',
    status: 200,
    contentType: 'text/html',
    expected: ['PGlite', 'mount_rs_blocks', 'information_schema.columns', 'mount_rs_state', 'PGLITE_DATABASE_URL', '40 PGlite-inclusive', '5 Rust SDK', '4 Node SDK', '9 CLI cases', '10 passes', 'one explicit R2 skip'],
  },
  {
    path: '/docs/providers/r2',
    status: 200,
    contentType: 'text/html',
    expected: ['Cloudflare R2 / S3-compatible blocks', 'vol-a/blocks/', 'b0123456789abcdef0123456789abcdef', 'aws s3api', 'block-only', 'both metadata-provider composition gates', 'fails closed', 'five-seed differential traces', 'positional writes'],
  },
  {
    path: '/docs/providers/rustfs',
    status: 200,
    contentType: 'text/html',
    expected: ['RustFS', 'owned-run/', 'raw block bytes', 'S3-compatible', 'FoundationDB', 'TiDB', 'v8.5.7', 'Validated'],
  },
  {
    path: '/docs/providers/tidb',
    status: 200,
    contentType: 'text/html',
    expected: ['TiDB', 'mount_rs_tidb_metadata', 'mount_rs_tidb_blocks', 'information_schema.COLUMNS', 'v8.5.7', 'symlink-path rejection', 'Experimental'],
  },
  {
    path: '/docs/providers/foundationdb',
    status: 200,
    contentType: 'text/html',
    expected: ['FoundationDB', 'meta/manifest', 'meta/lease-oracle', 'block/sha256:', 'owned-prefix cleanup', 'fdbcli'],
  },
  {
    path: '/docs/providers/aws-s3',
    status: 200,
    contentType: 'text/html',
    expected: ['AWS S3', 'mount-rs-tests/owned-run/', 'planned block key', 'Planned', 'AWS_REGION'],
  },
  {
    path: '/docs/providers/ozone',
    status: 200,
    contentType: 'text/html',
    expected: ['Apache Ozone', 'logical S3 prefix', 'mount-rs-tests/owned-run/', 'OZONE_ENDPOINT', 'Experimental'],
  },
  {
    path: '/docs/transports',
    status: 200,
    contentType: 'text/html',
    expected: ['HTTP multi-drive API', 'Mount-free is a first-class path.', 'Preview'],
  },
  { path: '/docs/transports/', status: 307, redirectTo: '/docs/transports' },
  {
    path: '/docs/transports/fuse',
    status: 200,
    contentType: 'text/html',
    expected: ['FUSE', '/dev/fuse', 'Preview', 'Rust-backed', './fuse', 'GETATTR', 'SETATTR', 'OPEN', 'OPENDIR', 'CREATE', 'LOOKUP', 'READLINK', 'STATFS', 'BATCH_FORGET', 'INTERRUPT', 'RELEASE', 'FSYNC', 'READDIRPLUS', 'ACCESS', 'FUSE_INIT_EXT', 'packDirentsPlus', 'Hosted CI run 35575442663'],
  },
  {
    path: '/docs/transports/nfs',
    status: 200,
    contentType: 'text/html',
    expected: ['NFS', 'mount_nfs', 'Preview', 'unlink/rename', '266 oracle', 'Node CLI', 'HostFs', 'capability-gated'],
  },
  {
    path: '/docs/transports/9p',
    status: 200,
    contentType: 'text/html',
    expected: ['9P2000.L', '9pnet_tcp', 'Experimental'],
  },
  {
    path: '/docs/transports/fskit',
    status: 200,
    contentType: 'text/html',
    expected: ['macOS FSKit', 'CODE_SIGNING_ALLOWED=NO', 'Planned'],
  },
  {
    path: '/docs/transports/http',
    status: 200,
    contentType: 'text/html',
    expected: ['HTTP multi-drive API', '/v1/drives', 'Preview'],
  },
  {
    path: '/docs/transports/webdav',
    status: 200,
    contentType: 'text/html',
    expected: ['WebDAV', 'mount.davfs', 'Preview'],
  },
  {
    path: '/missing',
    status: 404,
    contentType: 'text/html',
    expected: ['That path is not in the namespace.'],
  },
  { path: '/favicon.svg', status: 200, contentType: 'image/svg+xml', expected: ['<svg'] },
]

async function findFreePort() {
  const probe = createServer()
  await new Promise((resolve, reject) => {
    probe.once('error', reject)
    probe.listen(0, host, resolve)
  })
  const address = probe.address()
  const port = typeof address === 'object' && address ? address.port : 0
  await new Promise((resolve, reject) => probe.close((error) => error ? reject(error) : resolve()))
  return port
}

const configuredPort = process.env.PORT ? Number(process.env.PORT) : await findFreePort()
const port = configuredPort || await findFreePort()
const baseUrl = `http://${host}:${port}`
const vercelConfig = JSON.parse(
  await readFile(new URL('../vercel.json', import.meta.url), 'utf8'),
)
const wildcardHeaders = vercelConfig.headers?.find(({ source }) => source === '/(.*)')
const configuredHeaders = new Map(
  wildcardHeaders?.headers?.map(({ key, value }) => [key.toLowerCase(), value]),
)
const requiredHeaders = [
  'content-security-policy',
  'permissions-policy',
  'referrer-policy',
  'x-content-type-options',
  'x-frame-options',
  'cross-origin-opener-policy',
  'cross-origin-resource-policy',
  'x-dns-prefetch-control',
  'x-permitted-cross-domain-policies',
  'strict-transport-security',
]

if (vercelConfig.framework !== 'tanstack-start') {
  throw new Error('route test failed: vercel.json must select the tanstack-start framework')
}
if (vercelConfig.public === true) {
  throw new Error('route test failed: vercel.json must not make deployment source public')
}
for (const header of requiredHeaders) {
  if (!configuredHeaders.has(header)) {
    throw new Error(`route test failed: vercel.json is missing ${header}`)
  }
}
const csp = String(configuredHeaders.get('content-security-policy'))
const requiredCspDirectives = [
  "default-src 'self'",
  "base-uri 'self'",
  "connect-src 'self'",
  "frame-ancestors 'none'",
  "frame-src 'none'",
  "form-action 'self'",
  "img-src 'self' data:",
  "object-src 'none'",
  "script-src 'self' 'unsafe-inline'",
  "script-src-attr 'none'",
  "style-src 'self'",
  "style-src-attr 'none'",
  "font-src 'self'",
  "worker-src 'self'",
]
for (const directive of requiredCspDirectives) {
  if (!csp.includes(directive)) {
    throw new Error(`route test failed: CSP is missing ${directive}`)
  }
}
if (csp.includes("'unsafe-eval'") || /(?:^|;)\s*(?:default-src|connect-src|img-src|font-src)[^;]*\*/.test(csp)) {
  throw new Error('route test failed: CSP contains an unsafe evaluation or wildcard source')
}

const server = spawn(process.execPath, ['.output/server/index.mjs'], {
  env: { ...process.env, HOST: host, NODE_ENV: 'production', PORT: String(port) },
  stdio: ['ignore', 'pipe', 'pipe'],
  detached: true,
})

let serverOutput = ''
let serverError
let serverExited = false
let serverExitCode
server.stdout.on('data', (chunk) => { serverOutput += chunk.toString() })
server.stderr.on('data', (chunk) => { serverOutput += chunk.toString() })
server.once('error', (error) => { serverError = error })
server.once('exit', (code, signal) => {
  serverExited = true
  serverExitCode = code ?? signal
})

function stopServer() {
  if (!server.pid || serverExited) return
  try { process.kill(-server.pid, 'SIGTERM') } catch {}
}

function fail(message) {
  stopServer()
  console.error(message)
  if (serverOutput) console.error(serverOutput)
  process.exit(1)
}

async function request(path, options = {}) {
  const controller = new AbortController()
  const timer = setTimeout(() => controller.abort(), requestTimeoutMs)
  try {
    return await fetch(`${baseUrl}${path}`, { ...options, signal: controller.signal })
  } finally {
    clearTimeout(timer)
  }
}

process.on('exit', stopServer)
process.on('SIGINT', () => { stopServer(); process.exit(130) })
process.on('SIGTERM', () => { stopServer(); process.exit(143) })

const deadline = Date.now() + 20_000
let lastError
let ready = false
while (Date.now() < deadline) {
  if (serverExited) break
  try {
    const response = await request('/')
    await response.arrayBuffer()
    if (response.status === 200) {
      ready = true
      break
    }
    lastError = new Error(`HTTP ${response.status}`)
  } catch (error) {
    lastError = error
  }
  await new Promise((resolve) => setTimeout(resolve, 150))
}

if (!ready) {
  const reason = serverError ?? (serverExited ? `server exited with ${serverExitCode}` : lastError)
  fail(`route test failed: server did not become ready: ${reason ?? 'timeout'}`)
}

const discoveredInternalPaths = new Set()
for (const route of routes) {
  if (serverExited) fail(`route test failed: server exited during ${route.path}`)
  let response
  try {
    response = await request(route.path, { redirect: 'manual' })
  } catch (error) {
    fail(`route test failed: ${route.path}: ${error}`)
  }
  const body = await response.text()
  if (response.status !== route.status) {
    fail(`route test failed: ${route.path}: HTTP ${response.status}, expected ${route.status}`)
  }
  if (route.redirectTo) {
    const location = response.headers.get('location')
    const redirectedPath = location ? new URL(location, baseUrl).pathname : null
    if (redirectedPath !== route.redirectTo) {
      fail(`route test failed: ${route.path}: Location ${location ?? '(missing)'}, expected ${route.redirectTo}`)
    }
    console.log(`route ok ${route.path} HTTP ${response.status} -> ${redirectedPath}`)
    continue
  }
  const actualContentType = response.headers.get('content-type') ?? ''
  if (!actualContentType.startsWith(route.contentType)) {
    fail(`route test failed: ${route.path}: content type ${actualContentType}, expected ${route.contentType}`)
  }
  if (route.contentType === 'text/html' && !body.includes('<html')) {
    fail(`route test failed: ${route.path}: response was not HTML`)
  }
  for (const expected of route.expected ?? []) {
    if (!body.includes(expected)) {
      fail(`route test failed: ${route.path}: expected content was missing: ${expected}`)
    }
  }
  for (const match of body.matchAll(/(?:href|src)="(\/[^"?#]*)/g)) {
    discoveredInternalPaths.add(match[1])
  }
  console.log(`route ok ${route.path} HTTP ${response.status} ${actualContentType}`)
}

for (const path of discoveredInternalPaths) {
  const response = await request(path, { redirect: 'manual' })
  if (response.status !== 200) {
    fail(`route test failed: rendered link or asset ${path}: HTTP ${response.status}`)
  }
  await response.arrayBuffer()
  console.log(`link ok ${path} HTTP ${response.status}`)
}

stopServer()
console.log(`route smoke passed: ${routes.length} paths and ${discoveredInternalPaths.size} rendered links/assets; Vercel config checks passed`)
