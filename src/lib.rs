//! Core filesystem contract for the Rust port of mountx.
//!
//! Backend integrations live in separate workspace crates. This crate contains
//! only the path/error/types contract, loopback harness, and reference memfs.

pub mod chunking;
pub mod driver;
pub mod error;
pub mod handle;
pub mod memory;
pub mod path;
pub mod storage;
pub mod types;
pub mod versioning;

pub use driver::{FileHandle, FsDriver, Loopback};
pub use error::{ErrorCode, FsError, Result, backend_error};
pub use handle::OpenFlags;
pub use memory::{MemoryFs, MemoryOptions};
pub use types::{
    Capabilities, DirEntry, FileType, MkdirOptions, S_IFBLK, S_IFCHR, S_IFDIR, S_IFIFO, S_IFLNK,
    S_IFMT, S_IFREG, S_IFSOCK, Stats, StatsFs,
};
