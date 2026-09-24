//! Core filesystem contract for the Rust port of mountx.
//!
//! Providers and filesystem implementations live in separate workspace crates.
//! This crate defines the path/error/types, driver, and storage contracts.

pub mod chunking;
pub mod delegation;
#[doc(hidden)]
pub mod diagnostics;
pub mod driver;
pub mod error;
pub mod handle;
pub mod path;
pub mod snapshot;
pub mod storage;
pub mod types;
pub mod versioning;

pub use driver::{
    FileHandle, FsDriver, GuardedDirectoryEntry, GuardedMutation, GuardedMutationResult,
    GuardedRead, GuardedReadResult, GuardedSetattr, Loopback, ObservedEntry, PathGuard,
    PathIdentity, collect_bounded_dir_entries,
};
pub use error::{ErrorCode, FsError, Result, backend_error};
pub use handle::OpenFlags;
pub use snapshot::{LoadedSnapshot, StateStore, snapshot_conflict};
pub use types::{
    Capabilities, DirEntry, FileType, MkdirOptions, S_IFBLK, S_IFCHR, S_IFDIR, S_IFIFO, S_IFLNK,
    S_IFMT, S_IFREG, S_IFSOCK, Stats, StatsFs,
};
