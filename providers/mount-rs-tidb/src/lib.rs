//! TiDB-backed metadata and immutable block stores.
//!
//! This crate uses TiDB's MySQL-compatible protocol through `mysql_async`.
//! Metadata and blocks are deliberately separate providers, so callers can
//! compose either one with a provider from another crate.
//!
//! Metadata selects TiDB's pessimistic transaction mode before each explicit
//! transaction and uses a `SELECT ... FOR UPDATE` on the volume row. Lease
//! expiry and fencing are evaluated using TiDB's clock inside the transaction.
//! Revision publication uses an atomic conditional statement guarded by the
//! lease or MRC2 backing identity and the expected revision.
//! Each newly opened pool session explicitly enables and verifies autocommit,
//! independent of the server's global default.
//!
//! MRC2 enrollment binds a never-used metadata volume to a persistent block
//! authority and exhausts the legacy lease fence in the same statement. New
//! enrollment and MRC1 migration have read-only preflight hooks; the final
//! publication always rechecks their conditions. Concurrent reclamation is
//! unsupported because distributed handle pins and a capacity policy are
//! required before immutable blocks can be deleted safely.
//!
//! The provider never retries a transaction after `COMMIT` returns an error:
//! the commit outcome can be ambiguous after a connection failure. Statement
//! conflicts are returned as `EAGAIN` where the operation's state is known;
//! callers may retry the complete operation. Cancellation drops the leased
//! connection, allowing `mysql_async` to roll back a dirty transaction before
//! the connection is reused.
//!
//! Every round trip is bounded by a statement timeout. A timeout is never
//! retried, and one that ends a `COMMIT` or an autocommit publication is
//! reported with an unknown outcome, because TiDB may still commit a
//! statement it has received.
//!
//! `durable` is an explicit caller assertion. A successful TiDB commit is an
//! acknowledged database commit, but host/cluster durability still depends on
//! the TiDB/TiKV deployment and its replication/sync-log configuration. The
//! provider therefore never infers durability from a URL.

mod storage;

pub use storage::{
    DEFAULT_MAX_BLOCK_BYTES, DEFAULT_MAX_NAMESPACE_BYTES, DEFAULT_STATEMENT_TIMEOUT,
    STATEMENT_TIMEOUT_URL_PARAMETER, TLS_CA_FILE_URL_PARAMETER, TidbBlockStore, TidbMetadataStore,
    TidbStorageOptions, TidbStorePool,
};
