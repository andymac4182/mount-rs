//! NFS transport for [`mount-rs-core`].
//!
//! The wire implementation follows RFC 1813 (NFSv3 and MOUNTv3), RFC 5531
//! (ONC RPC v2), and the implemented NFSv4.1 COMPOUND/session subset from RFC
//! 8881. The byte-oriented sessions are usable without a socket, which is the
//! basis of the rootless integration tests. [`NfsServer`] adds a TCP
//! record-marking listener; native kernel mounting is intentionally a separate,
//! platform-specific concern and is not claimed by wire tests.

pub mod constants;
pub mod handles;
pub mod native;
pub mod protocol;
pub mod rpc;
pub mod server;
pub mod session;
pub mod v4;
pub mod xdr;

pub use constants::*;
pub use handles::{DirectorySnapshots, FH_SIZE, FileHandleTable, HandleEntry};
pub use native::{
    MountEntry, NativeNfsMount, NfsClientProbe, NfsMountError, NfsMountOptions, NfsPlatform,
    NfsVersion, consent_advice, live_nfs_mounts, mount_entry_at, mount_nfs, mount_nfs_with_hooks,
    nfs_client_probe, nfs_mount_options, nfs_platform, nfs_platform_for, parse_mount_table,
    unmount_all_nfs, version_refusal,
};
pub use protocol::*;
pub use rpc::{
    AuthSysParams, OpaqueAuth, RecordAssembler, RpcCall, RpcCredentials, RpcReply, auth_null,
    auth_sys, decode_call, decode_reply, encode_auth_sys, encode_call, frame_record,
};
pub use server::{
    NfsConnection, NfsServer, NfsServerHooks, NfsServerOptions, NfsTransportError,
    NfsTransportErrorHook, NfsTransportErrorKind, create_nfs_server, create_nfs_server_with_hooks,
};
pub use session::{
    Nfs3Session, Nfs4Clock, Nfs4IdMap, Nfs4StateOptions, NfsRequestContext, NfsSessionOptions,
    NfsSessionStats,
};
pub use v4::{NFS_V4, NFS4_PROGRAM, Nfs4Session};
pub use xdr::{XdrError, XdrReader, XdrWriter};
