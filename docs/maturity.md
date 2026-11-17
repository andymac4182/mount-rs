# Maturity levels

Every provider and transport on the project site carries one of four labels.
This page says what each label requires, so a label can be checked against
evidence instead of argued about.

| Level | Meaning |
|-------|---------|
| **Validated** | The named surface has current, scoped test or live-service evidence, and nothing listed under "Required for Validated" below is open. |
| **Preview** | The integration is useful and has meaningful evidence, but at least one required item below is open. |
| **Experimental** | A real implementation exists with material acceptance gaps. Behaviour may change. |
| **Planned** | Preparation exists, but the integration is not accepted. |

"GA" and "stable" are not separate levels. Validated is the highest level.

## What Validated does and does not claim

Validated is a statement about the software: its contract is written down,
tests prove the contract against the real dependency, and those tests run
on every change. It is not a statement about any one deployment. Identity
and secret management, backup and disaster-recovery drills, capacity
planning, alerting, canary and rollback remain the operator's
responsibility, and each entry says so under its limits.

## Required for Validated

### Every provider and transport

1. **A written contract.** The behaviour callers may rely on, and the
   limits, are documented in the repository.
2. **Tests against the real dependency.** Contract tests run against the
   actual service or kernel interface, not a stand-in, and fail when that
   dependency is missing instead of skipping.
3. **Those tests run in CI** on every pull request, or on every push to
   `main` when a pull-request run is impractical (for example, a budgeted
   cloud account). A test that exists but runs in no workflow is not
   evidence.
4. **Failure behaviour is tested**, not only the success path: restart or
   reconnect, and the fail-closed paths the contract promises.
5. **No known correctness gap rated High** in
   [`production-readiness.md`](production-readiness.md) for that surface.
6. **The site entry cites current evidence** and states its limits. An
   entry must not cite runs from a revision that no longer reflects the
   code.

### Providers, additionally

7. **Composition.** The provider passes the split-store composition tests
   with at least one other provider in the opposite role.
8. **Fencing and uncertain outcomes.** For metadata providers: stale-writer
   rejection, concurrent publication, and an ambiguous commit reported as
   outcome-unknown and never retried.
9. **Differential traces.** The seeded trace-parity harness
   (`scripts/check-trace-parity.mjs`) includes the provider.
10. **Transport security is exercised** where the provider speaks over a
    network: a real TLS handshake, including a rejected one.

### Transports, additionally

7. **A machine-readable contract is checked.** Where the transport
   publishes one (the HTTP API's `openapi.yaml`), a test proves the server
   conforms to it.
8. **A compatibility policy.** What may change within a version, and how
   a breaking change is introduced, is written down.
9. **Operator documentation** covers configuration, health, shutdown,
   upgrade and rollback, and the observability a deployment needs.
10. **The binary is exercised end to end** on every platform the release
    publishes for.

## Changing a label

Raise a label in the same pull request that lands, or links, the last piece
of required evidence. Lower it when required evidence stops running or a
High correctness gap is found.
