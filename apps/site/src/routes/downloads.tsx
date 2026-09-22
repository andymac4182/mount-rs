import { Link, createFileRoute } from '@tanstack/react-router'
import { cliRelease } from '../content/cli-release'

export const Route = createFileRoute('/downloads')({
  head: () => ({
    meta: [
      { title: 'Downloads — mount-rs' },
      {
        name: 'description',
        content:
          'Verified mount-rs CLI preview artifacts published through GitHub Releases.',
      },
    ],
  }),
  component: DownloadsPage,
})

function externalProps() {
  return { target: '_blank' as const, rel: 'noreferrer' }
}

function DownloadsPage() {
  return (
    <section className="page-frame downloads-shell">
      <article className="doc-article downloads-page">
        <p className="eyebrow">Downloads / GitHub Releases</p>
        <h1>Get the mount-rs CLI.</h1>
        <p className="doc-lede">
          CLI binaries are published by GitHub Actions as GitHub Release
          assets. This page records the currently verified preview and its
          exact checksum; Vercel does not mirror executable bytes.
        </p>

        <div className="callout callout-amber">
          <strong>Preview status</strong>
          <p>
            The project is still prerelease and not release-ready. The current
            download covers one Apple Silicon target only and does not claim a
            complete platform matrix or native-mount qualification.
          </p>
        </div>

        <div className="callout callout-blue">
          <strong>Build-matrix qualification is ahead of publication.</strong>
          <p>
            Hosted W08 target run{' '}
            <a href="https://github.com/andymac4182/mount-rs/actions/runs/35641555767" {...externalProps()}>
              35641555767
            </a>{' '}
            from source <code>2bbd0266</code> qualified the Linux x86_64 and
            macOS arm64 build, download, checksum, CycloneDX SBOM, and
            Sigstore-attestation checks. The Linux archive measured 8,373,376
            bytes with SHA-256
            <code>6a3de0a607ffafcedf6bd3385c3849a30208df0345120061061821109a09ac79</code>;
            the macOS arm64 archive measured 6,958,388 bytes with SHA-256
            <code>75a031c4e439ede07f0fa1a09db050b15c45f6d802a703d90b67cde52b4a941d</code>.
            It did not execute the approved tag-triggered publication flow, so
            this page still lists only the release assets that are actually
            present below.
          </p>
          <p>
            A later immutable W08 candidate check in{' '}
            <a href="https://github.com/andymac4182/mount-rs/actions/runs/35714146811" {...externalProps()}>
              run 35714146811
            </a>{' '}
            at source <code>7efded54</code> passed the Linux and macOS release
            builds plus downloaded-asset verification for package version
            <code>0.1.0</code>. The checks reported 288 CycloneDX components
            and CI artifact digests{' '}
            <code>b93450472c0ed1d2bd9c5e06998e60d65071e4ac1af3050ebd07d8b650130db4</code>{' '}
            (Linux) and{' '}
            <code>a6fab3b9374ebcc5e150d71c6493f2005dec60cb7ee90892045dee68c6dda67b</code>{' '}
            (macOS). The Linux and macOS attestation jobs also completed
            successfully, but the approved tag-triggered publication flow
            still has not produced additional public release assets, so this
            page continues to link only the verified GitHub release archive and
            <code>SHA256SUMS</code> below.
          </p>
          <p>
            The later immutable production candidate at source
            <code>d1a81ad4</code> dispatched W08 release-target run{' '}
            <a href="https://github.com/andymac4182/mount-rs/actions/runs/35721892593" {...externalProps()}>
              35721892593
            </a>{' '}
            with attestations enabled, but that run was cancelled before it
            produced release assets when the candidate was superseded. No new
            public binary is claimable from it; the verified preview below
            remains the only downloadable release asset.
          </p>
          <p>
            The latest W08 release-target run{' '}
            <a href="https://github.com/andymac4182/mount-rs/actions/runs/35742010242" {...externalProps()}>
              35742010242
            </a>{' '}
            at source <code>b10db219</code> completed successfully for
            <code>x86_64-unknown-linux-gnu</code> and
            <code>aarch64-apple-darwin</code>. Its verified CI artifacts are
            <code>mount-rs-0.1.0-x86_64-unknown-linux-gnu.tar.gz</code>{' '}
            (8,550,825 bytes; SHA-256{' '}
            <code>497a5bfc8f9494ddb7e62cce3cc7fd908a340284650be745ce2f66f7cfdf4fbc</code>)
            and <code>mount-rs-0.1.0-aarch64-apple-darwin.tar.gz</code>{' '}
            (7,119,469 bytes; SHA-256{' '}
            <code>fd670dce71ada1e0671962a1eb25bef54704a514de75c0b8657f2fee6e9a98fe</code>).
            Each CI bundle passed its <code>SHA256SUMS</code> check; the
            manifests themselves are <code>87fb4a4b6a457724fe649af3ef56373ae610ba10a774cb082cd436dea96dc31d</code>{' '}
            (Linux) and <code>cef948cfcf9b824b4d665b2306780d6b4a41ffddb6771a99d9c94ba374e2ae1a</code>{' '}
            (macOS). Attestation jobs were skipped and the manifests record
            signature/SBOM as pending and canary as not run, so these remain
            CI evidence rather than public GitHub Release downloads. The
            public release below is still the only claimable download.
          </p>
        </div>

        <section className="download-release-card" aria-labelledby="download-release-heading">
          <div>
            <p className="eyebrow">Current verified preview</p>
            <h2 id="download-release-heading">
              mount-rs {cliRelease.version}
            </h2>
            <p>
              Release tag <code>{cliRelease.tag}</code>. The archive contains
              the repository's <code>mount-rs-cli</code> binary built in
              release mode for the target shown below.
            </p>
          </div>
          <div className="download-release-body">
            <div className="download-facts">
              <div className="download-fact">
                <span>Target</span>
                <code>{cliRelease.target}</code>
              </div>
              <div className="download-fact">
                <span>Artifact</span>
                <code>{cliRelease.artifact}</code>
              </div>
              <div className="download-fact">
                <span>SHA-256</span>
                <code>{cliRelease.sha256}</code>
              </div>
            </div>
            <div className="download-actions">
              <a className="button button-warm" href={cliRelease.assetUrl} {...externalProps()}>
                Download macOS arm64 CLI
              </a>
              <a href={cliRelease.checksumsUrl} {...externalProps()}>
                SHA256SUMS <span aria-hidden="true">↗</span>
              </a>
              <a href={cliRelease.releaseUrl} {...externalProps()}>
                View GitHub release <span aria-hidden="true">↗</span>
              </a>
            </div>
          </div>
        </section>

        <h2>Published artifact</h2>
        <div className="download-table-wrap">
          <table className="api-table download-table">
            <thead>
              <tr>
                <th>Version</th>
                <th>Target</th>
                <th>Archive</th>
                <th>Checksum</th>
              </tr>
            </thead>
            <tbody>
              <tr>
                <td><code>{cliRelease.tag}</code></td>
                <td><code>{cliRelease.target}</code></td>
                <td>
                  <a href={cliRelease.assetUrl} {...externalProps()}>
                    <code>{cliRelease.artifact}</code>
                  </a>
                </td>
                <td><code>{cliRelease.sha256}</code></td>
              </tr>
            </tbody>
          </table>
        </div>

        <h2>How release publication works</h2>
        <ol className="download-steps">
          <li>
            A <code>v&lt;crate-version&gt;-cli-&lt;channel&gt;</code> tag must point to a
            commit reachable from <code>main</code>.
          </li>
          <li>
            GitHub Actions runs the CLI tests, builds
            <code> mount-rs-cli</code> with <code>--locked --release</code>, and
            verifies the binary version and target. The current workflow also
            writes a release manifest and CycloneDX SBOM, then generates and
            verifies Sigstore attestations for the archive.
          </li>
          <li>
            The published GitHub release is downloaded again and checked for
            matching checksums, manifest/SBOM contents, attestations, archive
            shape, and <code>mount-rs --version</code> before the workflow
            completes. The current <code>{cliRelease.tag}</code> release
            predates those extra verification assets, so this page links only
            the archive and <code>SHA256SUMS</code> that are actually present.
          </li>
        </ol>

        <div className="source-note">
          <span className="source-note-mark" aria-hidden="true">↗</span>
          <p>
            Read the <Link to="/docs/cli">CLI guide</Link> for executable
            workflows, then browse <a href={cliRelease.releasesUrl} {...externalProps()}>all GitHub Releases</a> for future preview artifacts. For the implementation and current acceptance boundaries, return to the <Link to="/docs">technical docs</Link>.
          </p>
        </div>
      </article>
    </section>
  )
}
