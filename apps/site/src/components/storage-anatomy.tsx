import type { ReactNode } from 'react'
import { CodeBlock } from './code-block'

type StoragePlane = 'metadata' | 'blocks' | 'control' | 'legacy'

type SchemaColumn = {
  name: string
  type: string
  role: string
}

type SchemaTable = {
  name: string
  plane: StoragePlane
  role: string
  primaryKey: string
  columns: SchemaColumn[]
  note?: string
}

type RelationalModel = {
  kind: 'relational'
  providerLabel: string
  metadataLabel: string
  blockLabel: string
  description: string
  tables: SchemaTable[]
  legacyTables?: SchemaTable[]
  schemaQuery: string
}

type KeyValueEntry = {
  key: string
  value: string
  encoding: string
  role: string
}

type KeyValueModel = {
  kind: 'key-value'
  providerLabel: string
  metadataLabel: string
  blockLabel: string
  description: string
  prefix: string
  entries: KeyValueEntry[]
  rangeNote: string
}

type ObjectEntry = {
  key: string
  value: string
  role: string
}

type ObjectModel = {
  kind: 'object'
  providerLabel: string
  metadataLabel: string
  blockLabel: string
  description: string
  bucket: string
  prefix: string
  entries: ObjectEntry[]
  idNote: string
}

type MemoryModel = {
  kind: 'memory'
  providerLabel: string
  metadataLabel: string
  blockLabel: string
  description: string
  entries: KeyValueEntry[]
}

export type StorageModel = RelationalModel | KeyValueModel | ObjectModel | MemoryModel

const sqliteTables: SchemaTable[] = [
  {
    name: 'mount_rs_metadata',
    plane: 'metadata',
    role: 'One current namespace snapshot plus the writer lease and revision CAS state.',
    primaryKey: 'id = 1',
    columns: [
      { name: 'id', type: 'INTEGER', role: 'fixed singleton key; CHECK(id=1)' },
      { name: 'volume_id', type: 'TEXT', role: 'stable volume identity; added by migration' },
      { name: 'revision', type: 'INTEGER', role: 'monotonic namespace revision' },
      { name: 'namespace', type: 'TEXT NULL', role: 'serialized Namespace: nodes, attributes, layouts, block IDs' },
      { name: 'owner', type: 'TEXT NULL', role: 'current writer owner' },
      { name: 'fence', type: 'INTEGER', role: 'monotonic fencing token' },
      { name: 'expires', type: 'INTEGER', role: 'provider-clock lease expiry' },
    ],
  },
  {
    name: 'mount_rs_blocks',
    plane: 'blocks',
    role: 'Immutable byte blocks referenced by Namespace FileLayout extents.',
    primaryKey: 'id',
    columns: [
      { name: 'id', type: 'TEXT', role: 'opaque BlockId' },
      { name: 'bytes', type: 'BLOB', role: 'raw block bytes; never a path or directory record' },
    ],
  },
  {
    name: 'mount_rs_schema_versions',
    plane: 'control',
    role: 'Migration marker for the versioning schema.',
    primaryKey: 'schema_name',
    columns: [
      { name: 'schema_name', type: 'TEXT', role: 'schema identifier' },
      { name: 'schema_version', type: 'INTEGER', role: 'applied schema version' },
    ],
  },
  {
    name: 'mount_rs_version_state',
    plane: 'metadata',
    role: 'One row tracking the version head and the next sequence/fence values.',
    primaryKey: 'id = 1',
    columns: [
      { name: 'id', type: 'INTEGER', role: 'fixed singleton key; CHECK(id=1)' },
      { name: 'volume_id', type: 'TEXT', role: 'volume this version graph belongs to' },
      { name: 'head_id', type: 'TEXT NULL', role: 'current published version' },
      { name: 'next_sequence', type: 'INTEGER', role: 'next version sequence' },
      { name: 'next_read_fence', type: 'INTEGER', role: 'next reader fence' },
    ],
  },
  {
    name: 'mount_rs_versions',
    plane: 'metadata',
    role: 'Published namespace snapshots and their parent/fork lineage.',
    primaryKey: 'id',
    columns: [
      { name: 'id', type: 'TEXT', role: 'version identifier' },
      { name: 'volume_id', type: 'TEXT', role: 'owning volume' },
      { name: 'sequence', type: 'INTEGER', role: 'monotonic version sequence' },
      { name: 'parent_id', type: 'TEXT NULL', role: 'previous version' },
      { name: 'restored_from', type: 'TEXT NULL', role: 'restore source, if any' },
      { name: 'forked_from', type: 'TEXT NULL', role: 'fork source, if any' },
      { name: 'namespace', type: 'TEXT', role: 'serialized namespace for this version' },
      { name: 'block_store_id', type: 'TEXT', role: 'block-plane identity' },
      { name: 'kind', type: 'TEXT', role: 'publication kind' },
      { name: 'created_at_ms', type: 'INTEGER', role: 'creation timestamp' },
      { name: 'durable', type: 'INTEGER', role: 'durability assertion; 0 or 1' },
      { name: 'operation_id', type: 'TEXT', role: 'idempotency key; UNIQUE' },
    ],
  },
  {
    name: 'mount_rs_version_pins',
    plane: 'metadata',
    role: 'Reader views that pin a version while it is being consumed.',
    primaryKey: 'view_id',
    columns: [
      { name: 'view_id', type: 'TEXT', role: 'reader view identifier' },
      { name: 'volume_id', type: 'TEXT', role: 'owning volume' },
      { name: 'version_id', type: 'TEXT', role: 'pinned version' },
      { name: 'owner', type: 'TEXT', role: 'reader owner' },
      { name: 'fence', type: 'INTEGER', role: 'reader fence' },
      { name: 'expires', type: 'INTEGER', role: 'pin expiry' },
    ],
  },
]

