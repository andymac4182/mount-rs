use std::collections::HashMap;
use std::io::{self, BufRead};
use std::sync::Arc;

use mount_rs_core::{Capabilities, ErrorCode, FileHandle, FsError, Loopback, OpenFlags};
use mount_rs_host::HostFs;
use serde_json::{Value, json};

fn required_string(command: &Value, name: &str) -> String {
    command[name]
        .as_str()
        .unwrap_or_else(|| panic!("command field {name} must be a string"))
        .to_owned()
}

fn required_u64(command: &Value, name: &str) -> u64 {
    command[name]
        .as_u64()
        .unwrap_or_else(|| panic!("command field {name} must be an unsigned integer"))
}

fn bytes(command: &Value, name: &str) -> Vec<u8> {
    command[name]
        .as_array()
        .unwrap_or_else(|| panic!("command field {name} must be an array"))
        .iter()
        .map(|value| {
            let byte = value
                .as_u64()
                .unwrap_or_else(|| panic!("{name} must contain unsigned bytes"));
            u8::try_from(byte).unwrap_or_else(|_| panic!("{name} contains a non-byte value"))
        })
        .collect()
}

fn position(command: &Value) -> mount_rs_core::Result<Option<u64>> {
    match command.get("position") {
        Some(Value::Null) | None => Ok(None),
        Some(value) if value.as_i64() == Some(-1) => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or_else(|| {
            FsError::new(ErrorCode::Einval)
                .with_syscall("read/write")
                .with_message("invalid handle position")
        }),
    }
}

fn stable_stats(stats: mount_rs_core::Stats) -> Value {
    // `dev`, `ino`, allocation geometry, and all timestamp fields are
    // intentionally excluded: they are host-filesystem observations and are
    // not stable across the two isolated temporary roots or macOS/Linux.
    json!({
        "mode": stats.mode,
        "nlink": stats.nlink,
        "uid": stats.uid,
        "gid": stats.gid,
        "rdev": stats.rdev,
        "size": stats.size,
        "isFile": stats.is_file(),
        "isDirectory": stats.is_directory(),
        "isSymbolicLink": stats.is_symbolic_link(),
        "isBlockDevice": stats.is_block_device(),
        "isCharacterDevice": stats.is_character_device(),
        "isFIFO": stats.is_fifo(),
        "isSocket": stats.is_socket(),
    })
}

fn stable_capabilities(capabilities: Capabilities) -> Value {
    json!({
        "handles": capabilities.handles,
        "hardlinks": capabilities.hardlinks,
        "symlinks": capabilities.symlinks,
        "permissions": capabilities.permissions,
        "times": capabilities.times,
        "truncate": capabilities.truncate,
        "atomicRename": capabilities.atomic_rename,
        "caseSensitive": capabilities.case_sensitive,
        "statfs": capabilities.statfs,
        "readOnly": capabilities.read_only,
        "durableWrites": capabilities.durable_writes,
        "mknod": capabilities.mknod,
    })
}

fn file_type_name(file_type: mount_rs_core::FileType) -> &'static str {
    match file_type {
        mount_rs_core::FileType::File => "File",
        mount_rs_core::FileType::Directory => "Directory",
        mount_rs_core::FileType::Symlink => "Symlink",
        mount_rs_core::FileType::BlockDevice => "BlockDevice",
        mount_rs_core::FileType::CharacterDevice => "CharacterDevice",
        mount_rs_core::FileType::Fifo => "FIFO",
        mount_rs_core::FileType::Socket => "Socket",
    }
}

fn error_value(error: FsError) -> Value {
    json!({
        "code": error.code.as_str(),
        "syscall": error.syscall,
        "path": error.path,
        "dest": error.dest,
    })
}

fn open_mode(command: &Value) -> u32 {
    command.get("mode").and_then(Value::as_u64).unwrap_or(0o666) as u32
}

fn open_id(command: &Value) -> String {
    required_string(command, "id")
}

fn handle<'a>(
    handles: &'a HashMap<String, Arc<dyn FileHandle>>,
    command: &Value,
) -> &'a Arc<dyn FileHandle> {
    let id = open_id(command);
    handles
        .get(&id)
        .unwrap_or_else(|| panic!("unknown handle id {id}"))
}

#[cfg(target_os = "linux")]
const NODE_O_CREAT: u64 = 0o100;
#[cfg(target_os = "linux")]
const NODE_O_EXCL: u64 = 0o200;
#[cfg(target_os = "linux")]
const NODE_O_TRUNC: u64 = 0o1000;
#[cfg(target_os = "linux")]
const NODE_O_APPEND: u64 = 0o2000;

#[cfg(target_os = "macos")]
const NODE_O_CREAT: u64 = 0x0200;
#[cfg(target_os = "macos")]
const NODE_O_EXCL: u64 = 0x0800;
#[cfg(target_os = "macos")]
const NODE_O_TRUNC: u64 = 0x0400;
#[cfg(target_os = "macos")]
const NODE_O_APPEND: u64 = 0x0008;

