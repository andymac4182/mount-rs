import type { ReactNode } from 'react'
import { Link } from '@tanstack/react-router'
import { CodeBlock } from './code-block'
import { StorageAnatomy, providerStorageModels } from './storage-anatomy'

export type ProviderMaturity = 'Validated' | 'Preview' | 'Experimental' | 'Planned'

export type ProviderSpec = {
  slug: string
  name: string
  eyebrow: string
  maturity: ProviderMaturity
  maturityNote: string
  summary: ReactNode
  metadata: ReactNode
  blocks: ReactNode
  layout: ReactNode
  consistency: ReactNode
  inspectLabel: string
  inspectCode: string
  cleanup: ReactNode
  limitations: ReactNode
  evidence: ReactNode
  sources: Array<{ label: string; href: string }>
}

export const providerSpecs = {
  memory: {
    slug: 'memory',
    name: 'Memory / memfs',
    eyebrow: 'Provider / volatile local state',
    maturity: 'Validated',
    maturityNote: 'Core contract and local differential evidence; volatile by design.',
    summary: (
      <>
        The memory providers are the smallest complete composition: useful for
        tests, local tools, and a mount-free process that owns its state. They
        deliberately do not pretend that an acknowledged operation survives
        process exit.
      </>
    ),
    metadata: (
      <>
        <code>MemoryMetadataStore</code> holds the namespace, inode records,
        directory entries, block references, revisions, and writer lease in
        process memory.
      </>
    ),
    blocks: (
      <>
        <code>MemoryBlockStore</code> owns immutable byte blocks separately from
        metadata. The two handles can be composed independently through
        <code>ChunkedFs</code>.
      </>
    ),
    layout: (
      <>
        Files use the persisted fixed-size chunk configuration and metadata
        references to block IDs. In memory, both the layout and bytes disappear
        when the provider state is dropped; there is no on-disk object naming
        convention to preserve.
      </>
    ),
    consistency: (
      <>
        The store provides an in-process writer lease, monotonic fence, revision
        CAS, and a successful no-op flush barrier. <code>durable()</code> is
        always false, so combining memory with a durable provider remains a
        volatile filesystem.
      </>
    ),
    inspectLabel: 'Inspect the live process state',
    inspectCode: `let metadata = MemoryMetadataStore::new();
let blocks = MemoryBlockStore::new();

let loaded = metadata.load().await?;
let bytes = blocks.get(&block_id).await?;
println!("revision={} bytes={}", loaded.revision, bytes.len());`,
    cleanup: (
      <>
        No credentials or external service are involved. Drop the filesystem
        or await its <code>shutdown()</code> method; cleanup is process-local.
        Do not describe a memory snapshot as a backup unless the caller has
        explicitly exported and stored it elsewhere.
      </>
    ),
    limitations: (
      <>
        Memory is not crash-persistent, cross-process, or a replacement for a
        durable database/object store. There is no raw file, SQL table, or
        object prefix for an operator to inspect after the process exits.
      </>
    ),
    evidence: (
      <>
        Core tests, memory provider tests, and seeded oracle traces cover the
        in-process contract. That evidence is intentionally narrower than
        crash recovery or production durability.
      </>
    ),
    sources: [
      { label: 'Memory provider source', href: 'https://github.com/andymac4182/mount-rs/blob/main/integrations/mount-rs-memory/src/lib.rs' },
      { label: 'Architecture and durability model', href: 'https://github.com/andymac4182/mount-rs/blob/main/ARCHITECTURE.md' },
    ],
  },
  sqlite: {
    slug: 'sqlite',
    name: 'SQLite',
    eyebrow: 'Provider / file-backed relational state',
    maturity: 'Validated',
    maturityNote: 'File-backed provider and Linux FUSE hosting have scoped evidence; not universal SQLite safety.',
    summary: (
      <>
        SQLite is the local durable option when the application owns database
        files and can apply the repository's locking and journal-mode rules.
        Metadata and blocks may use separate database files or be paired with
        another provider.
      </>
    ),
    metadata: (
      <>
        <code>SqliteMetadataStore</code> stores the namespace JSON, revision,
        writer owner/fence/expiry, volume identity, and versioning tables in
        <code>mount_rs_metadata</code> plus the version tables.
      </>
    ),
    blocks: (
      <>
        <code>SqliteBlockStore</code> stores immutable bytes in
        <code>mount_rs_blocks(id, bytes)</code>. It can live in a second SQLite
        database so metadata publication and block storage remain separate.
      </>
    ),
    layout: (
      <>
        <code>ChunkedFs</code> records fixed-size chunk configuration in the
        namespace/file layout. A block row contains the opaque block ID and a
        BLOB; SQLite does not reinterpret the bytes as files or pages owned by
        the mount layer.
      </>
    ),
    consistency: (
      <>
        File-backed databases enable the provider's durable flag and use
        SQLite's <code>synchronous=FULL</code> setup plus provider-clock lease
        and revision checks. In-memory SQLite is volatile. SQLite used as a
        provider is different from hosting a separate SQLite database inside a
        native mount.
      </>
    ),
    inspectLabel: 'Inspect metadata and blocks with sqlite3',
    inspectCode: `sqlite3 metadata.sqlite <<'SQL'
SELECT id, revision, length(namespace) AS namespace_bytes
FROM mount_rs_metadata;
SELECT id, sequence, durable FROM mount_rs_versions
ORDER BY sequence DESC LIMIT 10;
SQL

sqlite3 blocks.sqlite \
  'SELECT id, length(bytes) AS block_bytes FROM mount_rs_blocks LIMIT 20;'`,
    cleanup: (
      <>
        Close all filesystem handles before copying or deleting database files.
        Remove only databases owned by the volume; never delete block rows that
        a live metadata revision may still reference. SQLite has no network
        credentials in this provider path.
      </>
    ),
    limitations: (
      <>
        The provider's file durability does not prove safe SQLite hosting over
        every transport. The verified Linux FUSE matrix and macOS single-host
        NFS DELETE-journal path do not establish universal WAL, power-loss,
        distributed-locking, or cross-platform guarantees.
      </>
    ),
    evidence: (
      <>
        Local provider tests and revision-matched Linux FUSE checks cover file
        reopen, leases, SQLite transactions, and selected recovery/fault paths.
        The repository still tracks hosted, Windows, and broader transport
        acceptance separately.
      </>
    ),
    sources: [
      { label: 'SQLite provider source', href: 'https://github.com/andymac4182/mount-rs/blob/main/integrations/mount-rs-sqlite/src/storage.rs' },
      { label: 'SQLite reliability matrix', href: 'https://github.com/andymac4182/mount-rs/blob/main/docs/sqlite-reliability-matrix.md' },
    ],
  },
  pglite: {
    slug: 'pglite',
    name: 'PGlite',
    eyebrow: 'Provider / PostgreSQL wire',
    maturity: 'Preview',
    maturityNote: 'Real socket-server, fencing, reconnect/restart, split-store, and Rust/Node/CLI matrix evidence; deployment durability remains caller-owned.',
    summary: (
      <>
        PGlite provides SQL-backed metadata and blocks through PostgreSQL wire
        connections. It is useful when a JavaScript-hosted PostgreSQL engine is
        already part of the process or service, but the server's storage policy
        must be treated as an explicit application decision.
      </>
    ),
    metadata: (
      <>
        <code>mount_rs_metadata</code> stores one row per <code>volume_key</code>
        with namespace text, revision, writer owner, fence, and expiry. The
        metadata connection and block connection are independent.
      </>
    ),
    blocks: (
      <>
        <code>mount_rs_blocks(volume_key, id, bytes)</code> stores BYTEA blocks.
        IDs are derived from the bytes and a conflict is checked before an
        existing row is reused.
      </>
    ),
    layout: (
      <>
        The mount layer still owns fixed-size chunking and publishes block IDs
        in namespace JSON. PGlite's tables are provider storage, not a second
        filesystem namespace; use separate <code>volume_key</code> values to
        isolate independent compositions.
      </>
    ),
    consistency: (
      <>
        Lease checks and publication use the PostgreSQL/PGlite provider clock
        and row locks. <code>with_durable(true)</code> is a caller assertion;
        the default is volatile because a successful SQL acknowledgement does
        not prove that the server data directory is persistent.
      </>
    ),
    inspectLabel: 'Inspect the SQL endpoint by volume key',
    inspectCode: `psql "$PGLITE_DATABASE_URL" <<'SQL'
SELECT volume_key, revision, length(namespace) AS namespace_bytes
FROM mount_rs_metadata;
SELECT volume_key, id, octet_length(bytes) AS block_bytes
FROM mount_rs_blocks
ORDER BY volume_key, id
LIMIT 20;
SQL`,
    cleanup: (
      <>
        Keep the connection string and any credentials in the environment or a
        secret manager. On teardown, await the provider/Node
        <code>shutdown()</code> or <code>close()</code> path, then delete only
        rows for the owned <code>volume_key</code> inside a controlled test
        transaction.
      </>
    ),
    limitations: (
      <>
        The adapter does not make a host-safe-mount claim about the PGlite data
        directory. Server restart, connection-slot lifecycle, platform support,
        and production crash durability remain separate from SQL round-trip
        success.
      </>
    ),
    evidence: (
      <>
        Fresh current-tree PGlite lifecycle acceptance covers provider
        parity/reconnect/fencing/cancellation, disk-server restart, split
        metadata/blocks, Rust/Node/CLI matrices, and all 40 PGlite-inclusive
        seeded trace lanes. The dedicated provider matrix passed Rust SDK 4/4,
        Node SDK 5/5, and CLI 7/7 gated cases; the Node CLI uses the same
        versioned provider configuration and reopen flow as the Rust CLI. The
        latest focused matrix passed 5 Rust SDK, 4 Node SDK, and 9 CLI cases,
        with PGlite and R2 remaining explicit prerequisite skips. A separate
        configured Rust CLI consumer check passed PGlite split-store write,
        shutdown, reopen, and readback; with PGlite enabled, the CLI matrix is
        10 passes and one explicit R2 skip. No config-validation row is counted
        as live R2 evidence. These are focused consumer checks, not
        live-provider or native-mount acceptance; hosted and release
        acceptance remain separate.
      </>
    ),
    sources: [
      { label: 'PGlite provider source', href: 'https://github.com/andymac4182/mount-rs/blob/main/integrations/mount-rs-pglite/src/storage.rs' },
      { label: 'Node split-store example', href: 'https://github.com/andymac4182/mount-rs/blob/main/README.md#node-split-store-api' },
      { label: 'Provider matrix and Node CLI', href: 'https://github.com/andymac4182/mount-rs/blob/main/tests/provider_matrix/cli.mjs' },
    ],
  },
  r2: {
    slug: 'r2',
    name: 'Cloudflare R2 / S3-compatible blocks',
    eyebrow: 'Provider / object storage',
    maturity: 'Validated',
    maturityNote: 'Authenticated live R2 block, configuration-driven CLI, and both metadata-provider composition gates passed; broader benchmark and release gates remain separate.',
    summary: (
      <>
        R2 is the remote block plane in the current split-store design. It is
        deliberately not used as the metadata store by the Node factory or the
        <code>R2BlockStore</code> API.
      </>
    ),
    metadata: (
      <>
        The chunked R2 adapter owns no namespace metadata. Select SQLite,
        PGlite, TiDB, FoundationDB, or another metadata provider and publish
        references there. The older <code>R2Fs</code> snapshot factory has a
        transitional state object and is not the split-store layout.
      </>
    ),
    blocks: (
      <>
        Immutable blocks are uploaded below the caller-owned prefix as
        <code>prefix/b&lt;opaque-id&gt;</code>. Creates are conditional, reads are
        exact-key GETs, and delete is explicit after a HEAD check.
      </>
    ),
    layout: (
      <>
        Files are split into fixed-size chunks by <code>ChunkedFs</code>. The
        metadata provider records the block IDs; R2 stores bytes only. A
        completed object upload is the block barrier before metadata
        publication, but automatic orphan collection is not implemented.
      </>
    ),
    consistency: (
      <>
        The adapter reports durability only when the caller declares it. A
        configured R2 endpoint normally uses the durable path. Endpoint,
        bucket, credential, and state-key shapes are validated before client
        construction, so malformed configuration fails closed. The live tests
        verify confirmed writes, ranges, reopen, conditional behavior, and
        owned-prefix cleanup. Those results do not turn every object-store
        deployment into a universal power-loss claim.
      </>
    ),
    inspectLabel: 'List, head, range-read, and delete an owned prefix',
    inspectCode: `export R2_ENDPOINT='https://<account>.r2.cloudflarestorage.com'
export R2_BUCKET='mount-rs-tests'
export R2_PREFIX='mount-rs-tests/trace-owned'

aws s3api list-objects-v2 --endpoint-url "$R2_ENDPOINT" \
  --bucket "$R2_BUCKET" --prefix "$R2_PREFIX/"
aws s3api head-object --endpoint-url "$R2_ENDPOINT" \
  --bucket "$R2_BUCKET" --key "$R2_PREFIX/<block-id>"
aws s3api get-object --endpoint-url "$R2_ENDPOINT" \
  --bucket "$R2_BUCKET" --key "$R2_PREFIX/<block-id>" \
  --range bytes=0-63 /tmp/block-prefix.bin`,
    cleanup: (
      <>
        Keep endpoint, access key, and secret values outside source control.
        Give each run a unique prefix, list it before cleanup, and delete only
        objects under that prefix. Never use a shared state key or bucket-wide
        delete as a test cleanup shortcut.
      </>
    ),
    limitations: (
      <>
        R2 is block-only here; it does not solve metadata fencing or garbage
        collection. Orphan blocks can remain after uncertain publication, and
        native/hosted benchmark and full release matrices remain separate from
        the authenticated live gate.
      </>
    ),
    evidence: (
      <>
        The current tracker records authenticated R2 filesystem checks,
        five-seed differential traces, Node and CLI coverage, ranged reads,
        reopen, owned-prefix cleanup, and both supported metadata-provider
        compositions. The portable provider matrix also adds seeded
        positional writes, truncate, flush, and reopen checks, but its R2 row
        remains an explicit credential gate. Local object-store tests and
        RustFS results are not substituted for those live Cloudflare results.
      </>
    ),
    sources: [
      { label: 'R2 block adapter', href: 'https://github.com/andymac4182/mount-rs/blob/main/integrations/mount-rs-r2/src/blocks.rs' },
      { label: 'Live R2 evidence in the tracker', href: 'https://github.com/andymac4182/mount-rs/blob/main/WORK_TRACKER.md#-w05--cloudflare-r2' },
      { label: 'Configuration-driven provider matrix', href: 'https://github.com/andymac4182/mount-rs/blob/main/tests/provider_matrix/config-pglite-r2.json' },
    ],
  },
  rustfs: {
    slug: 'rustfs',
    name: 'RustFS',
    eyebrow: 'Provider / local S3-compatible service',
    maturity: 'Validated',
    maturityNote: 'Pinned service contract, restart, CAS, range, hosted RustFS checks, and real FoundationDB/TiDB composition checkpoints passed; replicated topology and broader consumer coverage remain open.',
    summary: (
      <>
        RustFS is the reproducible local/CI S3-compatible service used to
        exercise the object block path without confusing a local service with
        Cloudflare R2 or AWS S3 acceptance.
      </>
    ),
    metadata: (
      <>
        RustFS stores no mount-rs namespace metadata. Pair its S3 gateway with
        an independent metadata provider such as SQLite, PGlite, TiDB, or
        FoundationDB through <code>ChunkedFs</code>.
      </>
    ),
    blocks: (
      <>
        The block adapter uses an owned S3 bucket/prefix and immutable object
        creates. Objects are addressed by the generated block ID below that
        prefix; the service does not reinterpret their contents.
      </>
    ),
    layout: (
      <>
        The same fixed-size chunker and metadata references used by R2 apply to
        RustFS. Use the service's S3 API to observe object keys, HEAD metadata,
        full bytes, and range reads.
      </>
    ),
    consistency: (
      <>
        The pinned harness exercises conditional writes, CAS publication,
        concurrent access, ranges, fresh reads, restart, and cleanup. Service
        replication, disk policy, authentication, and failure durability are
        deployment properties rather than promises of the adapter.
      </>
    ),
    inspectLabel: 'Use the S3 API against the loopback gateway',
    inspectCode: `export S3_ENDPOINT='http://127.0.0.1:<port>'
export S3_BUCKET='mount-rs'
export S3_PREFIX='mount-rs-tests/owned-run'

aws s3api list-objects-v2 --endpoint-url "$S3_ENDPOINT" \
  --bucket "$S3_BUCKET" --prefix "$S3_PREFIX/"
aws s3api head-object --endpoint-url "$S3_ENDPOINT" \
  --bucket "$S3_BUCKET" --key "$S3_PREFIX/<block-id>"
aws s3api get-object --endpoint-url "$S3_ENDPOINT" \
  --bucket "$S3_BUCKET" --key "$S3_PREFIX/<block-id>" \
  --range bytes=0-63 /tmp/block-prefix.bin`,
    cleanup: (
      <>
        Use the harness-owned container, bucket, and prefix only. Keep any S3
        credentials in the test environment, stop the service before removing
        its data directory, and do not use RustFS results as evidence for a
        different remote provider.
      </>
    ),
    limitations: (
      <>
        The all-in-one local deployment is not production authentication or
        replicated-durability evidence. TiDB/FoundationDB composition passed
        bounded single-node checkpoints, while replicated topology, provider
        restart promotion, broader CLI/Node coverage, and release
        qualification remain tracked separately.
      </>
    ),
    evidence: (
      <>
        The bounded current-tree harness passed immutable block writes/reopens,
        SQLite and PGlite metadata compositions, N-API factories, remote CLI
        HTTP reopen, SQLite VFS over RustFS blocks, fault recovery, service
        restart/reopen, and the RustFS benchmark. FoundationDB and TiDB mixed
        provider runs also passed, with TiDB limited to single-node v8.5.7.
        The maturity label is scoped to these service paths, not every
        S3-compatible server or a replicated production topology.
      </>
    ),
    sources: [
      { label: 'RustFS requirements and harness boundary', href: 'https://github.com/andymac4182/mount-rs/blob/main/REQUIREMENTS.md#rustfs-integration-test-service' },
      { label: 'RustFS evidence in the tracker', href: 'https://github.com/andymac4182/mount-rs/blob/main/WORK_TRACKER.md#-w06--rustfs-integration-service' },
    ],
  },
  tidb: {
    slug: 'tidb',
    name: 'TiDB',
    eyebrow: 'Provider / distributed SQL',
    maturity: 'Experimental',
    maturityNote: 'Provider and single-node ARM64 checks exist; durable topology and mixed-store acceptance remain open.',
    summary: (
      <>
        TiDB can supply either side of the split store using the MySQL wire
        protocol. Its transaction and replication behavior must be qualified
        from the actual TiDB/TiKV/PD deployment rather than inferred from a
        connection URL.
      </>
    ),
    metadata: (
      <>
        <code>mount_rs_tidb_metadata</code> stores the namespace LONGTEXT,
        revision, writer owner/fence/expiry, keyed by the caller's
        <code>volume_key</code>.
      </>
    ),
    blocks: (
      <>
        <code>mount_rs_tidb_blocks</code> stores <code>volume_key</code>, a
        content-derived block ID, and LONGBLOB bytes. Metadata and blocks may
        use separate TiDB options or different providers.
      </>
    ),
    layout: (
      <>
        Fixed-size chunks are represented as block rows and references in the
        namespace JSON. Defaults cap block and namespace payloads at 4 MiB to
        stay below common TiDB/TiKV entry and packet limits; deployments that
        raise those limits must make the matching chunk choice explicit.
      </>
    ),
    consistency: (
      <>
        Metadata operations require pessimistic transactions and a provider
        clock, with a volume-row <code>SELECT ... FOR UPDATE</code>. A flush is
        an acknowledgement round trip, not an fsync command. Set
        <code>durable</code> only when the TiDB/TiKV replication and sync-log
        policy supports that assertion.
      </>
    ),
    inspectLabel: 'Query only the owned volume key',
    inspectCode: `-- Run through an authenticated TiDB SQL client.
SELECT volume_key, revision, OCTET_LENGTH(namespace) AS namespace_bytes
FROM mount_rs_tidb_metadata
WHERE volume_key = '<owned-volume-key>';

SELECT volume_key, id, OCTET_LENGTH(bytes) AS block_bytes
FROM mount_rs_tidb_blocks
WHERE volume_key = '<owned-volume-key>'
ORDER BY id
LIMIT 20;`,
    cleanup: (
      <>
        Use a dedicated database/user and keep the MySQL/TiDB URL in an
        environment variable. The integration tests use a unique volume key;
        clean only that key's metadata and blocks after all clients close, then
        close the pool. Do not drop provider tables shared by another run.
      </>
    ),
    limitations: (
      <>
        A MySQL-compatible server is not TiDB acceptance. The durable 3PD/3TiKV
        topology and provider restart promotion remain capacity-gated, while
        Node, CLI, native-mount, and hosted restart coverage remain open.
      </>
    ),
    evidence: (
      <>
        The real single-node v8.5.7 service run passed TiDB metadata/block
        composition with durable-scope checks, block-absence assertions,
        metadata-row cleanup, and symlink-path rejection. The maturity label
        stays Experimental until replicated/durable topology and broader
        consumer gates are complete.
      </>
    ),
    sources: [
      { label: 'TiDB provider README', href: 'https://github.com/andymac4182/mount-rs/blob/main/integrations/mount-rs-tidb/README.md' },
      { label: 'TiDB provider source', href: 'https://github.com/andymac4182/mount-rs/blob/main/integrations/mount-rs-tidb/src/storage.rs' },
    ],
  },
  foundationdb: {
    slug: 'foundationdb',
    name: 'FoundationDB',
    eyebrow: 'Provider / transactional key-value store',
    maturity: 'Experimental',
    maturityNote: 'Real 7.4.7 provider and RustFS composition checkpoints, including exact owned-prefix cleanup; lease authority and broader integration remain open.',
    summary: (
      <>
        FoundationDB stores the split filesystem in a volume-scoped keyspace.
        It offers transactional serialization, but writer expiry still needs a
        shared provider-time authority and an explicit deployment durability
        assertion.
      </>
    ),
    metadata: (
      <>
        Metadata is a manifest at <code>meta/manifest</code>, sharded namespace
        values under <code>meta/chunk/</code>, and lease/fence/oracle records
        under the same volume prefix.
      </>
    ),
    blocks: (
      <>
        Immutable blocks use keys of the form
        <code>prefix\0block/sha256:&lt;hex-digest&gt;</code> and values containing
        the bytes. The provider validates block IDs and chunk configuration
        before opening or publishing.
      </>
    ),
    layout: (
      <>
        Defaults keep blocks at 64 KiB, metadata shards at 8 KiB, and one
        namespace publication at 512 KiB. The hard 100,000-byte value and
        10,000,000-byte affected-data limits are part of the composition
        preflight, not a late upload error.
      </>
    ),
    consistency: (
      <>
        Writer leases use a transactional persisted logical clock by default;
        without an oracle, lease operations fail closed with
        <code>ENOTSUP</code>. Maybe-committed non-idempotent metadata operations
        return an error for reopen/reconciliation rather than being replayed.
        <code>with_durable(true)</code> remains a caller assertion about the
        cluster.
      </>
    ),
    inspectLabel: 'Inspect a volume-scoped key range with fdbcli',
    inspectCode: `# Use a dedicated cluster file and an owned prefix.
fdbcli -C /path/to/fdb.cluster

# Keys contain a NUL separator; inspect the metadata and block ranges
# for <prefix> with the FoundationDB CLI/key-value tooling.
getrange <prefix>\\x00meta/ <prefix>\\x00meta0
getrange <prefix>\\x00block/ <prefix>\\x00block0`,
    cleanup: (
      <>
        Keep the cluster file and native client outside the repository. Grant
        the storage authority only its keyspace, and clear/delete only a
        volume prefix after exclusive ownership is established. A broad
        <code>clear_range</code> is irreversible and is not normal test
        cleanup.
      </>
    ),
    limitations: (
      <>
        The feature is opt-in and requires a matching FoundationDB 7.4 native
        client and cluster. The lease oracle's clock-skew/availability tradeoff,
        service restart, hosted root integration, Node/CLI, and broader platform
        coverage remain open; commit versions are not wall-clock expiry.
      </>
    ),
    evidence: (
      <>
        The real pinned Linux ARM64 provider and RustFS composition checks cover
        blocks, metadata, CAS, fencing, reopen, and the latest exact owned-
        prefix cleanup with sibling/parent sentinel preservation. This is not
        yet a general production or release-readiness claim.
      </>
    ),
    sources: [
      { label: 'FoundationDB integration README', href: 'https://github.com/andymac4182/mount-rs/blob/main/integrations/mount-rs-foundationdb/README.md' },
      { label: 'FoundationDB keyspace implementation', href: 'https://github.com/andymac4182/mount-rs/blob/main/integrations/mount-rs-foundationdb/src/lib.rs' },
    ],
  },
  'aws-s3': {
    slug: 'aws-s3',
    name: 'AWS S3',
    eyebrow: 'Provider / remote object storage',
    maturity: 'Planned',
    maturityNote: 'Private AWS bucket provisioning is recorded; Rust provider and composed acceptance are still pending.',
    summary: (
      <>
        AWS S3 is the next remote object-store target for the S3-compatible
        block path. The project has provisioned a private test bucket, but a
        bucket existing is not the same as a completed Rust integration gate.
      </>
    ),
    metadata: (
      <>
        The current object adapter is block-only. Use an independently selected
        metadata provider for namespace, revisions, leases, and chunk
        references; do not put metadata in a shared S3 listing and infer CAS.
      </>
    ),
    blocks: (
      <>
        The intended layout is an owned prefix containing one immutable object
        per fixed-size chunk, inspected through S3 GET/HEAD/LIST and ranged
        reads. Conditional create and delete must be verified against the
        actual AWS endpoint before the label changes.
      </>
    ),
    layout: (
      <>
        <code>ChunkedFs</code> selects the chunk size and the metadata provider
        records object IDs. A remote object key is not a filesystem path and
        is not safe to delete unless the owning metadata revision has been
        reconciled.
      </>
    ),
    consistency: (
      <>
        S3's service durability and visibility behavior belong to the selected
        AWS deployment. The adapter's completed upload barrier is necessary but
        not sufficient evidence for AWS-specific crash, IAM, retry, or
        multi-writer acceptance.
      </>
    ),
    inspectLabel: 'Inspect an AWS-owned prefix without exposing credentials',
    inspectCode: `export AWS_REGION='ap-southeast-2'
export S3_BUCKET='mount-rs-integration-...'
export S3_PREFIX='mount-rs-tests/owned-run'

aws s3api list-objects-v2 --bucket "$S3_BUCKET" \
  --prefix "$S3_PREFIX/"
aws s3api head-object --bucket "$S3_BUCKET" \
  --key "$S3_PREFIX/<block-id>"
aws s3api get-object --bucket "$S3_BUCKET" \
  --key "$S3_PREFIX/<block-id>" --range bytes=0-63 /tmp/block-prefix.bin`,
    cleanup: (
      <>
        Use the private test bucket, a least-privilege role/profile, and a
        unique prefix with a retention rule. Never put access keys in the
        repository or page examples. Delete only objects under the owned
        prefix after the test has closed all clients.
      </>
    ),
    limitations: (
      <>
        AWS S3 Rust integration, restart/reopen, ranges, conditional writes,
        composed metadata providers, and cleanup evidence remain open in the
        tracker. The private bucket is infrastructure preparation, not a
        passing backend result.
      </>
    ),
    evidence: (
      <>
        The maturity label is Planned because the current evidence stops at
        private bucket provisioning and read-back. Cloudflare R2 and RustFS
        results are useful comparisons but do not substitute for AWS S3.
      </>
    ),
    sources: [
      { label: 'AWS S3 workstream', href: 'https://github.com/andymac4182/mount-rs/blob/main/WORK_TRACKER.md#-w25--actual-aws-s3-integration' },
      { label: 'S3-compatible block adapter', href: 'https://github.com/andymac4182/mount-rs/blob/main/integrations/mount-rs-r2/src/blocks.rs' },
    ],
  },
  ozone: {
    slug: 'ozone',
    name: 'Apache Ozone',
    eyebrow: 'Provider / S3-compatible gateway',
    maturity: 'Experimental',
    maturityNote: 'Pinned 2.2.1 gateway block/restart checkpoint; mixed stores and hosted topology remain open.',
    summary: (
      <>
        Apache Ozone is exercised through its S3 gateway rather than a new
        mount-rs core dependency. It is a useful object-store target, but the
        current all-in-one harness is a loopback test deployment, not a
        production-authenticated replicated cluster.
      </>
    ),
    metadata: (
      <>
        Ozone supplies the object block plane only. Namespace metadata,
        revisions, leases, and chunk references remain in a separate provider
        such as SQLite, PGlite, TiDB, or FoundationDB.
      </>
    ),
    blocks: (
      <>
        Blocks are immutable S3 objects below a caller-owned bucket/prefix.
        Use the gateway's List/Head/Get/Range operations to inspect the object
        keys; the bytes are not stored in the Ozone metadata tables by
        mount-rs.
      </>
    ),
    layout: (
      <>
        Fixed-size chunks are chosen by <code>ChunkedFs</code> and referenced by
        the independent metadata store. Conditional object creation, range
        reads, and delete ownership must remain visible in the test evidence.
      </>
    ),
    consistency: (
      <>
        The pinned local gateway harness passed immutable block operations,
        CAS/publication, concurrent access, restart/reopen, and cleanup. The
        actual cluster's replication, Ratis, auth, TLS, and failure policy are
        not inferred from that single-node or all-in-one run.
      </>
    ),
    inspectLabel: 'Use S3-compatible tools against the Ozone gateway',
    inspectCode: `export OZONE_ENDPOINT='http://127.0.0.1:<s3-port>'
export OZONE_BUCKET='mount-rs'
export OZONE_PREFIX='mount-rs-tests/owned-run'

aws s3api list-objects-v2 --endpoint-url "$OZONE_ENDPOINT" \
  --bucket "$OZONE_BUCKET" --prefix "$OZONE_PREFIX/"
aws s3api head-object --endpoint-url "$OZONE_ENDPOINT" \
  --bucket "$OZONE_BUCKET" --key "$OZONE_PREFIX/<block-id>"
aws s3api get-object --endpoint-url "$OZONE_ENDPOINT" \
  --bucket "$OZONE_BUCKET" --key "$OZONE_PREFIX/<block-id>" \
  --range bytes=0-63 /tmp/block-prefix.bin`,
    cleanup: (
      <>
        Keep the Ozone cluster and gateway isolated, use an owned bucket/prefix,
        and remove only resources created by the harness. Production cleanup
        must follow Ozone's retention and replication policy; do not use a
        broad bucket delete in examples.
      </>
    ),
    limitations: (
      <>
        Hosted Linux-amd64 results, mixed metadata-provider composition, and
        Node/CLI acceptance remain open. The non-secure all-in-one service is
        loopback-only and is not production authentication or durability
        evidence.
      </>
    ),
    evidence: (
      <>
        The current maturity is Experimental: the real gateway harness has a
        meaningful block/restart checkpoint, but the broader backend matrix and
        deployment topology are not yet accepted.
      </>
    ),
    sources: [
      { label: 'Ozone acceptance requirements', href: 'https://github.com/andymac4182/mount-rs/blob/main/REQUIREMENTS.md#apache-ozone-backend-acceptance' },
      { label: 'Ozone workstream evidence', href: 'https://github.com/andymac4182/mount-rs/blob/main/WORK_TRACKER.md#-w26--apache-ozone-s3-backend' },
    ],
  },
} as const satisfies Record<string, ProviderSpec>

