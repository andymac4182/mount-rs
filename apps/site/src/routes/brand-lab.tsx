import { Link, createFileRoute } from '@tanstack/react-router'

type BrandOption = {
  board: 'A' | 'B'
  number: string
  name: string
  description: string
  colors: readonly [string, string, string, string, string]
}

const paletteRoles = ['Ink', 'Surface', 'Primary', 'Secondary', 'Signal'] as const

const boardAOptions: readonly BrandOption[] = [
  { board: 'A', number: '01', name: 'Peakline', description: 'A mountain boundary with a bright locator at the mount point.', colors: ['#101a2d', '#f5f9ff', '#146bff', '#2949a8', '#25c8e9'] },
  { board: 'A', number: '02', name: 'Shardmark', description: 'A two-plane stack for a product that makes composition visible.', colors: ['#17191d', '#fff8ee', '#ff6a00', '#b94b16', '#f4a80a'] },
  { board: 'A', number: '03', name: 'Portline', description: 'An open gate for a filesystem contract that crosses providers.', colors: ['#0d1d2a', '#f2fbff', '#08bde5', '#157f9f', '#7ce8f4'] },
  { board: 'A', number: '04', name: 'Greenshift', description: 'A folded block with a low-noise, sustainable systems signal.', colors: ['#102218', '#f2fff4', '#62d326', '#078548', '#a4ec3e'] },
  { board: 'A', number: '05', name: 'Keywell', description: 'An object boundary with a single key that stays easy to find.', colors: ['#11202a', '#f1fbff', '#0bbce3', '#167c9d', '#72dff3'] },
  { board: 'A', number: '06', name: 'Coremesh', description: 'A central block held by four edges: simple, stable, direct.', colors: ['#101b2a', '#f4f8ff', '#126eff', '#1e48bd', '#9bc7ff'] },
  { board: 'A', number: '07', name: 'Bytegrid', description: 'A file plane beside a dense block index for data-heavy tooling.', colors: ['#1c1b1c', '#fff9f1', '#ff6a00', '#c14a16', '#ffbd66'] },
  { board: 'A', number: '08', name: 'Fluxstack', description: 'A purple layered mark for controlled movement across planes.', colors: ['#17142a', '#fbf7ff', '#7a3cff', '#4b20be', '#c4a9ff'] },
  { board: 'A', number: '09', name: 'Relay', description: 'A bidirectional transport mark for mount-free and mounted paths.', colors: ['#10202a', '#f0fcff', '#08c2e8', '#0b789b', '#8aebf4'] },
  { board: 'A', number: '10', name: 'Blockfold', description: 'A cube assembled from planes, with storage structure as the hero.', colors: ['#102018', '#f1fff4', '#05bc63', '#087d4a', '#83e74f'] },
  { board: 'A', number: '11', name: 'Anchor', description: 'A pinned plane for durable metadata and deliberate ownership.', colors: ['#26190f', '#fff8ed', '#ff6810', '#b54213', '#ffb454'] },
  { board: 'A', number: '12', name: 'Lockstep', description: 'A key over layers: coordinated metadata and block commits.', colors: ['#111d31', '#f3f8ff', '#116dff', '#2448b5', '#88bcff'] },
  { board: 'A', number: '13', name: 'Gatefold', description: 'An opening between planes for provider and transport composition.', colors: ['#0d1f2c', '#eefcff', '#0bc3ee', '#117d9c', '#81edf5'] },
  { board: 'A', number: '14', name: 'Vaultline', description: 'A protected record path for leases, manifests, and state.', colors: ['#1b1230', '#fbf7ff', '#7f42ff', '#4d22bc', '#d4c3ff'] },
  { board: 'A', number: '15', name: 'Splitplane', description: 'Two vertical planes that keep metadata and immutable bytes explicit.', colors: ['#21160f', '#fff8ef', '#ff6709', '#b84b17', '#ffc274'] },
  { board: 'A', number: '16', name: 'Lattice', description: 'A green layer edge for predictable, repeatable storage geometry.', colors: ['#102218', '#f1fff5', '#62d72b', '#07844a', '#b1f18a'] },
  { board: 'A', number: '17', name: 'Pathwell', description: 'A route bending through a stable mount boundary.', colors: ['#101b30', '#f2f8ff', '#146bff', '#2447aa', '#79b5ff'] },
  { board: 'A', number: '18', name: 'Gridmark', description: 'A single lit cell in a compact block map.', colors: ['#25180f', '#fff8ec', '#ff6a00', '#bf4a16', '#ffc476'] },
  { board: 'A', number: '19', name: 'Bridge', description: 'A linked pair for provider adapters and transport receipts.', colors: ['#0d202b', '#effcff', '#07c1ea', '#0b7f9f', '#8debf4'] },
  { board: 'A', number: '20', name: 'Strata', description: 'A high-contrast stack that makes layered storage legible.', colors: ['#17132b', '#fbf7ff', '#783cff', '#4b20af', '#c9b2ff'] },
]

