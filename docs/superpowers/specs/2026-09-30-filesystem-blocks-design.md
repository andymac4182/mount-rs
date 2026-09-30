# Durable filesystem blocks and TiDB comparison

The user approved testing TiDB metadata with direct filesystem blobs after reviewing the RustFS comparison. This is another compiled block provider; local mounting and the metadata/protocol paths keep their current interfaces.

## Design

`FilesystemBlockStore::open(root, persistent)` serves SHA-256 immutable blocks from an existing private root. Unix descriptor-relative operations reject symlink traversal and bind all operations to the opened root's physical identity. Sharded object paths contain only canonical digest characters. Private staging writes complete before atomic non-overwriting publication. Acknowledgment follows complete atomic publication and authority checks. The OS flushes normally; constructor, PUT and flush issue no forced file/directory/device sync. The provider reports durable=false and exposes the caller's persistent assertion for healthy runtime retirement. OS crashes or power loss can lose blobs after TiDB metadata commits. Existing objects must match their identity and bytes. A stable persisted backing marker supports independent contexts sharing the same authority, and missing/replaced roots or markers fail closed.

Tokio blocking workers own operations to completion. Cancelling an async caller cannot cause a partially written blob to be acknowledged or metadata to be published by this provider. Ambiguous unpublished blobs are retained. Reconciliation is unsupported initially and fails with ENOTSUP; shutdown does not imply reclamation.

Expose `StoreConfig::Filesystem { root, persistent }` on the block side and a corresponding CLI storage selection. Reject its use for metadata. Keep existing RustFS configuration and behavior. The generic local object-store adapter is unsuitable without the persistence and path-isolation contract; HostFs would replace TiDB metadata and is therefore not the desired comparison.

## Test and benchmark scope

Real local provider controls cover independent-context races, immutable duplicate publication, corruption, persistence failures, reopen, missing/replaced authority, invalid identities and symlink isolation. SDK/CLI and harness controls cover block-only selection and owned-root admission.

The native comparison retains ten native servers, ten QUIC clients, ten Drives, five Partitions, 1,000 mixed-size files per Drive, two traffic modes, eight patterns and complete initial/final payload/size/EOF/membership oracles. TiDB remains the same owned backend. Each Drive has an owner-created private filesystem root visible to the ten local processes. Allocation/resource/IO profiling and existing deadlines, evidence caps and 64 GiB host-free floor stay enabled. Original backend containers/volumes are preserved. New data is retained after settled process cleanup.

This compares host filesystem blobs with the capped RustFS/Docker topology; it does not isolate all topology effects, establish power-loss durability, qualify cross-host shared filesystem semantics or satisfy the 10,000-client production target. Independent local disks across machines are not a replacement for shared authoritative storage; production needs ownership/routing and replication/recovery or a qualified shared filesystem.