export const providerStorageModels: Record<string, StorageModel> = {
  slatedb: {
    kind: 'object',
    providerLabel: 'SlateDB on RustFS',
    metadataLabel: 'SlateDB database path',
    blockLabel: 'Independent RustFS prefix',
    description: 'SlateDB persists its WAL, SSTs and manifest in the object store; mount-rs stores a namespace record inside SlateDB and file bytes in separate RustFS blocks.',
    bucket: 'caller-selected RustFS bucket',
    prefix: 'separate SlateDB and block prefixes',
    entries: [
      { key: '<slatedb-path>/...', value: 'SlateDB WAL, SST and manifest objects', role: 'database storage' },
      { key: 'mount-rs/metadata/v1', value: 'namespace, revision and fence', role: 'logical SlateDB key' },
      { key: '<block-prefix>/...', value: 'immutable file blocks', role: 'ChunkedFs block storage' },
    ],
    idNote: 'The SlateDB path and block prefix must be distinct and owned by the same volume.',
  },
  memory: {
    kind: 'memory',
    providerLabel: 'Process heap',
    metadataLabel: 'MemoryMetadataStore',
    blockLabel: 'MemoryBlockStore',
    description: 'The same logical split exists, but the records are Rust structs behind Arc<Mutex<...>> rather than persisted keys.',
    entries: [
      { key: 'metadata.revision', value: 'u64 → 7', encoding: 'integer', role: 'namespace CAS revision' },
      { key: 'metadata.namespace', value: 'Option<Namespace>', encoding: 'typed struct', role: 'attributes, directory entries, layouts, and block IDs' },
      { key: 'metadata.lease', value: '{ owner: "writer-a", fence: 3, expires_at_ms: ... }', encoding: 'typed struct', role: 'single writer lease' },
      { key: 'metadata.versions', value: 'BTreeMap<VersionId, VersionInfo>', encoding: 'map', role: 'published version graph' },
      { key: 'blocks["b0123..."]', value: '[raw bytes]', encoding: 'BTreeMap<BlockId, Vec<u8>>', role: 'immutable block plane' },
    ],
  },
  sqlite: {
    kind: 'relational',
    providerLabel: 'SQLite file(s)',
    metadataLabel: 'metadata.sqlite',
    blockLabel: 'blocks.sqlite',
    description: 'The current split-store schema is normalized into six active tables. Metadata and blocks can be separate SQLite files; the table names stay the same.',
    tables: sqliteTables,
    legacyTables: [
      {
        name: 'mount_rs_state',
        plane: 'legacy',
        role: 'Legacy SqliteFs snapshot table; not part of the split MetadataStore/BlockStore schema above.',
        primaryKey: 'id = 1',
        columns: [
          { name: 'id', type: 'INTEGER', role: 'fixed singleton key; CHECK(id=1)' },
          { name: 'revision', type: 'INTEGER', role: 'snapshot revision' },
          { name: 'snapshot', type: 'BLOB', role: 'serialized persisted filesystem snapshot' },
        ],
        note: 'Only expect this table when the legacy SqliteFs state-store API is selected.',
      },
    ],
    schemaQuery: `SELECT name, sql
FROM sqlite_master
WHERE type = 'table' AND name LIKE 'mount_rs_%'
ORDER BY name;

PRAGMA table_info(mount_rs_metadata);
PRAGMA table_info(mount_rs_blocks);
PRAGMA table_info(mount_rs_versions);`,
  },
  pglite: {
    kind: 'relational',
    providerLabel: 'PGlite / PostgreSQL wire',
    metadataLabel: 'mount_rs_metadata',
    blockLabel: 'mount_rs_blocks',
    description: 'The split provider creates two tables in the caller-selected PostgreSQL-compatible database. The volume key scopes metadata and bytes independently.',
    tables: [
      {
        name: 'mount_rs_metadata',
        plane: 'metadata',
        role: 'One row per volume: namespace JSON plus revision and writer lease.',
        primaryKey: 'volume_key',
        columns: [
          { name: 'volume_key', type: 'TEXT', role: 'PRIMARY KEY; independent volume scope' },
          { name: 'revision', type: 'BIGINT', role: 'namespace CAS revision' },
          { name: 'namespace', type: 'TEXT NULL', role: 'serialized Namespace' },
          { name: 'owner', type: 'TEXT NULL', role: 'current writer owner' },
          { name: 'fence', type: 'BIGINT', role: 'monotonic fencing token' },
          { name: 'expires', type: 'BIGINT', role: 'PGlite/provider-clock lease expiry' },
        ],
      },
      {
        name: 'mount_rs_blocks',
        plane: 'blocks',
        role: 'Immutable bytes scoped by volume and block ID.',
        primaryKey: '(volume_key, id)',
        columns: [
          { name: 'volume_key', type: 'TEXT', role: 'volume scope' },
          { name: 'id', type: 'TEXT', role: 'block identity' },
          { name: 'bytes', type: 'BYTEA', role: 'raw immutable block bytes' },
        ],
      },
    ],
    legacyTables: [
      {
        name: 'mount_rs_state',
        plane: 'legacy',
        role: 'Legacy PgliteFs snapshot table; not used by the split providers.',
        primaryKey: 'id',
        columns: [
          { name: 'id', type: 'TEXT', role: 'state key' },
          { name: 'snapshot', type: 'BYTEA', role: 'serialized snapshot' },
          { name: 'revision', type: 'BIGINT', role: 'snapshot revision' },
        ],
        note: 'Only expect this table when the legacy PgliteFs state-store API is selected.',
      },
    ],
    schemaQuery: `SELECT table_name, column_name, data_type
FROM information_schema.columns
WHERE table_name LIKE 'mount_rs_%'
ORDER BY table_name, ordinal_position;`,
  },
  tidb: {
    kind: 'relational',
    providerLabel: 'TiDB / MySQL wire',
    metadataLabel: 'mount_rs_tidb_metadata',
    blockLabel: 'mount_rs_tidb_blocks',
    description: 'TiDB keeps the split provider deliberately small: one metadata row per volume and one content-addressed block table. There are no hidden version tables in this adapter.',
    tables: [
      {
        name: 'mount_rs_tidb_metadata',
        plane: 'metadata',
        role: 'One row per volume: namespace LONGTEXT plus pessimistic-lock lease state.',
        primaryKey: 'volume_key',
        columns: [
          { name: 'volume_key', type: 'VARBINARY(1020)', role: 'PRIMARY KEY; volume scope' },
          { name: 'revision', type: 'BIGINT', role: 'namespace CAS revision' },
          { name: 'namespace', type: 'LONGTEXT NULL', role: 'serialized Namespace' },
          { name: 'owner', type: 'VARBINARY(1020) NULL', role: 'current writer owner' },
          { name: 'fence', type: 'BIGINT', role: 'monotonic fencing token' },
          { name: 'expires', type: 'BIGINT', role: 'provider-clock lease expiry' },
        ],
      },
      {
        name: 'mount_rs_tidb_blocks',
        plane: 'blocks',
        role: 'Content-derived immutable blocks scoped by volume.',
        primaryKey: '(volume_key, id)',
        columns: [
          { name: 'volume_key', type: 'VARBINARY(1020)', role: 'volume scope' },
          { name: 'id', type: 'VARBINARY(65)', role: 'content-derived BlockId' },
          { name: 'bytes', type: 'LONGBLOB', role: 'raw immutable block bytes' },
        ],
      },
    ],
    schemaQuery: `SELECT TABLE_NAME, COLUMN_NAME, COLUMN_TYPE, COLUMN_KEY
FROM information_schema.COLUMNS
WHERE TABLE_SCHEMA = DATABASE()
  AND TABLE_NAME LIKE 'mount_rs_tidb_%'
ORDER BY TABLE_NAME, ORDINAL_POSITION;`,
  },
  foundationdb: {
    kind: 'key-value',
    providerLabel: 'FoundationDB keyspace',
    metadataLabel: '<prefix>\\0meta/*',
    blockLabel: '<prefix>\\0block/*',
    description: 'Every record is a FoundationDB key/value under one caller-owned prefix. Metadata is sharded and described by a binary manifest; blocks are content-addressed.',
    prefix: 'demo-volume',
    entries: [
      { key: 'demo-volume\\0meta/manifest', value: 'MRM1 | revision:u64 | chunk_count:u32 | payload_len:u64', encoding: '24-byte binary', role: 'metadata manifest and shard count' },
      { key: 'demo-volume\\0meta/chunk/<u32-be-index>', value: 'serialized Namespace bytes for one shard', encoding: 'raw bytes; 8 KiB default shard', role: 'metadata payload' },
      { key: 'demo-volume\\0meta/lease', value: 'MRL1 | fence:u64 | expires:u64 | owner_len:u32 | owner', encoding: 'binary lease record', role: 'current writer lease' },
      { key: 'demo-volume\\0meta/fence', value: 'MRF1 | last_fence:u64', encoding: '12-byte binary', role: 'fencing floor that survives lease replacement' },
      { key: 'demo-volume\\0meta/lease-oracle', value: 'MRO1 | provider_time_ms:u64', encoding: '12-byte binary', role: 'persisted shared time authority when configured' },
      { key: 'demo-volume\\0flush', value: 'not a data record; read as an acknowledgement barrier', encoding: 'probe key', role: 'flush round trip' },
      { key: 'demo-volume\\0block/sha256:<64-hex>', value: 'raw immutable block bytes', encoding: 'byte value; 64 KiB default maximum', role: 'content-addressed block' },
    ],
    rangeNote: 'The metadata chunk keys form one range: <prefix>\\0meta/chunk/ followed by a big-endian u32 index. Clearing a whole volume prefix is destructive; reconcile ownership first.',
  },
  r2: {
    kind: 'object',
    providerLabel: 'Cloudflare R2 / S3-compatible bucket',
    metadataLabel: 'selected relational/KV provider',
    blockLabel: 'bucket objects under an owned prefix',
    description: 'R2 is the block plane in the current split-store path. The bucket does not contain a mount-rs namespace table or manifest.',
    bucket: 'mount-rs-tests',
    prefix: 'vol-a/blocks',
    entries: [
      { key: 'vol-a/', value: 'logical prefix boundary; not a stored directory', role: 'volume scope' },
      { key: 'vol-a/blocks/', value: 'LIST prefix used to enumerate this block plane', role: 'block prefix' },
      { key: 'vol-a/blocks/b0123456789abcdef0123456789abcdef', value: 'raw bytes for one immutable block', role: 'exact object key' },
      { key: 'vol-a/blocks/bfedcba9876543210fedcba987654321', value: 'raw bytes for another block', role: 'exact object key' },
    ],
    idNote: 'The current R2 adapter generates a 33-character lowercase ID: b + 32 hexadecimal characters. It is an opaque allocation ID, not a filesystem path or a content hash.',
  },
  rustfs: {
    kind: 'object',
    providerLabel: 'RustFS S3 gateway',
    metadataLabel: 'selected relational/KV provider',
    blockLabel: 'bucket objects under an owned prefix',
    description: 'RustFS exposes the same S3 object shape as the R2 block adapter. Its service boundary is local/CI evidence, not a claim about every S3-compatible deployment.',
    bucket: 'mount-rs',
    prefix: 'mount-rs-tests/owned-run',
    entries: [
      { key: 'mount-rs-tests/', value: 'bucket prefix; no directory object is required', role: 'test namespace' },
      { key: 'mount-rs-tests/owned-run/', value: 'LIST prefix for one harness-owned run', role: 'cleanup boundary' },
      { key: 'mount-rs-tests/owned-run/b0123456789abcdef0123456789abcdef', value: 'raw block bytes', role: 'block object' },
      { key: 'mount-rs-tests/owned-run/bfedcba9876543210fedcba987654321', value: 'raw block bytes', role: 'block object' },
    ],
    idNote: 'Use S3 LIST/HEAD/GET/RANGE to inspect these objects. The namespace and the block references remain in the separately selected metadata provider.',
  },
  'aws-s3': {
    kind: 'object',
    providerLabel: 'AWS S3 bucket',
    metadataLabel: 'selected relational/KV provider',
    blockLabel: 'S3 objects under an owned prefix',
    description: 'The AWS S3 provider uses this block-only, owned-prefix shape and has passed scoped qualification. Bucket and block-key values below are illustrative; qualification does not imply production deployment approval.',
    bucket: 'mount-rs-integration-…',
    prefix: 'mount-rs-tests/owned-run',
    entries: [
      { key: 'mount-rs-tests/', value: 'logical LIST prefix; not a directory', role: 'test namespace' },
      { key: 'mount-rs-tests/owned-run/', value: 'unique cleanup boundary', role: 'owned prefix' },
      { key: 'mount-rs-tests/owned-run/b0123456789abcdef0123456789abcdef', value: 'illustrative raw block object', role: 'illustrative block key' },
      { key: 'mount-rs-tests/owned-run/bfedcba9876543210fedcba987654321', value: 'illustrative raw block object', role: 'illustrative block key' },
    ],
    idNote: 'The prefix shape reflects the qualified AWS block-provider path; the sample block IDs are not captured production keys. AWS qualification is separate from R2/RustFS evidence and does not establish production readiness.',
  },
  ozone: {
    kind: 'object',
    providerLabel: 'Apache Ozone S3 gateway',
    metadataLabel: 'selected relational/KV provider',
    blockLabel: 'Ozone bucket objects under an owned prefix',
    description: 'Ozone is observed through its S3 gateway. The current all-in-one harness uses loopback resources and keeps namespace metadata outside the bucket.',
    bucket: 'mount-rs',
    prefix: 'mount-rs-tests/owned-run',
    entries: [
      { key: 'mount-rs-tests/', value: 'logical S3 prefix; no filesystem directory', role: 'harness scope' },
      { key: 'mount-rs-tests/owned-run/', value: 'LIST/cleanup boundary', role: 'owned prefix' },
      { key: 'mount-rs-tests/owned-run/b0123456789abcdef0123456789abcdef', value: 'raw immutable block bytes', role: 'gateway object' },
      { key: 'mount-rs-tests/owned-run/bfedcba9876543210fedcba987654321', value: 'raw immutable block bytes', role: 'gateway object' },
    ],
    idNote: 'The object key shape is real for the S3 gateway path, while hosted topology, authentication, and mixed-provider acceptance remain experimental.',
  },
}

