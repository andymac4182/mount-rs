# mount-rs AWS S3 provider

This crate owns the signed AWS S3 client configuration and its immutable block
provider. `AwsS3Config::build_store` resolves credentials through the standard
AWS environment and workload identity sources, validates the bucket and region,
and applies a bounded retry policy. It rejects custom endpoints, unsigned
requests, plain HTTP, and invalid certificate overrides.

Use `AwsS3BlockStore::from_config(&config, prefix)` for durable AWS blocks, or
`AwsS3BlockStore::new(store, prefix, durable)` with an existing object-store
client and an explicit durability declaration. The shared object-store block
adapter owns content addressing, conditional creation, diagnostics, and scoped
reconciliation. Namespace metadata is supplied by a separate provider.

`AwsS3BlockStore::from_config_with_durable` keeps an explicit durability
setting. Configured clients run a bounded signed create/read probe under the
same block prefix before concurrent metadata publication. Injected `new`
clients can run in exclusive mode and reject concurrent startup.