const boardBOptions: readonly BrandOption[] = [
  { board: 'B', number: '01', name: 'Layerbase', description: 'A clean two-level foundation for a filesystem-first product.', colors: ['#101b2d', '#f1f6ff', '#216cf0', '#79aeea', '#d9e9ff'] },
  { board: 'B', number: '02', name: 'Forkpoint', description: 'A split boundary for choosing providers without changing the contract.', colors: ['#171a22', '#fff8ef', '#ff6a0a', '#f3a04a', '#ffe0bd'] },
  { board: 'B', number: '03', name: 'Slate', description: 'A monochrome stack for precise, quiet infrastructure tooling.', colors: ['#151c25', '#f4f6f8', '#4f5c6d', '#8792a0', '#d2d8df'] },
  { board: 'B', number: '04', name: 'Rackline', description: 'An active storage plane with a small, readable health signal.', colors: ['#101b2a', '#effdfb', '#08b7d3', '#55d8e8', '#c9f5f7'] },
  { board: 'B', number: '05', name: 'Ridgeline', description: 'An angular green silhouette for capacity and terrain-aware storage.', colors: ['#102218', '#f0fff5', '#1fbf62', '#76e6a0', '#d5f8df'] },
  { board: 'B', number: '06', name: 'Fetch', description: 'A direct pull gesture for retrieving bytes through a stable API.', colors: ['#102219', '#f0fff6', '#13b95a', '#7ce5a3', '#d2f9dd'] },
  { board: 'B', number: '07', name: 'Meshroute', description: 'A small graph for paths, adapters, and provider fan-out.', colors: ['#101d2f', '#f1f8ff', '#1479f3', '#6ca9ed', '#d8eaff'] },
  { board: 'B', number: '08', name: 'Shards', description: 'A block index with an orange write signal.', colors: ['#1d1b18', '#fff8f0', '#ff6b0a', '#f39a54', '#ffd9ad'] },
  { board: 'B', number: '09', name: 'Upstream', description: 'An upward flow for promotion, publication, and durable state.', colors: ['#1c1430', '#faf7ff', '#7441f2', '#b19af1', '#ece4ff'] },
  { board: 'B', number: '10', name: 'Archive', description: 'A curved handle over layers for a friendly long-term store.', colors: ['#21170f', '#fff8ee', '#ff670b', '#f2ad64', '#ffe5c5'] },
  { board: 'B', number: '11', name: 'Manifest', description: 'Two documents for schemas, receipts, and object metadata.', colors: ['#10202a', '#f1fbff', '#12aee4', '#7fd3ef', '#d6f2fc'] },
  { board: 'B', number: '12', name: 'Route', description: 'A reversible path for routing around provider-specific limits.', colors: ['#102318', '#effff5', '#20bc63', '#82e5a5', '#d8f9e1'] },
  { board: 'B', number: '13', name: 'Crate', description: 'A contained block for portable storage primitives.', colors: ['#101c2e', '#f0f7ff', '#126cf0', '#6aa5ed', '#d2e6ff'] },
  { board: 'B', number: '14', name: 'Index', description: 'A dashed layer boundary for planned and queryable state.', colors: ['#171c24', '#f4f6f8', '#5a6675', '#a2abb5', '#e0e4e8'] },
  { board: 'B', number: '15', name: 'Gateway', description: 'A bright center between two walls: access with a clear edge.', colors: ['#21170f', '#fff8ef', '#ff6b0a', '#f2af65', '#ffe2be'] },
  { board: 'B', number: '16', name: 'Focus', description: 'A target for the metadata plane and its ownership boundary.', colors: ['#1c1430', '#faf7ff', '#7c45f1', '#bba4f3', '#eee7ff'] },
  { board: 'B', number: '17', name: 'Orbit', description: 'A compact path from a source node to a live provider.', colors: ['#10202a', '#effcff', '#0db4d3', '#63dae9', '#d3f7fa'] },
  { board: 'B', number: '18', name: 'Queue', description: 'Ordered green records for work that crosses a transport boundary.', colors: ['#102319', '#effff5', '#18b95c', '#82e4a6', '#d4f8df'] },
  { board: 'B', number: '19', name: 'Controlplane', description: 'A matrix with one orange state for operational clarity.', colors: ['#171b22', '#f3f5f7', '#536071', '#a1aab4', '#ff6d16'] },
  { board: 'B', number: '20', name: 'Deepstore', description: 'A violet diamond stack for storage beneath the filesystem surface.', colors: ['#1b1530', '#faf7ff', '#7c45f1', '#b39cf0', '#ebe3ff'] },
]

const brandOptions = [...boardAOptions, ...boardBOptions]

export const Route = createFileRoute('/brand-lab')({
  head: () => ({
    meta: [
      { title: 'Brand lab — mount-rs' },
      {
        name: 'description',
        content:
          'Forty generated mount-rs logo directions with a palette system for every mark.',
      },
    ],
  }),
  component: BrandLabPage,
})

