import { createFileRoute } from '@tanstack/react-router'
import { CodeBlock } from '../../components/code-block'

export const Route = createFileRoute('/docs/rust')({
  head: () => ({
    meta: [
      { title: 'Rust API — mount-rs docs' },
      {
        name: 'description',
        content:
          'The mount-rs core Rust contract and split metadata/block storage APIs.',
      },
    ],
  }),
  component: RustDocs,
})

function RustDocs() {
  return (
    <article className="doc-article">
      <p className="eyebrow">Documentation / Rust</p>
      <h1>Use the filesystem contract directly.</h1>
      <p className="doc-lede">
        The core crate exposes the observable contract. The loopback helper
        normalizes paths and forwards operations to an <code>FsDriver</code>
        without requiring a kernel mount.
      </p>

      <CodeBlock label="Core contract + memfs / direct loopback access">
        {`use mount_rs_core::Loopback;
use mount_rs_memfs::{MemoryFs, MemoryOptions};

let fs = Loopback::new(MemoryFs::new(MemoryOptions::default()));
fs.write_file("/hello", b"hello").await?;
assert_eq!(fs.read_file("/hello").await?, b"hello");`}
      </CodeBlock>

      <h2>The public contract</h2>
      <div className="api-table-wrap">
        <table className="api-table">
          <caption className="sr-only">Core Rust API types</caption>
          <thead><tr><th>Type</th><th>Role</th><th>Source</th></tr></thead>
          <tbody>
            <tr><td><code>FsDriver</code></td><td>Filesystem-wide operations and capability reporting.</td><td><a href="https://github.com/andymac4182/mount-rs/blob/main/src/driver.rs">driver.rs ↗</a></td></tr>
            <tr><td><code>FileHandle</code></td><td>Read, write, stat, truncate, sync, and close.</td><td><a href="https://github.com/andymac4182/mount-rs/blob/main/src/driver.rs">driver.rs ↗</a></td></tr>
            <tr><td><code>MetadataStore</code></td><td>Load, lease, fenced publication, and metadata flush.</td><td><a href="https://github.com/andymac4182/mount-rs/blob/main/src/storage.rs">storage.rs ↗</a></td></tr>
            <tr><td><code>BlockStore</code></td><td>Immutable block put/get/delete and byte flush.</td><td><a href="https://github.com/andymac4182/mount-rs/blob/main/src/storage.rs">storage.rs ↗</a></td></tr>
          </tbody>
        </table>
      </div>

      <h2>Split storage with fixed-size chunks</h2>
      <p>
        <code>ChunkedFs</code> composes independent stores. A write publishes
        block references only after new blocks are flushed; the chunker
        configuration is persisted with the namespace and file layout.
      </p>
      <CodeBlock label="mount-rs-memory + mount-rs-chunked">
        {`use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_memory::{MemoryBlockStore, MemoryMetadataStore};

let fs = ChunkedFs::open(
    MemoryMetadataStore::new(),
    MemoryBlockStore::new(),
    ChunkedOptions::fixed("demo", 64 * 1024)?,
).await?;

fs.shutdown().await?; // release the provider writer lease`}
      </CodeBlock>

      <div className="callout callout-amber">
        <strong>Durability is a capability, not an adjective.</strong>
        <p>
          Memory stores are intentionally volatile. Durable providers must
          implement their own flush and fencing semantics; this page does not
          turn a local round trip into crash or power-loss evidence.
        </p>
      </div>

      <p className="source-note inline-source">
        Related source: <a href="https://github.com/andymac4182/mount-rs/tree/main/filesystems/mount-rs-memfs">MemoryFs crate</a>, <a href="https://github.com/andymac4182/mount-rs/tree/main/filesystems/mount-rs-chunked">ChunkedFs crate</a>, and <a href="https://github.com/andymac4182/mount-rs/blob/main/src/chunking.rs">chunking contract</a>.
      </p>
    </article>
  )
}