export function MaturityBadge({ maturity }: { maturity: ProviderMaturity }) {
  return <span className={`maturity-badge maturity-${maturity.toLowerCase()}`}>{maturity}</span>
}

export function ProviderIndex() {
  return (
    <article className="doc-article provider-index">
      <p className="eyebrow">Documentation / providers</p>
      <h1>Choose the plane before the provider.</h1>
      <p className="doc-lede">
        Providers are deliberately split into metadata and immutable blocks.
        These pages describe what the repository currently implements, how to
        inspect the backing data, how each schema/keyspace/prefix is laid out,
        and exactly where the evidence stops.
      </p>

      <div className="callout callout-blue">
        <strong>How to read maturity</strong>
        <p>
          <b>Validated</b> means the named surface has current scoped test or
          live-service evidence. <b>Preview</b> means the integration is useful
          with meaningful evidence but important deployment gates remain open.
          <b> Experimental</b> marks a real implementation/checkpoint with
          material acceptance gaps. <b>Planned</b> means preparation exists but
          the provider integration is not yet accepted.
        </p>
      </div>

      <div className="provider-card-grid">
        {Object.values(providerSpecs).map((provider) => (
          <Link className="provider-card" key={provider.slug} to={`/docs/providers/${provider.slug}`}>
            <div className="provider-card-top">
              <span className="eyebrow">{provider.eyebrow}</span>
              <MaturityBadge maturity={provider.maturity} />
            </div>
            <h2>{provider.name}</h2>
            <p>{provider.maturityNote}</p>
            <span className="card-arrow" aria-hidden="true">→</span>
          </Link>
        ))}
      </div>

      <div className="source-note">
        <span className="source-note-mark" aria-hidden="true">↗</span>
        <p>
          The source of truth is the repository's <a href="https://github.com/andymac4182/mount-rs/blob/main/ARCHITECTURE.md">architecture</a>,
          <a href="https://github.com/andymac4182/mount-rs/blob/main/PORTING_STATUS.md"> porting evidence</a>,
          <a href="https://github.com/andymac4182/mount-rs/blob/main/REQUIREMENTS.md"> requirements</a>, and
          <a href="https://github.com/andymac4182/mount-rs/blob/main/WORK_TRACKER.md"> work tracker</a>.
        </p>
      </div>
    </article>
  )
}

