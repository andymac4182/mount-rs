# Shared directory ownership

Phase 1 provides exclusive writeback and explicit existing shared CAS; it does not implement directory checkout/checkin. The approved proposal included shared directory authority and safe handoff. Continue that scope while optional clarification remains pending.

## Protocol

Introduce a distinct persisted delegated protocol (MRC3), bound to the existing physical block backing, which rejects both old volume-lease publication and old MRC2 CAS publication. Do not change MRC2 semantics in place. Enrollment/migration is explicit and offline; live clients cannot silently switch.

Directory authority is an identity-bound, nonexpiring grant: root inode, unique session owner, monotonic fence. Claims must reject ancestor/descendant overlap and external hardlinks. Do not add client-clock expiry. Clean checkin releases a grant; crashed owners require explicit expected-fence recovery, never automatic forced takeover. Provider transactions enforce grant identity/fence, backing and revision, and validate that a replacement namespace modifies only the authorized subtree. A client-supplied touched-inode list is not sufficient. Claims and releases require their own durable commit barrier and retry-safe identities.

Namespaces retain global inode allocation and root identity. Namespace headers cannot be changed through a subtree grant. New inode IDs must not reuse prior allocation. Rename/hardlink across ownership boundaries is initially forbidden; claim must reject outside hardlinks. Open-unlinked nodes need persistent ownership provenance while their handles exist, or fail such operations closed without advertising full POSIX support. Provider-side authorization must be reusable and tested from hostile candidate namespaces, not just normal driver calls.

## Engine and access

Shared-delegated clients retain immediate publication; exclusive writeback remains unchanged. Checkout reloads the authoritative namespace and creates a new handle/cache generation. Read/write file opens require authority for the database directory, including read-only SQLite clients. Existing handles carry the grant fence. Checkin blocks new operations, drains in-flight work, rejects open handles within the scope, flushes blocks and metadata, retires local state, then atomically releases provider authority. Errors or ambiguous barriers must not release ownership prematurely.

Initial native handoff requires unmount/remount so a new kernel mount generation cannot reuse old file data or path caches. Do not advertise live NFS/FSKit handoff. SDK direct-driver checkout/checkin may operate with explicit generation retirement. CLI should acquire configured directory grants before exposing a fresh mount and check them in after unmount and successful shutdown. Separate checkin commands cannot pretend to flush another running client's buffers; any live control path must target that mount service.

## Provider support and qualification

Default metadata extension methods return ENOTSUP. Implement SQLite first with immediate transactions and preserved physical-file/backing authority, then PGlite/TiDB/FoundationDB with equivalent mode/token/revision/delta checks. Unsupported provider/transport combinations fail before mutation. Memory may be a correctness oracle but remains volatile and is not independent-host evidence.

Tests must cover old-client rejection, grant overlap, external hardlinks, unauthorized outside edits/header edits/inode reuse, stale handles, clean release/reclaim, crashed owner recovery, ambiguous grants/publications, failed drain retaining authority and generation overflow. Native Linux tests must cover two independent mounted clients with disjoint owned database directories, both DELETE/WAL, denied unauthorized writers, remount handoff and stale owner rejection. Benchmark delegation overhead separately from phase-1 exclusive writeback; preserve existing raw performance evidence.
