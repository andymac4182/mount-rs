# Owned RustFS client construction

## Evidence and scope

The unchanged native TiDB/RustFS cold-retirement run failed its existing frame
freshness gate before servers or coordinators started. The standalone probe's
inner HTTP client build occupied 2,780.8465 ms of a 2,782.570 ms prefix poll.
The four later SDK builds occupied 723.558418 ms of a 724.120 ms SDK poll.
These are wall durations. They do not establish exclusive CPU, allocator,
physical I/O, SQL contention, or production capacity.

Move those existing synchronous constructors off Tokio runtime workers. Preserve
all four clients, signing, TLS, retries, role separation, block prefixes and
durability choices. LIST, SQL, qualification and marker writes remain awaited
in their existing owners. This step does not change the storage format.

## Provider contract

`RustFsConstructionContext` owns at most `max_builds` reserved, building,
ready-unclaimed, disposing, or quarantined tickets. A positive bound is required.
Admission waits for capacity without starting a worker. Seal rejects and wakes
waiting admissions. Every admitted ticket is strongly retained before the
observer callback and before spawning. The production recipe is closed:
`BlockStore { config, prefix, durable }` or `OwnedPrefixProbe { config, prefix }`.

Public interfaces:

```rust
RustFsConstructionContext::new(max_builds: usize) -> Result<Self>;
context.seal_admission() -> Result<()>;
context.close().await -> Result<()>;
context.block_store(config, prefix, durable, observer).await
    -> Result<RustFsBlockStore>;
context.owned_prefix_probe(config, prefix, observer).await
    -> Result<OwnedPrefixProbe>;
probe.observe_owned_prefix_absence().await -> Result<bool>;
```

`observer` is `Option<&dyn ConstructionObserver>`. Configurations and prefixes
are owned values. The opaque probe validates its exact prefix before admission
and builds only the existing standalone probe. Its LIST delegates to
`observe_owned_prefix_absence_with` with the unchanged 30 second deadline.

The actual `spawn_blocking` handle stays inside its ticket and is polled by
borrow with a fixed owner waker. Enable notifications before polling. Caller
cancellation cannot take, replace or abort that handle. Construction closures
own only recipes, never their context or ticket. Reserved registration is
unsettled until the callback returns and either a worker is installed or seal
acknowledges a no-spawn result. Call observers outside ownership mutexes.

Claim and seal select exactly one result consumer. Transfer a successful result
once; store a separate terminal receipt for repeated close. Dispose discarded
products outside mutexes, and release the charge only after join and transfer
or actual disposal. Opportunistic admission reaps completed abandoned work;
close joins all retained work even after another ticket fails. Typed constructor
errors preserve their original caller result and can settle cleanup. Panic or
poison is sticky, prevents publication, retains the quarantine charge and makes
close fail. A poisoned registry seals and quarantines the entire context:
admission remains rejected and close remains an error after known healthy
tickets are actually joined and disposed. Those proven tickets can release
their charges; poisoned tickets retain theirs. Recover poisoned contents to
attempt all physical joins.

Explicit close is required. If the final context owner is dropped with an
unsettled or quarantined ticket, retain that ticket for process lifetime rather
than silently detach an unproven handle. Successful explicit drains remove
settled tickets. Constructor drainage is not an HTTP/socket close acknowledgment.

## SDK and CLI integration

`StorageContext` owns one constructor context with a default bound of eight.
Add constructor-only `seal_client_builds()` and `close_client_builds().await`.
Expose `prepare_rustfs_owned_prefix_probe(config, prefix, observer).await` for
retained context-based preparation; it returns the provider's opaque probe and
requires an open SDK context before admission.
Context-aware RustFS opening uses this owner. Non-context SDK opening keeps its
existing synchronous constructor, avoiding an unowned temporary async context.
Direct context close seals construction and first-polls constructor drain and
every TiDB pool close before yielding; no resource family is skipped on error.

On the first CLI close request, synchronously seal constructor admission.
Insert `ClientBuilds` between `Factories` and `Inspectors`. Drain constructors
despite earlier pool/factory failure, then preserve the existing authority
barrier before Inspectors, Context and Cache. An abandoned outer construction
journal remains uncertain; pure constructor cleanup cannot clear it.

Native initialization retains a StorageContext before preparing the prefix
probe, and passes the same context into SDK opening. Preserve namespace-present
short-circuiting, exact LIST semantics, fresh oracle independence and journal
handoff/failure ordering.
Fresh raw-provider oracle construction also uses a separately retained provider
construction context; its later public SDK oracle retains a fresh SDK context.
Both remain independent of initialization and of the CLI server contexts.

## Required proof

Use actually invoked held factories through the production ticket lifecycle.
Verify runtime heartbeat, canceled builds and closes, multiple waiters, seal
during observer registration, claim/seal arbitration, actual product disposal,
typed errors, panic/poison quarantine, draining peers after failure, and invalid
prefix rejection before admission. The baseline first-poll assertion is a
diagnostic RED; it is not a permanent scheduling promise for instant builds.

Qualify SDK and CLI close ordering and existing observer uncertainty. Run
formatting, strict Clippy, existing provider/SDK/CLI suites and unchanged native
TiDB/RustFS geometry. Keep failed receipts and honest incomplete cleanup status.

## Subsequent work remains required

After offload qualification, reuse an exact-configuration, policy-epoch keyed
set of four independently configured clients per server StorageContext. Keep
each drive's prefix/cache separate and keep the standalone probe distinct.
Then resume D100/P50/F1000, D1000/P500 and 10,000 clients/drives across 5,000
partitions and ten servers, both idle and continuous I/O. Cache faults, backing
savings, secure negotiated fallback, native mount E2E, formal checks, terminal
CI, independent review and the authorized PR delivery remain part of the goal.
