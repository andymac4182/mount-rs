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
    maturityNote: 'Real socket-server, fencing, reconnect/restart, split-store, and Rust/Node/CLI matrix evidence, plus hosted cross-platform recovery checks; production persistence, backup, observability, and ownership remain open.',
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
        with namespace text, revision, writer owner, fence, expiry, and a
        stable <code>volume_id</code>. Versioning adds
        <code>mount_rs_schema_versions</code>, <code>mount_rs_version_state</code>,
        <code>mount_rs_versions</code>, and <code>mount_rs_version_pins</code>;
        each record keeps sequence, parent/restore/fork links, namespace JSON,
        block-store ID, kind, timestamp, durability, and lease-pin ownership
        explicit. The metadata connection and block connection are independent.
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
        filesystem namespace. On startup the versioning schema records
        <code>mount-rs-versioning</code> at schema version <code>1</code>, checks
        that stored versions and pins belong to the metadata
        <code>volume_id</code>, and keeps the current head plus next sequence in
        <code>mount_rs_version_state</code>. Use separate
        <code>volume_key</code> values to isolate independent compositions.
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
SELECT schema_name, schema_version
FROM mount_rs_schema_versions
ORDER BY schema_name;
SELECT volume_key, volume_id, head_id, next_sequence, next_read_fence
FROM mount_rs_version_state;
SELECT volume_key, id, sequence, parent_id, restored_from, forked_from,
       kind, block_store_id, created_at_ms, durable