function PlaneLabel({ plane }: { plane: StoragePlane }) {
  return <span className={`storage-plane-label storage-plane-${plane}`}>{plane}</span>
}

function StorageFlow({ model }: { model: StorageModel }) {
  return (
    <div className="storage-flow" aria-label="How one logical file is split between metadata and immutable bytes">
      <div className="storage-flow-card storage-flow-logical">
        <span className="storage-flow-label">Logical file</span>
        <strong>/docs/readme.md</strong>
        <code>inode 42 · size 8,192</code>
        <small>NodeMetadata → NodeData::File(FileLayout)</small>
      </div>
      <div className="storage-flow-junction" aria-hidden="true"><span>publishes block refs</span></div>
      <div className="storage-flow-stack">
        <div className="storage-flow-card storage-flow-metadata">
          <PlaneLabel plane="metadata" />
          <strong>{model.metadataLabel}</strong>
          <code>namespace + extents + revision/lease</code>
        </div>
        <div className="storage-flow-card storage-flow-blocks">
          <PlaneLabel plane="blocks" />
          <strong>{model.blockLabel}</strong>
          <code>BlockId → raw bytes</code>
        </div>
      </div>
    </div>
  )
}

function NamespaceRecordExample() {
  return (
    <div className="storage-record-example">
      <CodeBlock label="Illustrative Namespace / FileLayout record (shape, not serialized bytes)">{`Namespace {
  nodes[42] = NodeMetadata {
    stats: { ino: 42, size: 8192, mode: regular },
    data: File(FileLayout {
      chunker: { algorithm: "fixed-size", version: 1, chunk_size: 4096 },
      extents: [
        { file_offset: 0,    block: "b012...", block_offset: 0, length: 4096 },
        { file_offset: 4096, block: "bfed...", block_offset: 0, length: 4096 }
      ]
    })
  }
}

BlockId "b012..." -> [4 KiB raw bytes]
BlockId "bfed..."  -> [4 KiB raw bytes]`}</CodeBlock>
    </div>
  )
}

