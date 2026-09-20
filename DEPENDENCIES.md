# Dependency policy

Keep direct and transitive dependencies small without compromising correctness,
interoperability, or maintainability. Prefer the standard library for small,
well-defined operations. Tokio and other focused, established libraries are
appropriate where they provide necessary runtime or protocol functionality.

- Core must not depend on storage SDKs, network transports, Node bindings, or
  platform mount implementations.
- Each integration and transport owns its dependencies in its separate crate.
- Enable only required dependency features; disable defaults when unnecessary.
- Keep test-only tools in dev-dependencies or the test harness, not runtime code.
- Before adding a dependency, consider its transitive footprint and macOS/Linux
  support. Record the reason in the crate documentation for substantial additions.
- Do not replace cryptography, TLS, database engines, or complex protocol handling
  with fragile bespoke code simply to reduce a dependency count.

`pithings/mountx` is the behavior and compatibility oracle. The additional
`tursodatabase/agentfs` reference can inform storage and platform architecture;
it does not redefine parity. Preserve applicable licensing and attribution when
adapting code from either project.

Review the resolved graph with `cargo tree --workspace` and enabled features with
`cargo tree --workspace -e features` before accepting new integrations.