FROM mount_rs_versions
ORDER BY volume_key, sequence
LIMIT 20;
SELECT volume_key, view_id, version_id, owner, fence, expires
FROM mount_rs_version_pins
ORDER BY volume_key, view_id;
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
        directory. Production persistence, connection-slot lifecycle,
        backup/restore, observability, platform support, and crash durability
        remain separate from SQL round-trip success.
      </>
    ),
    evidence: (
      <>
        The latest exact published-tree PGlite rerun passed provider
        lifecycle/reconnect/fencing/cancellation, SQLite VFS round-trip,
        fresh-provider reconnect, disk-server restart, split stores, N-API,
        chunked storage, userspace FUSE, Rust SDK 6/6, Node SDK 5/5, CLI 11/11,
        the upstream suite at 1,200 passed and 82 skipped, and all 40
        PGlite-inclusive seeded oracle lanes. R2 factory/runtime rows remained
        explicit credential-gated
        skips, so no config-validation row is counted as live R2 evidence. The
        earlier focused matrix also passed 5 Rust SDK, 4 Node SDK, and 9 CLI
        cases; the local R2 adapter contract also recorded 10 passes and one
        explicit R2 skip. These are focused consumer checks, not
        live-provider or native-mount acceptance; hosted and release
        acceptance remain separate. The current provider initializes and
        validates durable version metadata on reconnect, including head,
        sequence, volume identity, and pin invariants; a newer stored schema or
        cross-volume version record fails closed. The current hosted W04
        packet also passed the exact PGlite/restart check on Linux, Linux ARM,
        macOS-latest, and macOS Intel, plus the package and clean-consumer
        gates; current-tip policy run <code>35641832862</code> passed the
        positive bounded-TTL and fail-closed configuration fixtures. A local
        disk-backed rehearsal emitted
        <code>PGLITE_BACKUP_RESTORE_ROLLBACK_PASS</code>, but that is
        supporting evidence rather than production backup or rollback
        approval. Hosted qualification run <code>35635114595</code> at
        <code>d2db74dd</code> is the recorded source for the cross-platform
        packet. The provider remains a Preview/NO-GO deployment boundary until
        persistent production storage, restore RPO/RTO, observability,
        ownership, and release approval are recorded.
      </>
    ),
    sources: [
      { label: 'PGlite provider source', href: 'https://github.com/andymac4182/mount-rs/blob/main/integrations/mount-rs-pglite/src/storage.rs' },
      { label: 'Node split-store example', href: 'https://github.com/andymac4182/mount-rs/blob/main/README.md#node-split-store-api' },
      { label: 'Provider matrix and Node CLI', href: 'https://github.com/andymac4182/mount-rs/blob/main/tests/provider_matrix/cli.mjs' },
      { label: 'PGlite progress ledger', href: 'https://github.com/andymac4182/mount-rs/blob/main/docs/w04-progress-ledger.md' },
      { label: 'Hosted W04 qualification', href: 'https://github.com/andymac4182/mount-rs/actions/runs/35635114595' },
      { label: 'Current W04 exact-tip qualification', href: 'https://github.com/andymac4182/mount-rs/actions/runs/35673738166' },
      { label: 'PGlite production rollout', href: 'https://github.com/andymac4182/mount-rs/blob/main/docs/W04-production-rollout.md' },
    ],
  },
  r2: {
    slug: 'r2',
    name: 'Cloudflare R2 / S3-compatible blocks',
    eyebrow: 'Provider / object storage',
    maturity: 'Validated',
    maturityNote: 'Authenticated live R2 block, configuration-driven CLI, both metadata-provider composition gates, the public-NAPI benchmark, and a complete budgeted hosted live-R2 workflow passed; broader native and release gates remain separate.',
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
        deployment into a universal power-loss claim. When the selected driver
        advertises <code>durable_writes</code>, successful mutations now await
        its <code>syncfs</code> barrier and return an error if that barrier
        fails; volatile drivers remain unchanged. This is a local mutation
        acknowledgment boundary, not proof of provider or power-loss
        durability. The subsequent local S3 qualification packet at exact
        revision <code>71972b28</code> passed the release N-API build, 990
        pinned-oracle tests with 79 explicit skips, 4 unit, 6 chunked, 29
        gateway, and 5 public-API Rust tests, 64-way direct-session/CAS
        concurrency, process restart, structural-factory parity, and the
        40-case S3/WebDAV HTTP differential. It remains local/oracle evidence;
        the follow-up local hook packet at <code>d43f5ea</code> also wires Rust
        <code>S3SessionHooks</code> and the N-API <code>now</code>,
        <code>requestId</code>, <code>onError</code>, and
        <code>onAssertion</code> controls with callback keepalive/release;
        its callback-observability, package, Rust, and strict-Clippy checks
        passed. These remain local controls, not live-provider acceptance. The
        latest AWS admission <code>35693941024</code> on published source
        <code>fcf1d547</code> stopped at
        <code>AWS_S3_CI_CONFIG_BLOCKED missing_bucket</code>. The latest R2
        admission <code>35693941037</code> on the same source stopped at
        <code>count=329 limit=20</code> and skipped its live integration job,
        so neither produced a live service PASS. The current S3 transport
        packet also closes bounded HEAD Content-Length/no-body, chunked PUT
        <code>411 MissingContentLength</code>, <code>Expect: 100-continue</code>,
        pipelined-response ordering, short streamed-response, and rejected-body
        keep-alive reuse cases; the full local Rust packet passed 40 gateway
        cases alongside strict Clippy, formatting, and diff checks.
        The same packet also proves bounded graceful close for an in-flight
        response. These are
        local transport boundaries, not live-provider acceptance.
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
        the authenticated live gate. The main-only workflow admits at most 20
        runs per month under a worst-case $100 monthly envelope (up to $4 per
        run). Hosted run <code>35579174675</code> completed the full R2 packet
        after the earlier <code>35575940720</code> ESTALE failure; this still
        does not establish native mount, power-loss, or full release
        acceptance.
      </>
    ),
    evidence: (
      <>
        The current tracker records authenticated R2 filesystem checks,
        five-seed differential traces, Node and CLI coverage, ranged reads,
        reopen, owned-prefix cleanup, and both supported metadata-provider
        compositions. The latest isolated rerun also passed the signed-HTTP
        adapter contract (10/10 unit tests and 1/1 HTTP test), including eight
        concurrent publications, prefix isolation, and exact cleanup; its live
        Cloudflare rows stopped at credential preflight with
        <code>R2_ENDPOINT</code> unset. The portable provider matrix also adds
        seeded positional writes, truncate, flush, and reopen checks. Local
        object-store tests and RustFS results are not substituted for recorded
        live Cloudflare results. The current live rerun additionally passed
        the complete Node/N-API suite with live R2 and isolated PGlite, all
        <code>3,105/3,105</code> operations across five seeded differential
        traces, the configuration-driven live-R2 CLI, and the public-NAPI
        benchmark's <code>8/8</code> iterations across 1, 4, 10, and 16 MiB
        with fixed 64 KiB chunks and verified cleanup. Hosted CI does not
        receive the dedicated R2 credentials, so those results remain a
        separately authenticated acceptance boundary. The repository now also
        records an exact-current local packet at <code>db431f4</code> with
        Rust SDK <code>6/3/0</code>, Node SDK <code>5/3/0</code>, CLI
        <code>12/2</code>, upstream <code>1200 passed / 82 skipped</code>, and
        all 40 five-seed/eight-backend traces at 621 operations; provider rows
        without credentials remain explicit skips.
        It wires a budget-gated <code>Live Cloudflare R2</code> workflow with
        pinned Rust/Node tooling, a pinned mountx checkout, AWS CLI cleanup,
        the full Rust/Node/CLI/PGlite/trace packet, and an uploaded benchmark
        artifact. Hosted run <code>35579174675</code> on revision
        <code>c0aa081</code> passed the budget gate, Rust backend, PGlite
        SDK/CLI, the bounded remote R2 trace for seed <code>4182</code> with
        <code>621</code> operations, Rust CLI, Node SDK, and service-evidence
        cleanup, emitting <code>CLOUDFLARE_R2_FULL_PASS</code>. Its local
        five-seed differential packet also passed all <code>3,105/3,105</code>
        operations, and the benchmark artifact was uploaded. This supersedes
        the earlier <code>35575940720</code> ESTALE failure for hosted
        acceptance while preserving that failure as historical evidence.
        The newer workflow admission run <code>35670469503</code> at
        <code>d570ab4</code> passed trigger coverage but stopped at the hosted
        monthly budget guard with <code>count=243 limit=20</code>; it provides
        no new live-R2 pass. The transport tracker also records 64 concurrent
        unique-object PUTs followed by 64 concurrent GETs across two
        session-owned buckets with exact readback, matching counters, and no
        retained assertions; this is same-process N-API session evidence, not
        live-provider or power-loss durability. The newer R2 admission run
        <code>35675116591</code> passed trigger coverage but failed the bounded
        usage admission, so its live job was skipped. Local process-restart
        evidence in the S3 tracker is deliberately not promoted to live R2
        durability.
      </>
    ),
    sources: [
      { label: 'R2 block adapter', href: 'https://github.com/andymac4182/mount-rs/blob/main/integrations/mount-rs-r2/src/blocks.rs' },
      { label: 'Live R2 evidence in the tracker', href: 'https://github.com/andymac4182/mount-rs/blob/main/WORK_TRACKER.md#-w05--cloudflare-r2' },
      { label: 'Configuration-driven provider matrix', href: 'https://github.com/andymac4182/mount-rs/blob/main/tests/provider_matrix/config-pglite-r2.json' },
      { label: 'Budgeted live-R2 workflow', href: 'https://github.com/andymac4182/mount-rs/blob/main/.github/workflows/cloudflare-r2.yml' },
      { label: 'R2 progress ledger', href: 'https://github.com/andymac4182/mount-rs/blob/main/docs/w05-progress-ledger.md' },
      { label: 'S3 transport durability boundary', href: 'https://github.com/andymac4182/mount-rs/blob/main/docs/W01_S3_PROGRESS.md' },
      { label: 'Hosted R2 acceptance run', href: 'https://github.com/andymac4182/mount-rs/actions/runs/35579174675' },
      { label: 'Hosted R2 benchmark artifact', href: 'https://github.com/andymac4182/mount-rs/actions/runs/35579174675/artifacts/10630750055' },
      { label: 'Latest hosted R2 admission', href: 'https://github.com/andymac4182/mount-rs/actions/runs/35693941037' },
      { label: 'S3 session-hook change', href: 'https://github.com/andymac4182/mount-rs/commit/d43f5ea4' },
    ],
  },
  rustfs: {
    slug: 'rustfs',
    name: 'RustFS',
    eyebrow: 'Provider / local S3-compatible service',
    maturity: 'Validated',
    maturityNote: 'Pinned service contract, restart, CAS, range, hosted RustFS checks, and real FoundationDB/TiDB composition checkpoints passed; bounded TiDB/RustFS Node and CLI configuration gates are wired, while replicated topology and credential-gated live consumer coverage remain open.',
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
        qualification remain tracked separately. The TiDB/RustFS consumer rows
        require <code>MOUNT_RS_TIDB_URL</code> and loopback RustFS credentials;
        without them, those rows remain explicit skips.
      </>
    ),
    evidence: (
      <>
        The bounded current-tree harness passed immutable block writes/reopens,
        SQLite and PGlite metadata compositions, N-API factories, remote CLI
        HTTP reopen, SQLite VFS over RustFS blocks, fault recovery, service
        restart/reopen, and the RustFS benchmark. FoundationDB and TiDB mixed
        provider runs also passed, with TiDB limited to single-node v8.5.7. The
        bounded consumer slice now wires TiDB metadata with RustFS/S3-compatible
        <code>r2</code> blocks through the public Rust SDK, N-API, and both
        CLIs, covering configuration, partial write, truncate, shutdown/reopen,
        and owned-prefix cleanup. Its local matrix records Node
        <code>pass=4 skip=3 fail=0</code> and CLI
        <code>pass=10 skip=3 fail=0</code>; live mixed-store rows remain
        credential-gated. The maturity label is scoped to these service paths,
        not every S3-compatible server or a replicated production topology.
      </>
    ),
    sources: [
      { label: 'RustFS requirements and harness boundary', href: 'https://github.com/andymac4182/mount-rs/blob/main/REQUIREMENTS.md#rustfs-integration-test-service' },
      { label: 'RustFS evidence in the tracker', href: 'https://github.com/andymac4182/mount-rs/blob/main/WORK_TRACKER.md#-w06--rustfs-integration-service' },
      { label: 'TiDB/RustFS consumer matrix', href: 'https://github.com/andymac4182/mount-rs/blob/main/WORK_TRACKER.md#-w08--tidb' },
    ],
  },
  tidb: {
    slug: 'tidb',
    name: 'TiDB',
    eyebrow: 'Provider / distributed SQL',
    maturity: 'Preview',
    maturityNote: 'Functional qualification is complete for the defined hosted TiDB/RustFS scope, including durable 3PD/3TiKV restart, fencing/ambiguous commit, and Linux/macOS consumer rows; production topology, IAM, DR, SLO/capacity, release, canary, and rollback gates remain open.',
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
        topology and provider restart promotion remain capacity-gated. A bounded
        Node/CLI consumer slice now covers configuration, partial write,
        truncate, shutdown/reopen, and owned RustFS-prefix cleanup. The
        terminal W08 hosted packet qualifies the defined Linux/macOS consumer
        and durable-service scope, but native mount beyond that matrix and
        production deployment remain separate. Its live TiDB/RustFS rows
        require <code>MOUNT_RS_TIDB_URL</code> and loopback RustFS credentials;
        do not treat those credentials or hosted fixtures as production IAM.
      </>
    ),
    evidence: (
      <>
        W08 is 100% complete for its defined functional acceptance scope:
        terminal hosted jobs qualify the durable 3PD/3TiKV restart sequence,
        provider fencing and ambiguous-commit behavior, live Linux TiDB/RustFS
        Node/CLI/FUSE composition, ARM Node, Ubuntu NFS, and macOS native-NFS
        rows. Authoritative hosted runs include
        <code>35624385556</code>, <code>35627761501</code>, and
        <code>35631063978</code>; each remains revision- and scope-specific.
        Production is still NO-GO: topology, secret management/IAM, backup and
        restore, upgrade/rollback, SLOs/capacity, security sign-off, on-call,
        canary, and release-owner approval remain open. A green functional
        packet is not production deployment evidence.
      </>
    ),
    sources: [
      { label: 'TiDB provider README', href: 'https://github.com/andymac4182/mount-rs/blob/main/integrations/mount-rs-tidb/README.md' },
      { label: 'TiDB provider source', href: 'https://github.com/andymac4182/mount-rs/blob/main/integrations/mount-rs-tidb/src/storage.rs' },
      { label: 'TiDB/RustFS consumer matrix', href: 'https://github.com/andymac4182/mount-rs/blob/main/WORK_TRACKER.md#-w08--tidb' },
      { label: 'Durable Ozone/TiDB CI gate', href: 'https://github.com/andymac4182/mount-rs/blob/main/.github/workflows/ci.yml' },
      { label: 'TiDB progress ledger', href: 'https://github.com/andymac4182/mount-rs/blob/main/docs/W08-progress-ledger.md' },
      { label: 'TiDB production rollout contract', href: 'https://github.com/andymac4182/mount-rs/blob/main/docs/W08-production-rollout.md' },
    ],
  },
  foundationdb: {
    slug: 'foundationdb',
    name: 'FoundationDB',
    eyebrow: 'Provider / transactional key-value store',
    maturity: 'Experimental',
    maturityNote: 'Real 7.4.7 provider and RustFS composition checkpoints, protected shared lease-authority code, shared-provider consumer selection, and bounded hosted Linux qualification now exist; production identity, recovery, capacity, platform, and release evidence remain open.',
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
        cluster. The guarded <code>with_production_lease_oracle</code> entry
        point rejects unverified, development, and single-authority clocks
        unless <code>LeaseOracle::authority_kind</code> declares
        <code>SharedProvider</code>; that declaration is a trust boundary, not
        proof of distributed safety. The integration now exposes a write-side
        <code>FoundationDbLeaseAuthority</code> and a read-only
        <code>FoundationDbSharedLeaseOracle</code>. The authority publishes
        monotonic provider time under its protected keyspace; workers fail
        closed when the record is absent or unavailable and never fall back to
        a local clock. Consumer configuration selects
        <code>lease_authority: "shared-provider"</code> with an explicit
        <code>authority_prefix</code>; the persisted single-authority mode is
        reserved for an owned test cluster. Production deployment must enforce
        one write-capable authority identity per prefix, read-only consumer
        credentials, a monitored clock-skew bound, and publication more often
        than the smallest lease TTL; authority time must be republished after
        authority restart. These controls are deployment contracts, not
        permissions created by the library. The authority prefix is also
        checked against FoundationDB's key-size limit at construction, so an
        oversized prefix fails before its first transaction.
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
        client and cluster. The production path now fails closed for
        unverified, development, and single-authority clocks. Hosted runtime,
        deployment-enforced read-only authority credentials,
        multi-host clock-skew/recovery controls beyond the tested topology,
        production identity/ACL/TLS, backup/restore, capacity, multi-day soak,
        failover rehearsal, hosted root integration, and broader platform
        coverage remain open;
        commit versions are not wall-clock expiry. The shared-provider Rust
        SDK/CLI and Node selection paths are opt-in native features, not
        portable-default support, and the authority service must enforce the
        read-only worker boundary outside the library API. The durable Ozone
        composition is loopback/non-secure test deployment evidence; it does
        not establish production authentication, TLS, or power-loss durability.
      </>
    ),
    evidence: (
      <>
        The real pinned Linux ARM64 provider and RustFS composition checks cover
        blocks, metadata, CAS, fencing, reopen, and the latest exact
        <code>owned-prefix cleanup</code> with sibling/parent sentinel
        preservation. The real
        cluster authority test covers two independent readers, an absent-
        authority fail-closed result, backward-sample clamping, forward
        recovery, and stale-writer fencing. The RustFS composition now
        publishes the authority, opens consumers through the read-only oracle,
        republishes after the owned FoundationDB restart, and reopens from a
        fresh client. A hosted FoundationDB lane now builds the feature-enabled
        Node addon and can run the configuration-driven Linux FUSE CLI lifecycle
        when <code>/dev/fuse</code> is present; the live Node gate and native CLI
        use a separately published shared-provider authority prefix. The macOS
        job compiles the FoundationDB-enabled CLI NFS test, but live macOS
        service/cluster acceptance and hosted results remain open. The latest
        arm64 durable Ozone composition created three fixed-address FoundationDB
        7.4.7 servers with three coordinators, separate persistent volumes, and
        <code>double</code>/<code>SSD</code> configuration. Transactional
        readiness passed before and after restarting replicated node 2; first
        and fresh-client Ozone compositions emitted
        <code>FOUNDATIONDB_RUSTFS_CHUNKED_PASS</code> and
        <code>FOUNDATIONDB_RUSTFS_SERVICE_RESTART_PASS</code>, followed by
        <code>FOUNDATIONDB_TEST_PASS topology=durable</code> and owned cleanup.
        This is real multi-node restart evidence, not production auth/TLS or
        power-loss proof. Hosted run <code>35636591071</code> at revision
        <code>97b63aed</code> then passed a bounded five-round Linux
        FoundationDB/RustFS qualification with live Node/N-API, native Linux
        CLI/FUSE mount and reopen, service restart, and schema-2/provenance
        evidence; its artifact is
        <code>foundationdb-production-qualification-35636591071-1</code>
        (SHA-256
        <code>123c2c5ece757fda141342d2b8e415e34269fff4246f5c3a8c147902415c137d</code>).
        The exact-current-main run <code>35638932960</code> at revision
        <code>3817efc9</code> passed the same bounded packet with
        <code>p50_us=9751</code>, <code>p95_us=28824</code>,
        <code>p99_us=28824</code>, and
        <code>throughput_ops_per_sec=99.31</code>; its artifact is
        <code>foundationdb-production-qualification-35638932960-1</code>
        (SHA-256
        <code>c474b5ef9275ef88daf73e7fe90e36bd25eba8d149ac6849edfc672217ef4bd0</code>).
        The latest mainline run <code>35652638242</code> at revision
        <code>c6f0039</code> also passed the explicit lease-TTL policy fixtures,
        durable three-server composition, five isolated soak rounds, live
        Node/N-API, native Linux CLI/FUSE mount and reopen, service restart,
        and schema-2 provenance validation. Its base marker recorded
        <code>p50_us=7712</code>, <code>p95_us=228723</code>,
        <code>p99_us=228723</code>, and
        <code>throughput_ops_per_sec=42.41</code>; the five-round throughput
        range was 77.45–95.05 ops/s. The retained artifact is
        <code>foundationdb-production-qualification-35652638242-1</code>
        with SHA-256
        <code>366cb78d19bc5182a438a8459ebfe54efc510232e9dab667a815aba5eacfbee7</code>.
        The newer mainline run <code>35657842924</code> at revision
        <code>87a500b1</code> also passed the explicit lease-publication marker
        policy, five isolated soak rounds, live Node/N-API, native Linux
        CLI/FUSE mount and reopen, service restart, and schema-2 provenance
        validation. Its base marker recorded <code>p50_us=9426</code>,
        <code>p95_us=29053</code>, <code>p99_us=29053</code>, and
        <code>throughput_ops_per_sec=95.22</code>; soak p95/p99 ranged from
        27,345µs to 29,175µs and throughput from 85.77 to 94.49 ops/s.
        The retained artifact is
        <code>foundationdb-production-qualification-35657842924-1</code>
        with SHA-256
        <code>dc37b10f82ecd1c4c4513f4cf3ecc9a95e2ef37ccdadf0f4d69e56d30620846b</code>.
        The latest requalified mainline run <code>35663914822</code> at
        revision <code>147c53a8</code> passed the same bounded policy,
        authority/consumer, restart, native Linux CLI/FUSE, and RustFS
        recovery packet after a lockfile repair. Its base marker recorded
        <code>p50_us=9564</code>, <code>p95_us=39516</code>,
        <code>p99_us=39516</code>, and
        <code>throughput_ops_per_sec=91.71</code>; soak p95/p99 ranged from
        21,487µs to 24,210µs and throughput from 91.97 to 106.58 ops/s.
        The retained artifact is
        <code>foundationdb-production-qualification-35663914822-1</code>
        with SHA-256
        <code>098ab3a4bb1fd6ae84971cebbb67baf2e50d9cb271accdaa9351ec28e49c3042</code>.
        The newer current-tip run <code>35666991514</code> at revision
        <code>5b9af323</code> also passed the same bounded policy, durable
        three-server composition, five isolated soak rounds, live Node/N-API,
        native Linux CLI/FUSE mount and reopen, service restart, and RustFS
        recovery packet. Its base marker recorded <code>p50_us=8101</code>,
        <code>p95_us=35120</code>, <code>p99_us=35120</code>, and
        <code>throughput_ops_per_sec=105.73</code>; soak p95/p99 ranged from
        20,100µs to 21,587µs and throughput from 107.93 to 119.84 ops/s. The
        retained artifact is
        <code>foundationdb-production-qualification-35666991514-1</code> with
        SHA-256
        <code>7313f7ce6f7fb188d84d79a5c5d98df01a1319f071208f1058c5ab87a72acb12</code>.
        The newer hosted run <code>35671492720</code> at revision
        <code>9460a62</code> passed the same guarded-authority packet with a
        base marker of <code>p50_us=3846</code>, <code>p95_us=38409</code>,
        <code>p99_us=38409</code>, and
        <code>throughput_ops_per_sec=155.90</code>; five-round soak throughput
        ranged from 199.39 to 214.47 ops/s. Its retained artifact is
        <code>foundationdb-production-qualification-35671492720-1</code> with
        SHA-256
        <code>815dfecadab366a9fca9a7501c4a36d771324d8f71b521e0e8082261cbe431c3</code>.
        The latest extended hosted qualification <code>35692674674</code> at
        exact revision <code>5a6d6507</code> passed the guarded-authority
        packet, durable three-server composition, live Node/N-API, Linux
        CLI/FUSE mount and reopen, service restart, and RustFS recovery checks
        across ten isolated soak rounds. Its bounded workload recorded 1,200
        successful lifecycle operations at <code>110.18</code> IOPS with zero
        timeouts and cleanup failures; the base p95/p99 was
        <code>439603</code> microseconds at <code>21.41</code> ops/s, while the
        ten-round soak ranged from <code>70204</code> to <code>236130</code>
        microseconds and <code>18.19</code> to <code>96.22</code> ops/s. The
        retained artifact is
        <code>foundationdb-production-qualification-35692674674-1</code> with
        SHA-256
        <code>44beef451832c307a53571bb86f92293b70c302d0add5104152059ed4c41d2ec</code>.
        The fixed-profile floor is structural rather than a production
        capacity target; identity/ACL/TLS, backup/restore, production
        capacity, multi-day operation, failover, macOS coverage, and release
        approval remain open.
        The W07 workflow now assembles a fail-closed cross-platform packet: the
        Linux qualification must pass alongside an independent macOS
        native-feature compile lane before it can emit
        <code>W07_PLATFORM_QUALIFICATION_PASS</code>. Hosted run
        <code>35696391744</code> at exact source <code>8834abd3</code> is still
        pending, so this adds a qualification boundary rather than macOS or
        cross-platform acceptance.
        These hosted results do not establish production identity/ACL/TLS,
        backup/restore, production capacity, multi-day operation, failover,
        macOS acceptance, or release approval.
      </>
    ),
    sources: [
      { label: 'FoundationDB integration README', href: 'https://github.com/andymac4182/mount-rs/blob/main/integrations/mount-rs-foundationdb/README.md' },
      { label: 'FoundationDB keyspace implementation', href: 'https://github.com/andymac4182/mount-rs/blob/main/integrations/mount-rs-foundationdb/src/lib.rs' },
      { label: 'FoundationDB workstream evidence', href: 'https://github.com/andymac4182/mount-rs/blob/main/WORK_TRACKER.md#-w07--foundationdb' },
      { label: 'Durable composition harness', href: 'https://github.com/andymac4182/mount-rs/blob/main/tests/foundationdb/README.md' },
      { label: 'Ozone durability progress ledger', href: 'https://github.com/andymac4182/mount-rs/blob/main/docs/w26-progress-ledger.md' },
      { label: 'Latest hosted FoundationDB qualification', href: 'https://github.com/andymac4182/mount-rs/actions/runs/35692674674' },
      { label: 'Latest W07 cross-platform qualification (pending)', href: 'https://github.com/andymac4182/mount-rs/actions/runs/35696391744' },
    ],
  },
  'aws-s3': {
    slug: 'aws-s3',
    name: 'AWS S3',
    eyebrow: 'Provider / remote object storage',
    maturity: 'Validated',
    maturityNote: 'The first-class Rust SDK/CLI AWS S3 block provider passed the live myroot private-bucket gate with a prefix-scoped assumed role, composed SQLite metadata, process reopen, and owned-prefix cleanup. The repository now also defines an operations runbook and optional S3Session::stats() telemetry boundary; production identity rotation, metadata ownership, recovery drills, collectors/alerts, canary/rollback, and hosted release gates remain separate.',
    summary: (
      <>
        AWS S3 is a first-class block provider using the actual AWS region and
        the standard AWS workload credential chain. It is qualified through a
        private bucket and prefix-scoped role; the provider does not silently
        treat an S3-compatible endpoint as AWS evidence.
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
        The layout is an owned prefix containing one immutable object per
        fixed-size chunk, inspected through S3 GET/HEAD/LIST and ranged reads.
        Conditional create, ETag CAS, and cleanup are verified against the
        actual AWS endpoint by the W25 gate. The S3 gateway's streaming PUT
        and multipart completion paths stage under private
        <code>.mountx-put-...</code> keys and publish with atomic rename;
        staging entries are hidden from ordinary listings.
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
        AWS deployment. The adapter uses signed requests, create-only
        immutable publication, conditional updates, completed upload barriers,
        and a durable-driver <code>syncfs</code> acknowledgment when
        <code>durable_writes</code> is enabled. Streaming PUT, multipart completion, and CopyObject use
        bounded staging with <code>read_chunk_bytes</code>, so a failed
        integrity check or source read does not replace an existing destination
        before the final rename. Multi-writer scope and metadata-provider
        durability still belong to the selected deployment.
      </>
    ),
    inspectLabel: 'Inspect an AWS-owned prefix without exposing credentials',
    inspectCode: `export AWS_REGION='ap-southeast-2'
export AWS_S3_BUCKET='private-bucket'
export AWS_S3_PREFIX='mount-rs/volume-a'

aws s3api list-objects-v2 --bucket "$AWS_S3_BUCKET" \
  --prefix "$AWS_S3_PREFIX/"
aws s3api head-object --bucket "$AWS_S3_BUCKET" \
  --key "$AWS_S3_PREFIX/<block-id>"
aws s3api get-object --bucket "$AWS_S3_BUCKET" \
  --key "$AWS_S3_PREFIX/<block-id>" --range bytes=0-63 /tmp/block-prefix.bin`,
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
        AWS S3 is block-only: choose an independent metadata provider, and do
        not infer multi-writer or power-loss guarantees from the SQLite test
        composition. Production deployments still need explicit metadata
        durability, backup/restore, monitoring, cost/retention, and hosted
        release qualification. The companion operations runbook is a handoff
        and drill contract, not proof that those production gates have passed.
        The bounded publication path requires a driver
        with <code>atomic_rename</code>; unsupported drivers return an explicit
        <code>NotImplemented</code> response rather than falling back to a
        weaker direct write. Use the AWS workload identity chain rather than
        committing long-lived access keys.
      </>
    ),
    evidence: (
      <>
        The 2026-09-21 W25 gate used the private
        <code>myroot</code> bucket in <code>ap-southeast-2</code> and a
        short-lived prefix-scoped role. It passed the Rust SDK and Rust CLI
        public configuration paths, immutable create and duplicate rejection,
        byte ranges, stale conditional/CAS rejection, current-ETag CAS, a
        65,537-byte boundary read, four concurrent writers, multi-chunk
        overwrite/truncate/extend/sparse-tail behavior, fresh-process reopen,
        and owner-verified cleanup with zero remaining objects. Cloudflare R2
        and RustFS results remain separate providers. Separately, the
        2026-09-21 S3 gateway hardening sequence added bounded staged PUT,
        multipart completion, and cross-driver CopyObject publication, with
        existing-object preservation on integrity/read failure. That is
        implementation and rootless gateway-test evidence, not a new hosted
        AWS durability or release-qualification claim. The current local
        S3/N-API packet also covers streamed async-iterable
        and <code>ReadableStream</code> bodies, typed peer-fault callbacks,
        cancellation-safe private staging, replacement-session multipart
        completion, and native-filesystem process-restart recovery. The
        replacement-session and restart checks preserve the boundary between
        local filesystem evidence and live AWS durability; neither promotes
        these results to hosted or power-loss acceptance. The companion
        operations runbook defines provider/reconciliation/identity/capacity
        signals,
        redaction boundaries, and O01–O08 drills, including identity expiry,
        conditional conflicts, throttling, metadata outage, restore, schema
        migration, cleanup/retention, and canary/rollback. It explicitly keeps
        production collector and alert routing, workload-identity rotation,
        approved metadata ownership, staging drills, canary, rollback, and
        post-deploy smoke open. The latest hosted qualification run
        <code>35629600687</code> at <code>62383df</code> passed provenance and
        the four synthetic contract suites, then failed closed at
        <code>AWS_S3_CI_CONFIG_BLOCKED missing_bucket</code> before AWS
        authentication; its retained artifact is
        <code>aws-s3-qualification-35629600687-1</code>. It does not close AWS
        identity or production acceptance. A newer hosted safety-preflight run
        <code>35635498647</code> at <code>24408f8</code> also stopped before
        authentication with <code>AWS_S3_CI_CONFIG_BLOCKED
        missing_bucket</code>; protected bucket, region, account, versioning,
        and role inputs were blank. This is a current configuration refusal,
        not a provider failure or acceptance result. The latest authorized
        qualification at pushed source <code>870348184b5a047faea01f584a68cb961b34f810</code>
        passed sibling-prefix denial, the public SDK/CLI self-test, composed
        AWS S3 filesystem, process reopen, independent PGlite metadata,
        writer fencing, PGlite backup/restore, fresh-server reopen, and exact
        owned-prefix cleanup, emitting <code>AWS_S3_TEST_PASS</code> and
        <code>AWS_S3_PGLITE_TEST_PASS</code>. This refreshes qualification-
        account evidence only; production metadata ownership, identity,
        recovery, and operational sign-off remain open. The current sealed
        W25 source scan reported zero reportable findings across its six
        scoped surfaces, with the rest of the repository and live AWS/GitHub
        state explicitly deferred. The latest read-only OIDC audit at source
        <code>9b5acfbac3d88d5f17a969defd447f5d44ee3023</code> remained
        fail-closed for missing environment protection, protected inputs,
        GitHub OIDC provider, and immutable-subject role trust; it made no
        changes. The latest qualification-bucket audit repeated the account,
        region, public-access, ownership, encryption, versioning, lifecycle,
        and multipart-abort controls without mutating AWS. These are useful
        rollout controls, not production resource or identity approval. The
        newer provider-workflow admission run <code>35669918721</code> at
        <code>c2df980</code> passed trigger coverage and static contracts but
        failed closed before AWS authentication at
        <code>AWS_S3_CI_CONFIG_BLOCKED missing_bucket</code>. The local
        <code>process.abort()</code> multipart restart check is
        native-filesystem evidence only; neither result changes the live AWS
        or power-loss acceptance boundary. The latest admission
        <code>35693941024</code> at published source <code>fcf1d547</code>
        reached the same protected-config validator and stopped at
        <code>AWS_S3_CI_CONFIG_BLOCKED missing_bucket</code> before AWS
        authentication. No live AWS service PASS is claimable until the
        protected bucket, region, account, versioning, and OIDC role inputs are
        provisioned.
      </>
    ),
    sources: [
      { label: 'AWS S3 workstream', href: 'https://github.com/andymac4182/mount-rs/blob/main/WORK_TRACKER.md#-w25--actual-aws-s3-integration' },
      { label: 'AWS S3 production rollout checklist', href: 'https://github.com/andymac4182/mount-rs/blob/main/docs/aws-s3-production-rollout.md' },
      { label: 'AWS S3 operations runbook', href: 'https://github.com/andymac4182/mount-rs/blob/main/docs/aws-s3-operations-runbook.md' },
      { label: 'Latest hosted AWS S3 admission', href: 'https://github.com/andymac4182/mount-rs/actions/runs/35693941024' },
      { label: 'Latest AWS S3 qualification record', href: 'https://github.com/andymac4182/mount-rs/blob/main/docs/aws-s3-production-rollout.md' },
      { label: 'S3 gateway publication contract', href: 'https://github.com/andymac4182/mount-rs/blob/main/transports/mount-rs-s3/README.md' },
      { label: 'S3 transport durability boundary', href: 'https://github.com/andymac4182/mount-rs/blob/main/docs/W01_S3_PROGRESS.md' },
      { label: 'Staged publication change', href: 'https://github.com/andymac4182/mount-rs/commit/74f1cd5406001e88b39ef91b5d6b9bef5b560015' },
      { label: 'Bounded CopyObject change', href: 'https://github.com/andymac4182/mount-rs/commit/165f3690e4c4e23bf5118870ba1cfff0abf6083a' },
      { label: 'S3-compatible block adapter', href: 'https://github.com/andymac4182/mount-rs/blob/main/integrations/mount-rs-r2/src/blocks.rs' },
    ],
  },
  ozone: {
    slug: 'ozone',
    name: 'Apache Ozone',
    eyebrow: 'Provider / S3-compatible gateway',
    maturity: 'Experimental',
    maturityNote: 'Pinned 2.2.1 gateway and arm64 block/restart/CAS/range evidence exist; the latest exact-tip hosted packet is diagnostic after hard 1,000-IOPS misses, while customer topology, backup/DR, secure tenancy, and release gates remain external.',
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
        The current arm64 gateway run passed create-only publication and
        duplicate rejection, ETag stale-read/stale-write rejection and CAS,
        concurrent writers, full/range reads, missing objects, binary payloads,
        a bounded stopped-gateway failure, service restart/reopen, and owned
        cleanup. The actual cluster's replication, Ratis, auth, TLS, and
        failure policy are not inferred from that single-node or all-in-one
        run.
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
        The latest revision-bound hosted packet
        <code>35635486040</code> completed its base Ozone job, but all four
        configured provider rows missed the hard 1,000-IOPS target: SQLite/R2
        <code>61.97</code>, PGlite/R2 <code>63.56</code>, TiDB/R2
        <code>14.14</code>, and FoundationDB/R2 <code>23.07</code>. Each row
        still completed 1,200/1,200 lifecycle operations with zero timeouts and
        cleanup failures; the aggregate correctly failed closed because no
        provider emitted <code>OZONE_IOPS_PASS</code>. The failed packet is
        diagnostic only, not a retained acceptance result. The non-secure
        all-in-one service is loopback-only and is not production
        authentication or durability evidence; durable FoundationDB remains
        test-deployment evidence and does not imply production auth, TLS, or
        power-loss durability. W26.15 remains open after the first safe concurrency/publication
        remediation. Replacement run <code>35641941218</code> at exact W26 code
        head <code>88b707ba</code> completed all four provider rows with
        1,200/1,200 successful lifecycle operations and zero timeouts or cleanup
        failures, but missed the hard 1,000-IOPS target: SQLite/R2 measured
        <code>85.70</code>, PGlite/R2 <code>94.04</code>, TiDB/R2
        <code>14.50</code>, and FoundationDB/R2 <code>30.46</code>. The producer
        jobs and aggregate failed closed without <code>OZONE_IOPS_PASS</code>;
        this is diagnostic evidence, not a new acceptance result.
      </>
    ),
    evidence: (
      <>
        The current maturity is Experimental: the real arm64 gateway harness
        has the block, CAS, range, failure, restart/reopen, and cleanup
        checkpoint above. Its SQLite/PGlite compositions passed through the
        real Ozone gateway, and a separate single-node TiDB/Ozone run passed
        the direct TiDB contract, ChunkedFs partial/truncate/CAS/stale-fencing/
        reopen, ambiguous-commit, and cleanup checks. The arm64 Node provider
        and Node/Rust CLI run recorded <code>pass=7 skip=1 fail=0</code>. A
        durable FoundationDB/Ozone run also passed with three fixed-address
        FoundationDB 7.4.7 servers, persistent volumes, double/SSD
        configuration, transaction readiness across a replicated-node restart,
        fresh-client reopen, and owned cleanup. A dedicated
        <code>ozone-tidb</code> hosted job is wired for durable v8.5.7 TiDB with
        Node 24 against the real Ozone gateway, but the current replacement job
        is queued and has no result yet. A
        dedicated hosted
        <code>ozone-compositions</code> job now installs PGlite, builds the
        public Node addon, and runs the real SQLite/PGlite mixed-metadata gate.
        Its opt-in consumer phase covers the Node provider matrix, Node CLI,
        and Rust CLI against live Ozone/PGlite/R2-compatible services, but the
        hosted result is still pending and revision-specific. Durable
        multi-node TiDB, secure/replicated Ozone deployment, and the broader
        backend matrix are not yet accepted. The retained W26 packet on
        <code>9c098e5</code> / hosted run <code>35585066458</code> remains the
        last accepted scoped result. The newer hosted packet
        <code>35635486040</code> completed all four configured provider
        lifecycles but failed the 1,000-IOPS gate; aggregate job
        <code>106458415293</code> rejected the missing pass marker, so its
        artifacts are diagnostic only. W26's credential-free rollout contract
        fixes the Tier 1 99.99% availability objective, 1,000 IOPS per drive,
        five-minute RPO/RTO, TLS/SigV4, tenant-scoped prefixes, and
        three-node/three-replica/three-domain topology; these are
        customer/Ozone requirements, not proof of a deployed environment.
        Replacement run <code>35641941218</code> is terminal for W26 even
        though unrelated native jobs kept the parent workflow active. Its
        producer jobs <code>106473112920</code>, <code>106473112915</code>, and
        <code>106473112409</code>, plus aggregate
        <code>106477164582</code>, failed closed on the hard IOPS gate. The
        local overlap/read/shutdown regression suite is green, but this first
        remediation is insufficient; secure customer topology, backup/DR,
        measured SLO/capacity, and release/canary/rollback remain open.
        The current publication-barrier implementation adds a conservative
        provider capability that skips only the redundant post-publish flush
        probe while retaining explicit <code>syncfs</code>; the focused
        ChunkedFs suite passes 20/20 and the full locked workspace/Clippy
        checks are green. Fresh hosted run <code>35672928117</code> targets
        exact SHA <code>fbc8346d</code>, but its Ozone producers were queued or
        in progress and no aggregate existed at the recorded snapshot, so it
        is not acceptance evidence. The later terminal diagnostic packet
        <code>35674425514</code> completed 1,200/1,200 lifecycle operations
        with zero timeouts and cleanup failures for every provider row, but
        measured only 637.01 IOPS for SQLite/R2, 457.65 for PGlite/R2,
        147.31 for TiDB/R2, and 126.50 for FoundationDB/R2 against the hard
        1,000-IOPS target; its aggregate failed closed without pass markers.
        The newer manual run <code>35683158821</code> targeted exact SHA
        <code>14dbf2c6</code> and is now terminal but diagnostic: SQLite/R2
        measured <code>754.59</code> IOPS, PGlite/R2 <code>817.10</code>,
        TiDB/R2 <code>143.04</code>, and FoundationDB/R2 <code>345.17</code>
        against the unchanged 1,000-IOPS target. Each provider completed
        1,200/1,200 lifecycle operations with zero timeouts and cleanup
        failures, but the provider jobs failed the hard threshold and the
        aggregate job <code>106606577835</code> failed closed on missing
        <code>OZONE_IOPS_PASS</code> markers. No acceptance or production
        claim is made from this run.
        The newer terminal packet <code>35686340751</code> selected exact SHA
        <code>1e64bc25</code>: base Ozone passed, SQLite/R2 measured
        <code>997.06</code> IOPS, PGlite/R2 <code>592.43</code>, and TiDB/R2
        <code>277.59</code>; FoundationDB produced no benchmark after its
        locked-preflight failure, and aggregate job
        <code>106616062969</code> failed closed. A fresh manual dispatch
        <code>35691451007</code> selected exact SHA <code>dccd8351</code> and
        remains diagnostic: SQLite/R2 measured <code>213.947686</code> IOPS,
        PGlite/R2 <code>2065.669446</code>, TiDB/R2 <code>333.356025</code>,
        and FoundationDB/R2 <code>363.254472</code>. Every row completed
        1,200/1,200 lifecycle operations with zero timeouts and cleanup
        failures. PGlite cleared the row target, but SQLite, TiDB, and
        FoundationDB missed the hard 1,000 IOPS threshold, so the aggregate
        failed closed without <code>OZONE_IOPS_PASS</code>. No acceptance or
        production claim is made from this run; the next work is
        correctness-preserving variance diagnosis and requalification.
      </>
    ),
    sources: [
      { label: 'Ozone acceptance requirements', href: 'https://github.com/andymac4182/mount-rs/blob/main/REQUIREMENTS.md#apache-ozone-backend-acceptance' },
      { label: 'Ozone workstream evidence', href: 'https://github.com/andymac4182/mount-rs/blob/main/WORK_TRACKER.md#-w26--apache-ozone-s3-backend' },
      { label: 'Ozone composition CI job', href: 'https://github.com/andymac4182/mount-rs/blob/main/.github/workflows/ci.yml' },
      { label: 'Durable FoundationDB composition harness', href: 'https://github.com/andymac4182/mount-rs/blob/main/tests/foundationdb/README.md' },
      { label: 'Ozone progress ledger', href: 'https://github.com/andymac4182/mount-rs/blob/main/docs/w26-progress-ledger.md' },
      { label: 'Ozone production rollout contract', href: 'https://github.com/andymac4182/mount-rs/blob/main/docs/w26-production-rollout.md' },
      { label: 'Historical hosted Ozone qualification', href: 'https://github.com/andymac4182/mount-rs/actions/runs/35635486040' },
      { label: 'Current Ozone remediation qualification', href: 'https://github.com/andymac4182/mount-rs/actions/runs/35641941218' },
      { label: 'Latest hosted Ozone qualification', href: 'https://github.com/andymac4182/mount-rs/actions/runs/35691451007' },
      { label: 'Previous hosted Ozone diagnostic', href: 'https://github.com/andymac4182/mount-rs/actions/runs/35688061634' },
      { label: 'Earlier hosted Ozone diagnostic', href: 'https://github.com/andymac4182/mount-rs/actions/runs/35686340751' },
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