function SchemaCard({ table }: { table: SchemaTable }) {
  return (
    <article className="schema-card">
      <header className="schema-card-header">
        <div>
          <PlaneLabel plane={table.plane} />
          <h4><code>{table.name}</code></h4>
        </div>
        <span className="schema-card-key">PK {table.primaryKey}</span>
      </header>
      <p className="schema-card-role">{table.role}</p>
      <div className="schema-column-wrap">
        <table className="schema-column-table">
          <thead><tr><th>Column</th><th>Type</th><th>Stored meaning</th></tr></thead>
          <tbody>
            {table.columns.map((column) => (
              <tr key={column.name}>
                <td><code>{column.name}</code></td>
                <td><code>{column.type}</code></td>
                <td>{column.role}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {table.note ? <p className="schema-card-note">{table.note}</p> : null}
    </article>
  )
}

function RelationalStorage({ model }: { model: RelationalModel }) {
  return (
    <>
      <div className="storage-model-heading">
        <div>
          <p className="eyebrow">Relational storage</p>
          <h3>{model.providerLabel} schema</h3>
        </div>
        <span className="storage-count">{model.tables.length} active tables</span>
      </div>
      <p className="storage-model-description">{model.description}</p>
      <div className="schema-card-grid">
        {model.tables.map((table) => <SchemaCard key={table.name} table={table} />)}
      </div>
      {model.legacyTables?.length ? (
        <div className="legacy-schema-section">
          <div className="storage-model-heading">
            <div>
              <p className="eyebrow">Compatibility boundary</p>
              <h3>Legacy snapshot schema</h3>
            </div>
            <span className="storage-count">not split-store tables</span>
          </div>
          <p className="storage-model-description">These tables belong to the older snapshot state-store API. They should not be mistaken for the active metadata/block composition.</p>
          <div className="schema-card-grid">
            {model.legacyTables.map((table) => <SchemaCard key={table.name} table={table} />)}
          </div>
        </div>
      ) : null}
      <CodeBlock label="Read the live relational schema">{model.schemaQuery}</CodeBlock>
    </>
  )
}

function KeyValueStorage({ model }: { model: KeyValueModel }) {
  return (
    <>
      <div className="storage-model-heading">
        <div>
          <p className="eyebrow">Key/value storage</p>
          <h3>{model.providerLabel} keyspace</h3>
        </div>
        <span className="storage-count">one volume prefix</span>
      </div>
      <p className="storage-model-description">{model.description}</p>
      <div className="kv-prefix-banner">
        <span className="storage-flow-label">Caller-owned prefix</span>
        <code>{model.prefix}\\0</code>
        <small>NUL separates the volume prefix from every logical suffix.</small>
      </div>
      <div className="kv-entry-list">
        {model.entries.map((entry) => (
          <article className="kv-entry" key={entry.key}>
            <div className="kv-entry-key"><span>key</span><code>{entry.key}</code></div>
            <div className="kv-entry-arrow" aria-hidden="true">→</div>
            <div className="kv-entry-value"><span>{entry.encoding}</span><code>{entry.value}</code></div>
            <p>{entry.role}</p>
          </article>
        ))}
      </div>
      <div className="callout callout-amber storage-range-note">
        <strong>Range and cleanup boundary</strong>
        <p>{model.rangeNote}</p>
      </div>
    </>
  )
}

function ObjectStorage({ model }: { model: ObjectModel }) {
  return (
    <>
      <div className="storage-model-heading">
        <div>
          <p className="eyebrow">Object storage</p>
          <h3>{model.providerLabel} prefix map</h3>
        </div>
        <span className="storage-count">block-only bucket</span>
      </div>
      <p className="storage-model-description">{model.description}</p>
      <div className="object-layout">
        <div className="object-bucket-card">
          <span className="storage-flow-label">Bucket</span>
          <strong>{model.bucket}</strong>
          <div className="object-tree">
            <div className="object-tree-line object-tree-root"><code>{model.bucket}/</code></div>
            <div className="object-tree-line"><span>└─</span><code>{model.prefix}/</code></div>
            <div className="object-tree-line object-tree-object"><span>   ├─</span><code>b0123456789abcdef0123456789abcdef</code></div>
            <div className="object-tree-line object-tree-object"><span>   └─</span><code>bfedcba9876543210fedcba987654321</code></div>
          </div>
        </div>
        <div className="object-explanation">
          <div className="object-plane-card">
            <PlaneLabel plane="metadata" />
            <strong>{model.metadataLabel}</strong>
            <p>Stores the namespace and the references that make each object part of a file.</p>
          </div>
          <div className="object-plane-card">
            <PlaneLabel plane="blocks" />
            <strong>{model.blockLabel}</strong>
            <p>Stores only raw bytes at exact keys. LIST shows a prefix; it does not reveal a directory tree.</p>
          </div>
        </div>
      </div>
      <div className="object-entry-list">
        {model.entries.map((entry) => (
          <div className="object-entry" key={entry.key}>
            <code>{entry.key}</code>
            <span>{entry.role}</span>
            <p>{entry.value}</p>
          </div>
        ))}
      </div>
      <div className="callout callout-blue object-id-note">
        <strong>What the key does not tell you</strong>
        <p>{model.idNote} The file name, inode, offsets, and chunk layout are not encoded in the object key; inspect the metadata plane to map them.</p>
      </div>
    </>
  )
}

function MemoryStorage({ model }: { model: MemoryModel }) {
  return (
    <>
      <div className="storage-model-heading">
        <div>
          <p className="eyebrow">Volatile process storage</p>
          <h3>{model.providerLabel} maps</h3>
        </div>
        <span className="storage-count">no persisted keys</span>
      </div>
      <p className="storage-model-description">{model.description}</p>
      <div className="memory-map">
        {model.entries.map((entry) => (
          <article className="memory-entry" key={entry.key}>
            <PlaneLabel plane={entry.key.startsWith('blocks') ? 'blocks' : 'metadata'} />
            <code>{entry.key}</code>
            <strong>{entry.value}</strong>
            <small>{entry.encoding} · {entry.role}</small>
          </article>
        ))}
      </div>
      <div className="callout callout-amber">
        <strong>There is no filesystem path to browse after exit.</strong>
        <p>Memory provider state is reachable only through the live process handles. Export it explicitly before treating any bytes as a backup.</p>
      </div>
    </>
  )
}

export function StorageAnatomy({ model }: { model: StorageModel }) {
  let details: ReactNode
  if (model.kind === 'relational') details = <RelationalStorage model={model} />
  else if (model.kind === 'key-value') details = <KeyValueStorage model={model} />
  else if (model.kind === 'object') details = <ObjectStorage model={model} />
  else details = <MemoryStorage model={model} />

  return (
    <section className="storage-anatomy">
      <div className="storage-anatomy-intro">
        <p className="eyebrow">Storage anatomy</p>
        <h2>Follow one file through both planes.</h2>
        <p>
          A mount-rs namespace stores names, attributes, directory entries,
          layouts, and block references. The bytes stay in the independent
          block plane. The example below is illustrative; the provider-specific
          schema, keys, and prefixes are the inspectable contract.
        </p>
      </div>
      <StorageFlow model={model} />
      <NamespaceRecordExample />
      <div className="storage-detail-panel">{details}</div>
    </section>
  )
}