/// Decode the platform's Node numeric flag namespace before calling the
/// portable core `open_flags` contract. Linux's and Darwin's `O_*` values are
/// intentionally different, so this is not delegated to `OpenFlags::from_bits`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn decode_node_flags(bits: u64, path: &str) -> mount_rs_core::Result<OpenFlags> {
    let access = bits & 0b11;
    let (read, write) = match access {
        0 => (true, false),
        1 => (false, true),
        2 => (true, true),
        _ => {
            return Err(FsError::new(ErrorCode::Einval)
                .with_syscall("open")
                .with_path(path));
        }
    };
    Ok(OpenFlags {
        read,
        write,
        create: bits & NODE_O_CREAT != 0,
        truncate: bits & NODE_O_TRUNC != 0,
        append: bits & NODE_O_APPEND != 0,
        exclusive: bits & NODE_O_EXCL != 0,
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn decode_node_flags(_bits: u64, _path: &str) -> mount_rs_core::Result<OpenFlags> {
    Err(FsError::new(ErrorCode::Enotsup).with_syscall("open"))
}

async fn execute(
    fs: &Loopback,
    handles: &mut HashMap<String, Arc<dyn FileHandle>>,
    command: &Value,
) -> mount_rs_core::Result<Value> {
    let op = command["op"]
        .as_str()
        .unwrap_or_else(|| panic!("command op must be a string"));
    match op {
        "capabilities" => Ok(stable_capabilities(fs.capabilities)),
        "mkdir" => Ok(json!(
            fs.mkdir(
                &required_string(command, "path"),
                mount_rs_core::MkdirOptions {
                    recursive: command
                        .get("recursive")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    mode: command
                        .get("mode")
                        .and_then(Value::as_u64)
                        .map(|mode| mode as u32),
                },
            )
            .await?
        )),
        "rmdir" => {
            fs.rmdir(&required_string(command, "path")).await?;
            Ok(Value::Null)
        }
        "unlink" => {
            fs.unlink(&required_string(command, "path")).await?;
            Ok(Value::Null)
        }
        "rename" => {
            fs.rename(
                &required_string(command, "path"),
                &required_string(command, "dest"),
            )
            .await?;
            Ok(Value::Null)
        }
        "link" => {
            fs.link(
                &required_string(command, "path"),
                &required_string(command, "dest"),
            )
            .await?;
            Ok(Value::Null)
        }
        "symlink" => {
            fs.symlink(
                &required_string(command, "target"),
                &required_string(command, "path"),
            )
            .await?;
            Ok(Value::Null)
        }
        "readlink" => Ok(json!(fs.readlink(&required_string(command, "path")).await?)),
        "chmod" => {
            fs.chmod(
                &required_string(command, "path"),
                required_u64(command, "mode") as u32,
            )
            .await?;
            Ok(Value::Null)
        }
        "truncate" => {
            fs.truncate(
                &required_string(command, "path"),
                required_u64(command, "length"),
            )
            .await?;
            Ok(Value::Null)
        }
        "stat" => Ok(stable_stats(
            fs.stat(&required_string(command, "path")).await?,
        )),
        "lstat" => Ok(stable_stats(
            fs.lstat(&required_string(command, "path")).await?,
        )),
        "readdir" => {
            let mut entries = fs.readdir(&required_string(command, "path")).await?;
            entries.sort_by(|left, right| left.name.cmp(&right.name));
            Ok(json!(
                entries
                    .into_iter()
                    .map(|entry| json!({
                        "name": entry.name,
                        "type": file_type_name(entry.file_type),
                    }))
                    .collect::<Vec<_>>()
            ))
        }
        "open" => {
            let path = required_string(command, "path");
            let flags = required_string(command, "flags");
            let file = fs.open(&path, &flags, open_mode(command)).await?;
            handles.insert(open_id(command), file);
            Ok(json!({ "opened": true }))
        }
        "open_numeric" => {
            let path = required_string(command, "path");
            let flags = decode_node_flags(required_u64(command, "flags"), &path)?;
            let file = fs.open_flags(&path, flags, open_mode(command)).await?;
            handles.insert(open_id(command), file);
            Ok(json!({ "opened": true }))
        }
        "handle_write" => {
            let input = bytes(command, "buffer");
            let offset = required_u64(command, "offset") as usize;
            let length = required_u64(command, "length") as usize;
            let end = offset.saturating_add(length);
            if end > input.len() {
                return Err(FsError::new(ErrorCode::Einval).with_syscall("write"));
            }
            let count = handle(handles, command)
                .write(&input[offset..end], position(command)?)
                .await?;
            Ok(json!({ "bytesWritten": count, "buffer": input }))
        }
        "handle_read" => {
            let mut buffer = bytes(command, "buffer");
            let offset = required_u64(command, "offset") as usize;
            let length = required_u64(command, "length") as usize;
            let end = offset.saturating_add(length);
            if end > buffer.len() {
                return Err(FsError::new(ErrorCode::Einval).with_syscall("read"));
            }
            let count = handle(handles, command)
                .read(&mut buffer[offset..end], position(command)?)
                .await?;
            Ok(json!({ "bytesRead": count, "buffer": buffer }))
        }
        "handle_stat" => Ok(stable_stats(handle(handles, command).stat().await?)),
        "handle_truncate" => {
            handle(handles, command)
                .truncate(required_u64(command, "length"))
                .await?;
            Ok(Value::Null)
        }
        "handle_sync" => {
            handle(handles, command).sync().await?;
            Ok(Value::Null)
        }
        "handle_datasync" => {
            handle(handles, command).datasync().await?;
            Ok(Value::Null)
        }
        "handle_close" => {
            handle(handles, command).close().await?;
            Ok(Value::Null)
        }
        _ => panic!("unknown host parity operation {op}"),
    }
}

#[tokio::main]
async fn main() {
    let root = std::env::args()
        .nth(1)
        .unwrap_or_else(|| panic!("usage: host_oracle <existing-root>"));
    let fs = Loopback::new(HostFs::new(root));
    let mut handles = HashMap::<String, Arc<dyn FileHandle>>::new();

    for line in io::stdin().lock().lines() {
        let line = line.expect("read parity command");
        if line.trim().is_empty() {
            continue;
        }
        let command: Value = serde_json::from_str(&line).expect("valid parity command JSON");
        let result = match execute(&fs, &mut handles, &command).await {
            Ok(value) => json!({ "ok": value }),
            Err(error) => json!({ "error": error_value(error) }),
        };
        println!("{result}");
    }
}
