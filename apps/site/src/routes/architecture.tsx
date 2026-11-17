import { Link, createFileRoute } from '@tanstack/react-router'
import { LayerExplorer, NodeAnatomy, WritePathExplorer } from '../components/architecture-explorers'
import { ClusterSimulator } from '../components/cluster-simulator'

export const Route = createFileRoute('/architecture')({
  head: () => ({
    meta: [
      { title: 'Architecture — mount-rs' },
      {
        name: 'description',
        content:
          'How a mount-rs server handles requests, forwards them across a cluster, fails drives over between servers, caches metadata and blocks, and commits writes.',
      },
    ],
  }),
  component: ArchitecturePage,
})

const fastReasons = [
  {
    title: 'Metadata reads stay in memory',
    body: 'Each drive has exactly one owner holding a fenced writer lease. While the lease holds, the owner’s in-memory namespace is authoritative, so stat, head and list never wait on the database.',
  },
  {
    title: 'Any node, one hop',
    body: 'A request can land on any node. If that node does not own the drive it forwards once, over pooled HTTP/3 connections to the owner, and streams the response back. There is no second hop and no redirect to the client.',
  },
  {
    title: 'Routes are already known',
    body: 'Ingresses cache which node owns each drive and confirm active routes in the background, so forwarded requests skip the Redis lookup. On the cloud benchmark this lowered forwarded read p99 from 0.87 ms to 0.72 ms.',
  },
  {
    title: 'Leases renew off the request path',
    body: 'An owner renews its 240-second lease every 30 seconds in the background and serves within a 60-second window, instead of paying a lease round trip per operation. The same lease is what lets a standby take the drive over if the owner stops.',
  },
  {
    title: 'One statement per durable write',
    body: 'A durable mutation finishes its blocks, then commits the new revision with a single conditional update that checks both the revision and the writer fence. Nothing is locked across drives.',
  },
  {
    title: 'Small probes, not big reads',
    body: 'Checking whether a TiDB namespace changed reads a covering index entry, not the whole namespace row. That cut TiKV traffic per unchanged read from 1.74 MB to 4.76 KB.',
  },
  {
    title: 'Immutable, cacheable blocks',
    body: 'File bytes live in immutable blocks, cached in bounded RAM and disk tiers in front of the object store. Uploads stream their blocks to storage as they arrive, up to 5 GiB per file.',
  },
  {
    title: 'Writeback when you want it',
    body: 'Drives that can accept an acknowledgement before durability can use writeback: mutations are applied in memory and published later, with p99 under 1 ms even when forwarded.',
  },
]

