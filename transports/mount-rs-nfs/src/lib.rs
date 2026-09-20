//! NFS transport for [`mount-rs-core`].
//!
//! The wire implementation follows RFC 1813 (NFSv3 and MOUNTv3) and RFC 5531
//! (ONC RPC v2). The byte-oriented [`Nfs3Session`] is usable without a socket,
//! which is the basis of the rootless integration tests. [`NfsServer`] adds a
//! TCP record-marking listener; native kernel mounting is intentionally a
//! separate, platform-specific concern and is not claimed by this crate.

pub mod constants;
pub mod handles;
pub mod native;
pub mod protocol;
pub mod rpc;
pub mod server;
pub mod session;
pub mod xdr;

pub use constants::*;
pub use handles::{DirectorySnapshots, FH_SIZE, FileHandleTable, HandleEntry};
pub use native::{
    MountEntry, NativeNfsMount, NfsClientProbe, NfsMountError, NfsMountOptions, NfsPlatform,
    NfsVersion, consent_advice, live_nfs_mounts, mount_entry_at, mount_nfs, nfs_client_probe,
    nfs_mount_options, nfs_platform, nfs_platform_for, parse_mount_table, unmount_all_nfs,
    version_refusal,
};
pub use protocol::*;
pub use rpc::{
    AuthSysParams, OpaqueAuth, RecordAssembler, RpcCall, RpcCredentials, RpcReply, auth_null,
    auth_sys, decode_call, decode_reply, encode_auth_sys, encode_call, frame_record,
};
pub use server::{NfsServer, NfsServerOptions, create_nfs_server};
pub use session::{Nfs3Session, NfsRequestContext, NfsSessionOptions, NfsSessionStats};
pub use xdr::{XdrError, XdrReader, XdrWriter};
