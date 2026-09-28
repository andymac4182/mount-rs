# Construction ownership controls

This phase prepares safe activation of the bounded Drive runtime pool. It does
not enable lazy CLI startup or qualify the full production workload.

## Source changes

- The CLI can prepare an owned construction plan, then reopen its resolved
  paths, explicit environment references, owner and identity without resolving
  them again. Each opened runtime caches one driver Arc; getting that driver
  clones the Arc instead of constructing another wrapper.
- The core construction observer accepts the actual resource owner before a
  later await. PGlite and private TiDB constructors offer explicit observer
  variants. Existing entry points retain their existing behavior.
- Observed Chunked construction retains acquisition intent, acknowledged writer
  authority and the actual filesystem before initial publication or checkout.
  It uses the current filesystem's authority after transfer. Unknown acquisition,
  ambiguous checkout and sticky filesystem failure reject cleanup.
- The SDK exposes a per-attempt journal. Its cleanup is owned independently of
  waiters, runs in reverse registration order and stops before dependent
  providers when authority cleanup fails. Failure retains the registered owners.
  A successful handoff releases only journal references after the caller has
  retained the actual returned owner.
- Startup v2 can represent lazy construction plans separately from opened
  runtimes. The eager v1 shape remains available. The CLI and process fixture
  still use eager startup until the lifecycle wiring is completed.

The journal snapshot describes one attempt. Registration counts include aliases;
they are not native handle counts, physical IOPS or a process drain proof. The
journal and observer variants are explicit APIs; ordinary SDK filesystem opening
does not automatically retain partial resources in this journal.

## Required controls

The selected controls cover waiter cancellation, shared cleanup, failed and
panicking cleanup, unknown construction, successful handoff, late ownership
registration, and completion callbacks. Additional controls use actual SQLite
writer and checkout authority, actual PostgreSQL/MySQL clients against owned
loopback protocol peers, and full SQLite payload/EOF/backing checks after reopen.

Protocol peers do not qualify live PGlite or TiDB deployments. Modeled journal
controls do not prove native resource cleanup or filesystem integration. The
[retained report](live-service-metrics-20260928/report.json) records the executed
scope and the source-frozen gate results.

## Remaining seams

SDK provider composition and CLI postconfiguration must register these actual
owners before their later awaits. The CLI factory must retain each journal
before polling, preserve shared context/cache ownership and wire runtime banks
into diagnostics. Lazy signed CLI and real-process gates remain required.

AWS S3 still rereads configuration and credential-source environment variables
during construction. Freezing the credential source must preserve token-content
refresh. Locked SlateDB starts background tasks before its open call returns;
retaining the returned database does not qualify that internal construction
interval. FoundationDB network-guard integration remains separate.

Native mounts, live backend comparisons, crash/power-loss behavior, new symbolic
proofs and the unchanged ten-server full target remain open qualification gates.