function BrandLabPage() {
  return (
    <main className="page-frame brand-lab">
      <div className="doc-kicker-row">
        <p className="eyebrow">Design lab / 40-option system</p>
        <span className="brand-lab-status">Review before implementation</span>
      </div>
      <h1>One logo, one project name, one colour system.</h1>
      <p className="brand-lab-lede">
        The mark exploration is now split into two generated boards of twenty.
        Each row below turns one mark into a named project direction and a
        practical five-role palette: ink, surface, primary, secondary, and
        signal.
      </p>

      <div className="callout callout-blue brand-lab-callout">
        <strong>Forty marks, with palettes ready to compare.</strong>
        <p>
          The earlier ten-direction brand board has been removed. These are
          still explorations, so the selected mark should be redrawn as a
          responsive SVG before it becomes the header mark or favicon.
        </p>
      </div>

      <section className="brand-lab-section" aria-labelledby="source-boards-heading">
        <div className="brand-lab-heading">
          <div>
            <p className="eyebrow">01 / Source boards</p>
            <h2 id="source-boards-heading">Two boards, forty starting points.</h2>
          </div>
          <p>Board A is the original dark/glow exploration. Board B is the revised flat mark set.</p>
        </div>
        <div className="brand-lab-source-grid">
          <figure className="brand-lab-source-figure">
            <img
              src="/brand-explorations/mount-rs-logo-board-a.webp"
              alt="Logo board A with twenty numbered mount-rs mark explorations in a five by four grid."
            />
            <figcaption>Board A / 01–20. Dark systems and luminous accents.</figcaption>
          </figure>
          <figure className="brand-lab-source-figure">
            <img
              src="/brand-explorations/mount-rs-palette-board-a.webp"
              alt="Palette board A with twenty numbered five-swatch colour systems matching logo board A."
            />
            <figcaption>Palette A / 01–20. Generated ink, surface, accent, and signal roles.</figcaption>
          </figure>
          <figure className="brand-lab-source-figure">
            <img
              src="/brand-explorations/mount-rs-logo-board-b.webp"
              alt="Logo board B with twenty numbered mount-rs mark explorations in a five by four grid."
            />
            <figcaption>Board B / 01–20. Flat, light-background mark studies.</figcaption>
          </figure>
          <figure className="brand-lab-source-figure">
            <img
              src="/brand-explorations/mount-rs-palette-board-b.webp"
              alt="Palette board B with twenty numbered five-swatch colour systems matching logo board B."
            />
            <figcaption>Palette B / 01–20. Generated light-surface brand systems.</figcaption>
          </figure>
        </div>
      </section>

      <section className="brand-lab-section" aria-labelledby="logo-systems-heading">
        <div className="brand-lab-heading">
          <div>
            <p className="eyebrow">02 / Forty logo systems</p>
            <h2 id="logo-systems-heading">Compare the row, not just the mark.</h2>
          </div>
          <p>Every row keeps the logo, project name, description, and colour board together for a fast design review.</p>
        </div>

        <div className="brand-option-list">
          {brandOptions.map((option) => {
            const optionId = `${option.board}${option.number}`
            return (
              <article className={`brand-option-row brand-option-row-${option.board.toLowerCase()}`} key={optionId}>
                <div className="brand-option-logo">
                  <img
                    src={`/brand-explorations/logos/mount-rs-logo-${option.board.toLowerCase()}-${option.number}.webp`}
                    alt={`${optionId} logo option, ${option.name}`}
                    loading="lazy"
                    decoding="async"
                  />
                  <span className="brand-option-index">{optionId}</span>
                </div>
                <div className="brand-option-copy">
                  <p className="eyebrow">Board {option.board} / {option.number}</p>
                  <h3>{option.name}</h3>
                  <p>{option.description}</p>
                  <span className="brand-option-note">Project direction / {option.name.toLowerCase()}</span>
                </div>
                <div className="brand-option-palette">
                  <p className="brand-option-palette-kicker">Colour system</p>
                  <div className="brand-option-swatch-grid">
                    {option.colors.map((color, index) => (
                      <div className="brand-option-swatch-block" key={`${optionId}-${color}`}>
                        <svg
                          className="brand-option-swatch-chip"
                          viewBox="0 0 100 48"
                          preserveAspectRatio="none"
                          role="img"
                          aria-label={`${paletteRoles[index]} ${color}`}
                        >
                          <rect width="100" height="48" fill={color} />
                        </svg>
                        <div className="brand-option-swatch-meta">
                          <span>{paletteRoles[index]}</span>
                          <code>{color}</code>
                        </div>
                      </div>
                    ))}
                  </div>
                  <p className="brand-option-palette-note">Five tokens to carry into the site theme and product surfaces.</p>
                </div>
              </article>
            )
          })}
        </div>
      </section>

      <div className="brand-lab-next-step">
        <p className="eyebrow">Next step</p>
        <p>
          Choose a board, logo number, and palette. I’ll redraw the selected
          mark as SVG, promote the five colours into design tokens, and apply
          the combination to the header, favicon, and documentation surfaces.
        </p>
        <Link className="text-link" to="/docs">
          Return to the technical docs <span aria-hidden="true">↗</span>
        </Link>
      </div>
    </main>
  )
}
