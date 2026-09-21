//! A rootless 9P2000.L server for the [`mount-rs-core`] driver contract.
//!
//! The transport is split into protocol layers so wire behavior can be tested
//! without a kernel mount:
//!
//! * [`wire`] contains bounded little-endian primitives and the frame
//!   reassembler.
//! * [`protocol`] contains symmetric 9P2000.L message codecs.
//! * [`session`] maps a connection's fids to an [`FsDriver`].
//! * [`server`] exposes sessions over portable Tokio TCP sockets.
//!
//! The TCP server is usable on macOS and Linux. Native 9P mounts are an
//! operating-system feature rather than a property of the wire tests: Linux
//! has the `v9fs` client and normally needs the appropriate kernel modules and
//! mount privileges; macOS does not provide a native 9P client, so the
//! rootless protocol tests are the supported verification there.

pub mod constants;
pub mod fids;
pub mod locks;
pub mod mount;
pub mod protocol;
pub mod server;
pub mod session;
pub mod wire;

pub use constants::*;
pub use fids::{
    DirCursor, Fid, FidCursorView, FidOpenState, FidOpenView, FidTable, FidView, qid_type,
    qid_version, walk_step,
};
pub use locks::{
    P9Lock, P9LockClient, P9LockHolder, P9LockRequest, P9LockTable, P9LockTableOptions,
};
pub use mount::{
    MountEntry, P9ClientProbe, P9Mount, P9MountOptions, P9MountTarget, P9MountTransport,
    P9Platform, mount_9p, p9_client_probe, p9_mount_options, p9_platform, parse_mount_table,
    socket_path_refusal, tcp_source_refusal,
};
pub use protocol::*;
pub use server::{
    P9AttachOptions, P9Connection, P9Server, P9ServerHooks, P9ServerOptions, P9TransportError,
    P9TransportErrorHook, P9TransportErrorKind,
};
pub use session::{
    P9AssertionHook, P9Session, P9SessionErrorHook, P9SessionHooks, P9SessionOptions,
    P9SessionStats, P9User,
};
pub use wire::{P9Error, P9Qid, P9Reader, P9Writer};