function ArchitecturePage() {
  return (
    <>
      <section className="hero arch-hero-wrap">
        <div className="page-frame arch-hero">
          <p className="eyebrow">Architecture</p>
          <h1>
            How a mount-rs server <span className="grad-text">stays fast.</span>
          </h1>
          <p className="arch-lede">
            <code>mount-rs serve-http</code> serves drives over HTTP. Each drive
            is owned by one server at a time, which keeps the drive’s namespace in
            memory and commits changes to shared metadata and block storage. Any
            server can accept a request and forward it, in one hop, to the owner.
          </p>
          <div className="button-row">
            <a className="button button-primary" href="#cluster">
              Try the cluster
            </a>
            <Link className="button button-secondary" to="/benchmarks">
              See the benchmarks
            </Link>
          </div>
          <nav className="arch-toc" aria-label="On this page">
            <a href="#cluster">01 The cluster</a>
            <a href="#server">02 Inside a node</a>
            <a href="#caching">03 Caching</a>
            <a href="#writes">04 Writes</a>
            <a href="#why-fast">05 Why it is fast</a>
          </nav>
        </div>
      </section>

      <section className="page-frame arch-section" id="cluster">
        <div className="section-heading split-heading">
          <div>
            <p className="eyebrow">01 / a cluster</p>
            <h2>Forwarding: any node, one hop to the owner.</h2>
          </div>
          <p>
            Three nodes sit behind one load balancer. The load balancer does not
            know which server owns a drive: owners publish their drives in Redis,
            ingresses cache those routes, and every node holds open, mutually
            authenticated HTTP/3 connections to the others. Send a request and
            follow it, then stop a node and watch a standby take over its
            drive.
          </p>
        </div>
        <div className="callout callout-blue arch-callout">
          <p>
            <strong>Clustering is built into <code>serve-http</code>.</strong>{' '}
            A cluster needs three things: a membership file that pins every
            server’s key for mutual TLS, which servers re-read on{' '}
            <code>SIGHUP</code> or a timer, so nodes join and leave without a
            restart; a Redis route registry, reached over TLS with a password
            (plaintext is refused except to the same host unless you allow it);
            and metadata and block storage that every candidate owner of a
            drive can reach.
          </p>
          <p>
            In return, any node accepts any request, and with failover turned
            on a drive whose owner stops moves to a standby with no operator
            involved. Measured with real binaries, SQLite metadata and a
            20-second lease, the new owner was published about 21 seconds after{' '}
            <code>kill -9</code>, and within a second of a clean stop. With the
            default 240-second lease a crash takes about four minutes, because
            a standby cannot take a drive before the old lease expires.
          </p>
          <p>
            Limits: failover is opt-in and needs split-store drives; sessions
            and requests in flight on a failed owner are lost, not replayed;
            Redis Sentinel and Redis Cluster are not supported; and servers of
            different releases cannot share a cluster, because the
            server-to-server protocol is internal, so a cluster is upgraded by
            stopping every server. See the{' '}
            <a href="https://github.com/andymac4182/mount-rs/blob/main/docs/serve-http-failover.md">failover design</a>{' '}
            and the{' '}
            <a href="https://github.com/andymac4182/mount-rs/blob/main/apps/mount-rs-cli/README.md#remote-sessions-on-a-configured-http-server">cluster configuration</a>.
          </p>
        </div>
        <ClusterSimulator />
        <ol className="arch-steps">
          <li>
            <strong>Any node accepts the request.</strong> The client sends its
            drive token over HTTPS to whichever node the load balancer picks.
          </li>
          <li>
            <strong>The route is usually cached.</strong> On a miss, the ingress
            reads the drive’s route from Redis; concurrent misses for one drive
            share a single lookup. Active routes are re-confirmed in the
            background before they expire.
          </li>
          <li>
            <strong>One HTTP/3 hop.</strong> The request is streamed over one of
            the ingress’s pooled QUIC connections to the owner, authenticated
            in both directions with pinned keys from the membership file.
          </li>
          <li>
            <strong>The owner decides.</strong> It re-checks the original token,
            that the route still names it, and its current fencing generation,
            then streams the response back through the ingress.
          </li>
        </ol>
        <p className="storage-model-warning">
          A forwarded write whose outcome is unknown, because the connection
          broke after the owner may have received it, is reported as an error
          and never replayed. Stopping an owner withdraws its routes, so its
          drives answer 503 at once instead of after a connection timeout.
          While no owner is published the 503 carries{' '}
          <code>Retry-After: 1</code> and means nothing was executed. Without
          failover the drive stays unavailable until its owner is restarted.
          With it, a standby that reads no route opens the drive, which
          succeeds only once storage grants the writer lease, and then
          publishes a route with a higher fence. Redis decides nothing about
          who may write.
        </p>
      </section>

      <section className="page-frame arch-section" id="server">
        <div className="section-heading split-heading">
          <div>
            <p className="eyebrow">02 / one node</p>
            <h2>A request, from the socket to storage.</h2>
          </div>
          <p>
            The listener, token check and routing decision are the same on every
            node. What differs is whether this node owns the drive. Select any
            stage to see what it does and what it costs.
          </p>
        </div>
        <NodeAnatomy />
      </section>

      <section className="page-frame arch-section" id="caching">
        <div className="section-heading split-heading">
          <div>
            <p className="eyebrow">03 / caching</p>
            <h2>Seven layers between a request and the object store.</h2>
          </div>
          <p>
            Most requests are answered in the top three layers. Pick a request
            to see how far down it has to go, and open a layer to see what a hit
            there saves.
          </p>
        </div>
        <LayerExplorer />
      </section>

      <section className="page-frame arch-section" id="writes">
        <div className="section-heading split-heading">
          <div>
            <p className="eyebrow">04 / writes</p>
            <h2>Durable by default, writeback by choice.</h2>
          </div>
          <p>
            Blocks always reach storage before metadata refers to them, and
            every metadata commit carries the writer’s fence. Writeback moves
            the acknowledgement earlier; it does not change that order.
          </p>
        </div>
        <WritePathExplorer />
      </section>

      <section className="ink-section arch-fast architecture-fast" id="why-fast">
        <div className="page-frame">
          <div className="section-heading solution-heading">
            <p className="eyebrow eyebrow-light">What makes it fast</p>
            <h2>Remove round trips from the common path.</h2>
            <p>
              None of these needs a special client. They come from giving every
              drive a single owner and keeping each layer’s job narrow.
            </p>
          </div>
          <div className="arch-fast-grid">
            {fastReasons.map((reason, index) => (
              <article className="arch-fast-card" key={reason.title}>
                <span className="solution-card-number">{String(index + 1).padStart(2, '0')}</span>
                <h3>{reason.title}</h3>
                <p>{reason.body}</p>
              </article>
            ))}
          </div>
        </div>
      </section>

      <section className="page-frame arch-section arch-links">
        <div className="value-strip">
          <div>
            <p className="eyebrow">Go deeper</p>
            <h2>Read the measurements and the source.</h2>
          </div>
          <div className="value-list">
            <div>
              <strong>
                <Link to="/benchmarks">Benchmarks</Link>
              </strong>
              <span>100 drives, three servers, TiDB and RustFS, measured on GitHub’s runners.</span>
            </div>
            <div>
              <strong>
                <a href="https://github.com/andymac4182/mount-rs/blob/main/ARCHITECTURE.md">ARCHITECTURE.md</a>
              </strong>
              <span>Provider roles, write ordering and the concurrent-writer mode.</span>
            </div>
            <div>
              <strong>
                <a href="https://github.com/andymac4182/mount-rs/blob/main/docs/serve-http-runbook.md">serve-http runbook</a>
              </strong>
              <span>Configuration, TLS proxies, health checks, shutdown and recovery.</span>
            </div>
            <div>
              <strong>
                <a href="https://github.com/andymac4182/mount-rs/blob/main/docs/serve-http-failover.md">Failover design</a>
              </strong>
              <span>Candidates, states, timing bounds, what clients see and what is not handled.</span>
            </div>
          </div>
        </div>
      </section>
    </>
  )
}
