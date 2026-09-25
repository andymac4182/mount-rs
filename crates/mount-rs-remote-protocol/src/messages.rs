use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    Read,
    Write,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireError {
    pub code: String,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Message {
    ClientHello {
        version: u16,
        partition_id: String,
        bearer: String,
    },
    ServerHello {
        version: u16,
        session_id: String,
    },
    Renew {
        bearer: String,
    },
    Request {
        request_id: u64,
        drive_id: String,
        operation: Operation,
    },
    Response {
        request_id: u64,
        result: Result<Value, WireError>,
    },
}

impl std::fmt::Debug for Message {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ClientHello {
                version,
                partition_id,
                ..
            } => output
                .debug_struct("ClientHello")
                .field("version", version)
                .field("partition_id", partition_id)
                .field("bearer", &"<redacted>")
                .finish(),
            Self::ServerHello {
                version,
                session_id,
            } => output
                .debug_struct("ServerHello")
                .field("version", version)
                .field("session_id", session_id)
                .finish(),
            Self::Renew { .. } => output
                .debug_struct("Renew")
                .field("bearer", &"<redacted>")
                .finish(),
            Self::Request {
                request_id,
                drive_id,
                operation,
            } => output
                .debug_struct("Request")
                .field("request_id", request_id)
                .field("drive_id", drive_id)
                .field("operation", &operation.name)
                .finish(),
            Self::Response { request_id, .. } => output
                .debug_struct("Response")
                .field("request_id", request_id)
                .field("result", &"<redacted>")
                .finish(),
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    pub name: OperationName,
    pub body: Value,
}

impl Operation {
    #[must_use]
    pub fn required_permission(&self) -> Permission {
        match self.name {
            OperationName::Open
                if self
                    .body
                    .get("flags")
                    .and_then(Value::as_str)
                    .is_some_and(|flags| matches!(flags, "r" | "rs")) =>
            {
                Permission::Read
            }
            OperationName::Open if self.body.get("flags").is_some_and(read_only_flags) => {
                Permission::Read
            }
            OperationName::GuardedMutation
                if self
                    .body
                    .get("Open")
                    .and_then(|open| open.get("flags"))
                    .is_some_and(read_only_flags) =>
            {
                Permission::Read
            }
            _ => self.name.required_permission(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationName {
    Capabilities,
    Stat,
    Lstat,
    Statfs,
    ReaddirBounded,
    GuardedRead,
    Readlink,
    Open,
    HandleRead,
    HandleStat,
    HandleWrite,
    HandleTruncate,
    HandleSync,
    HandleDatasync,
    HandleClose,
    Write,
    Mkdir,
    Rmdir,
    Unlink,
    Rename,
    Link,
    Symlink,
    Chmod,
    Chown,
    Lchown,
    Truncate,
    Utimens,
    Utimes,
    Lutimes,
    Mknod,
    GuardedMutation,
    Syncfs,
}

impl OperationName {
    #[must_use]
    pub const fn required_permission(self) -> Permission {
        match self {
            Self::Capabilities
            | Self::Stat
            | Self::Lstat
            | Self::Statfs
            | Self::ReaddirBounded
            | Self::GuardedRead
            | Self::Readlink
            | Self::HandleRead
            | Self::HandleStat
            | Self::HandleClose => Permission::Read,
            Self::Open
            | Self::HandleWrite
            | Self::HandleTruncate
            | Self::HandleSync
            | Self::HandleDatasync
            | Self::Write
            | Self::Mkdir
            | Self::Rmdir
            | Self::Unlink
            | Self::Rename
            | Self::Link
            | Self::Symlink
            | Self::Chmod
            | Self::Chown
            | Self::Lchown
            | Self::Truncate
            | Self::Utimens
            | Self::Utimes
            | Self::Lutimes
            | Self::Mknod
            | Self::GuardedMutation
            | Self::Syncfs => Permission::Write,
        }
    }
}

fn read_only_flags(flags: &Value) -> bool {
    read_only_flag_values(
        ["read", "write", "create", "truncate", "append", "exclusive"]
            .map(|name| flags.get(name).and_then(Value::as_bool)),
    )
}

impl std::fmt::Debug for Operation {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        output
            .debug_struct("Operation")
            .field("name", &self.name)
            .field("body", &"<redacted>")
            .finish()
    }
}

fn read_only_flag_values(flags: [Option<bool>; 6]) -> bool {
    flags[0] == Some(true) && flags[1..].iter().all(|flag| *flag == Some(false))
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    #[kani::unwind(7)]
    fn remote_readonly_flags_require_explicit_nonmutating_values() {
        // Every flag independently is absent/invalid, false, or true (3^6 cases).
        let flags: [Option<bool>; 6] = kani::any();
        let accepted = read_only_flag_values(flags);
        assert_eq!(
            accepted,
            flags
                == [
                    Some(true),
                    Some(false),
                    Some(false),
                    Some(false),
                    Some(false),
                    Some(false)
                ]
        );
        kani::cover!(accepted);
        kani::cover!(!accepted && flags[0].is_none());
        kani::cover!(!accepted && flags[1] == Some(true));
        kani::cover!(!accepted && flags[3] == Some(true));
        kani::cover!(!accepted && flags[5].is_none());
    }
}
