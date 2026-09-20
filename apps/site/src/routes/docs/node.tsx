import { createFileRoute } from '@tanstack/react-router'
import { CodeBlock } from '../../components/code-block'

export const Route = createFileRoute('/docs/node')({
  head: () => ({
    meta: [
      { title: 'Node API — mount-rs docs' },
      {
        name: 'description',
        content:
          'The current mount-rs napi-rs Node factory for independent metadata and block providers.',
      },
    ],
  }),
  component: NodeDocs,
})

function NodeDocs() {
  return (
    <article className="doc-article">
      <p className="eyebrow">Documentation / Node</p>
      <h1>Keep the mount optional in JavaScript.</h1>
      <p className="doc-lede">
        The napi-rs package exposes the same split-store idea to Node: select
        metadata and blocks independently, set a chunk size, and release the
        filesystem explicitly when the process is done.
      </p>

      <CodeBlock label="integrations/mount-rs-napi / current factory shape">
        {`const { createChunkedDriver } = require('./integrations/mount-rs-napi');

const fs = await createChunkedDriver({
  metadata: { kind: 'sqlite', uri: './metadata.sqlite' },
  blocks: { kind: 'sqlite', uri: './blocks.sqlite' },
  chunkSize: 65536,
});

try {
  await fs.writeFile('/hello', Buffer.from('hello'));
} finally {
  await fs.shutdown();
}`}
      </CodeBlock>

      <h2>Factory choices</h2>
      <div className="node-factory-grid">
        <div className="node-factory-card"><span className="factory-kind">metadata</span><strong>memory · sqlite · pglite</strong><p>Namespace, leases, revisions, and publication.</p></div>
        <div className="node-factory-card"><span className="factory-kind">blocks</span><strong>memory · sqlite · pglite · r2</strong><p>Immutable bytes and the barrier before publication.</p></div>
        <div className="node-factory-card"><span className="factory-kind">lifecycle</span><strong><code>shutdown()</code></strong><p>Releases provider-owned writer leases explicitly.</p></div>
      </div>
      <div className="callout callout-amber">
        <strong>R2 is block-only here.</strong>
        <p>
          In <code>createChunkedDriver</code>, metadata must use memory, SQLite,
          or PGlite. R2 supplies immutable blocks; it is not a metadata store.
        </p>
      </div>

      <h2>Read the package, then read the caveats</h2>
      <p>
        The example mirrors the repository README and the generated TypeScript
        declarations. Provider configuration is not a promise that every
        combination has passed the full acceptance matrix: live credentials,
        service restart, SQLite hosting, and platform-specific gates remain
        separate evidence.
      </p>
      <div className="source-note">
        <span className="source-note-mark" aria-hidden="true">↗</span>
        <p>
          Source links: <a href="https://github.com/andymac4182/mount-rs/blob/main/integrations/mount-rs-napi/index.d.ts">Node declarations</a>, <a href="https://github.com/andymac4182/mount-rs/blob/main/integrations/mount-rs-napi/src/lib.rs">N-API implementation</a>, and the <a href="https://github.com/andymac4182/mount-rs/blob/main/README.md#node-split-store-api">README example</a>.
        </p>
      </div>
    </article>
  )
}
