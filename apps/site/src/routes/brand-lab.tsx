import { Link, createFileRoute } from '@tanstack/react-router'

export const Route = createFileRoute('/brand-lab')({
  head: () => ({
    meta: [
      { title: 'Brand lab — mount-rs' },
      {
        name: 'description',
        content:
          'Generated brand and logo directions for the next mount-rs site pass.',
      },
    ],
  }),
  component: BrandLabPage,
})

function BrandLabPage() {
  return (
    <main className="page-frame brand-lab">
      <div className="doc-kicker-row">
        <p className="eyebrow">Design lab / next site pass</p>
        <span className="brand-lab-status">Review before implementation</span>
      </div>
      <h1>Choose the visual system for the next mount-rs chapter.</h1>
      <p className="brand-lab-lede">
        The repository now describes a broader storage composition layer: one
        filesystem contract, explicit metadata and block planes, provider
        boundaries, and transport-specific evidence. These explorations test
        how that direction could look before we redraw the selected mark and
        apply a new site theme.
      </p>

      <div className="callout callout-blue brand-lab-callout">
        <strong>Generated explorations, not final artwork.</strong>
        <p>
          The ten brand systems and twenty marks below are comparison boards.
          Once a direction is chosen, the mark will be redrawn as a small
          responsive SVG and the palette will be expressed through the site’s
          existing design tokens.
        </p>
      </div>

      <section className="brand-lab-section" aria-labelledby="brand-directions-heading">
        <div className="brand-lab-heading">
          <div>
            <p className="eyebrow">01 / Brand and colour directions</p>
            <h2 id="brand-directions-heading">Ten systems for the same boundary.</h2>
          </div>
          <p>Each tile tests a different balance of storage-system precision and developer-tool character.</p>
        </div>
        <figure className="brand-lab-figure">
          <img
            src="/brand-explorations/mount-rs-brand-directions-v1.webp"
            alt="Ten numbered mount-rs brand and colour directions arranged in a five by two grid."
          />
          <figcaption>Brand directions 01–10. Palette and surface studies only.</figcaption>
        </figure>
      </section>

      <section className="brand-lab-section" aria-labelledby="logo-options-heading">
        <div className="brand-lab-heading">
          <div>
            <p className="eyebrow">02 / Logo marks</p>
            <h2 id="logo-options-heading">Twenty marks for the mount boundary.</h2>
          </div>
          <p>Look for a silhouette that still reads at favicon size and can survive one-colour reproduction.</p>
        </div>
        <figure className="brand-lab-figure">
          <img
            src="/brand-explorations/mount-rs-logo-options-v2.webp"
            alt="Twenty numbered mount-rs logo mark options arranged in a five by four grid."
          />
          <figcaption>Logo explorations 01–20. Marks should be redrawn before shipping.</figcaption>
        </figure>
      </section>

      <div className="brand-lab-next-step">
        <p className="eyebrow">Next step</p>
        <p>
          Choose a brand direction and a mark, then I’ll turn the combination
          into the site theme, favicon, header mark, and documentation surfaces.
        </p>
        <Link className="text-link" to="/docs">
          Return to the technical docs <span aria-hidden="true">↗</span>
        </Link>
      </div>
    </main>
  )
}
