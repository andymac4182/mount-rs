//! TiDB-backed metadata and immutable block stores.
//!
//! This crate uses TiDB's MySQL-compatible protocol through `mysql_async`.
//! Metadata and blocks are deliberately separate providers, so callers can
//! compose either one with a provider from another crate.
//!
//! Metadata selects TiDB's pessimistic transaction mode before each explicit
//! transaction and uses a `SELECT ... FOR UPDATE` on the volume row. Lease
//! expiry and fencing are evaluated using TiDB's clock inside the transaction.
//! Revision publication is a compare-and-swap guarded by the same row lock.
//!
//! The provider never retries a transaction after `COMMIT` returns an error:
//! the commit outcome can be ambiguous after a connection failure. Statement
//! conflicts are returned as `EAGAIN` where the operation's state is known;
//! callers may retry the complete operation. Cancellation drops the leased
//! connection, allowing `mysql_async` to roll back a dirty transaction before
//! the connection is reused.
//!
//! `durable` is an explicit caller assertion. A successful TiDB commit is an
//! acknowledged database commit, but host/cluster durability still depends on
//! the TiDB/TiKV deployment and its replication/sync-log configuration. The
//! provider therefore never infers durability from a URL.

mod storage;

pub use storage::{
    DEFAULT_MAX_BLOCK_BYTES, DEFAULT_MAX_NAMESPACE_BYTES, TidbBlockStore, TidbMetadataStore,
    TidbStorageOptions,
};
