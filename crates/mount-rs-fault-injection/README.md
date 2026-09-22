# mount-rs-fault-injection

Opt-in storage-contract fault injection, separate from production dependencies.
Wrap a `MetadataStore` or `BlockStore` with an explicit `FaultInjector` and
validated `FaultPlan`. Rules select a boundary, operation, before/after phase
and occurrence; a finite budget bounds injected events.

Supported actions include errors such as EIO/ENOSPC, lease failure, publication
CAS conflict, lost publication acknowledgment, and optional bounded delays
(`tokio-delay`). After-publication lost acknowledgment means the delegate
succeeded but the caller cannot assume whether it committed; reconcile rather
than blindly retry. These wrappers do not roll back provider side effects.

Traces record pending, completed and cancelled injections without file contents,
paths or credentials. The seed labels a run; it does **not** control concurrent
scheduling. Replay requires the same caller-controlled operation ordering.

This crate is a root workspace member using the shared lockfile.
CLI/Node/VFS/HTTP integration remains pending. The tests cover
storage-wrapper semantics, not kernel faults, network partitions, power loss,
or the complete SQLite reliability matrix.

```sh
cargo test --locked -p mount-rs-fault-injection
cargo test --locked -p mount-rs-fault-injection --all-features
```

CI runs these gates on Linux, macOS and Windows; configured jobs are not proof
that those platforms have passed. Licensed Apache-2.0.
