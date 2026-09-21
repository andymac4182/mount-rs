import { Link, createFileRoute } from '@tanstack/react-router'
import { CodeBlock } from '../../components/code-block'
import { cliRelease } from '../../content/cli-release'

export const Route = createFileRoute('/docs/cli')({
  head: () => ({
    meta: [
      { title: 'CLI — mount-rs docs' },
      {
        name: 'description',
        content:
          'Run mount-rs from the command line: probe a host, self-test the filesystem contract, serve HTTP, or create a native mount where supported.',
      },
    ],
  }),
  component: CliDocs,
})

function CliDocs() {
  return (
    <article className="doc-article">
      <p className="eyebrow">Documentation / CLI</p>
      <h1>Run mount-rs from the command line.</h1>
      <p className="doc-lede">
        The CLI is a top-level way to use mount-rs when you want a ready-made
        executable workflow instead of embedding the Rust or Node APIs. Probe
        the host, run the contract against a provider, serve an HTTP edge, or
        create a real mount when the platform integration is available.
      </p>

      <div className="callout callout-blue">
        <strong>Start with the CLI when you are evaluating the boundary.</strong>
        <p>
          The same filesystem contract sits behind the CLI, Rust API, and Node
          API. Use the CLI to make the storage, transport, lifecycle, and host
          prerequisites visible before you commit those choices to an
          application.
        </p>
      </div>

      <CodeBlock label="Quick start / inspect the host and run a self-test">
        {`mount-rs probe
mount-rs sdk-self-test
mount-rs --help`}
      </CodeBlock>

      <h2>What the CLI is for</h2>
      <div className="api-table-wrap">
        <table className="api-table">
          <caption className="sr-only">mount-rs CLI entry points</caption>
          <thead><tr><th>Command</th><th>Use it to</th><th>Mounts?</th></tr></thead>
          <tbody>
            <tr><td><code>probe</code></td><td>Explain the host's FUSE, 9P, and NFS prerequisites and the auto preference.</td><td>No</td></tr>
            <tr><td><code>sdk-self-test</code></td><td>Write, read, sync, and optionally reopen through the public filesystem contract.</td><td>No</td></tr>
            <tr><td><code>serve-http</code></td><td>Expose configured drives through the loopback HTTP service.</td><td>No</td></tr>
            <tr><td><code>mount</code></td><td>Attach the configured filesystem through the selected native transport.</td><td>Yes, where supported</td></tr>
          </tbody>
        </table>
      </div>

      <h2>Use a checked-in configuration</h2>
      <p>
        Versioned JSON keeps provider and transport choices explicit. Validate
        the shape first, then run the mount or HTTP service with the same file.
        Relative paths resolve from the configuration file.
      </p>
      <CodeBlock label="Configuration / validate, mount, or serve">
        {`mount-rs validate-config --config config.json
mount-rs mount --config config.json
mount-rs serve-http --config config-http.json`}
      </CodeBlock>

      <div className="callout callout-amber">
        <strong>Transport qualification stays host-specific.</strong>
        <p>
          The CLI's auto path can select FUSE, 9P, or NFS from the host
          prerequisites. <code>probe</code> itself never mounts, and
          <code> serve-http</code> is intentionally unmounted. A passing CLI
          self-test is not blanket proof of native mount, platform, or
          production readiness.
        </p>
      </div>

      <h2>Get the verified preview</h2>
      <p>
        The current published artifact is a prerelease for{' '}
        <code>{cliRelease.target}</code> only. It is a real built CLI archive,
        but it does not claim a complete platform matrix or native-mount
        qualification.
      </p>
      <div className="source-note">
        <span className="source-note-mark" aria-hidden="true">↗</span>
        <p>
          <Link to="/downloads">Download mount-rs {cliRelease.version}</Link>{' '}
          or read the <a href="https://github.com/andymac4182/mount-rs/blob/main/crates/mount-rs-cli/README.md">CLI README</a> and <a href="https://github.com/andymac4182/mount-rs/blob/main/.github/workflows/cli-release.yml">release workflow</a> for the exact build and publication boundary.
        </p>
      </div>
    </article>
  )
}