export function ProviderPage({ provider }: { provider: ProviderSpec }) {
  return (
    <article className="doc-article provider-page">
      <div className="doc-kicker-row">
        <p className="eyebrow">{provider.eyebrow}</p>
        <MaturityBadge maturity={provider.maturity} />
      </div>
      <h1>{provider.name}</h1>
      <p className="doc-lede">{provider.summary}</p>

      <div className="provider-facts">
        <div className="provider-fact">
          <span className="fact-label">Metadata</span>
          <p>{provider.metadata}</p>
        </div>
        <div className="provider-fact">
          <span className="fact-label">Immutable blocks</span>
          <p>{provider.blocks}</p>
        </div>
      </div>

      <StorageAnatomy model={providerStorageModels[provider.slug]} />

      <div className="callout callout-blue">
        <strong>{provider.maturity} — scoped evidence</strong>
        <p>{provider.maturityNote} {provider.evidence}</p>
      </div>

      <h2>Layout and chunking</h2>
      <p>{provider.layout}</p>

      <h2>Durability and consistency</h2>
      <p>{provider.consistency}</p>
      <p>
        Across every split-store composition, new blocks are flushed before
        metadata references are published. A provider acknowledgement is not
        automatically a power-loss or distributed-locking guarantee.
      </p>

      <h2>{provider.inspectLabel}</h2>
      <CodeBlock label="Read-only inspection example">{provider.inspectCode}</CodeBlock>

      <h2>Credentials, cleanup, and safe operations</h2>
      <p>{provider.cleanup}</p>

      <h2>Current limits</h2>
      <p>{provider.limitations}</p>

      <div className="source-note">
        <span className="source-note-mark" aria-hidden="true">↗</span>
        <p>
          Sources:{' '}
          {provider.sources.map((source, index) => (
            <span key={source.href}>
              {index > 0 ? ' · ' : ''}<a href={source.href}>{source.label}</a>
            </span>
          ))}
        </p>
      </div>
      <p className="inline-source">
        <Link to="/docs/providers">← Back to providers</Link>
      </p>
    </article>
  )
}
