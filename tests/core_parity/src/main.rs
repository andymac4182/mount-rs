use std::collections::HashMap;
use std::io::BufRead;
use std::sync::Arc;

use mount_rs_core::{FileHandle, Loopback, MemoryFs, MkdirOptions, Stats, StatsFs};
use serde_json::{Value, json};

fn stable_stats(stats: &Stats, exact_times: bool) -> Value {
    json!({
        "dev": stats.dev,
        "ino": stats.ino,
        "mode": stats.mode,
        "nlink": stats.nlink,
        "uid": stats.uid,
        "gid": stats.gid,
        "rdev": stats.rdev,
        "size": stats.size,
        "blksize": stats.blksize,
        "blocks": stats.blocks,
        "atime_ms": exact_times.then_some(stats.atime_ms),
        "mtime_ms": exact_times.then_some(stats.mtime_ms),
        "atime_present": true,
        "mtime_present": true,
        "ctime_present": true,
        "birthtime_present": true,
        "kind": format!("{:?}", stats.file_type()),
    })
}

fn stable_statfs(stats: &StatsFs) -> Value {
    json!({
        "filesystem_type": stats.filesystem_type,
        "block_size": stats.block_size,
        "blocks": stats.blocks,
        "blocks_free": stats.blocks_free,
        "blocks_available": stats.blocks_available,
        "files": stats.files,
        "files_free": stats.files_free,
    })
}

fn position(command: &Value) -> Option<u64> {
    command.get("position").and_then(Value::as_u64)
}

fn bytes(command: &Value) -> Vec<u8> {
    command["data"]
        .as_array()
        .expect("trace data must be an array")
        .iter()
        .map(|value| value.as_u64().expect("trace byte must be an integer") as u8)
        .collect()
}

fn path(command: &Value) -> &str {
    command["path"]
        .as_str()
        .expect("trace path must be a string")
}

async fn execute(
    fs: &Loopback,
    handles: &mut HashMap<String, Arc<dyn FileHandle>>,
    command: &Value,
) -> mount_rs_core::Result<Value> {
    let op = command["op"].as_str().expect("trace op must be a string");
    match op {
        "mkdir_recursive" => Ok(json!(
            fs.mkdir(
                path(command),
                MkdirOptions {
                    recursive: true,
                    mode: command["mode"].as_u64().map(|mode| mode as u32),
                },
            )
            .await?
        )),
        "mkdir" => {
            fs.mkdir(
                path(command),
                MkdirOptions {
                    recursive: false,
                    mode: command["mode"].as_u64().map(|mode| mode as u32),
                },
            )
            .await?;
            Ok(Value::Null)
        }
        "write" => {
            fs.write_file(path(command), &bytes(command)).await?;
            Ok(Value::Null)
        }
        "read" => Ok(json!(fs.read_file(path(command)).await?)),
        "stat" | "lstat" => {
            let stats = if op == "stat" {
                fs.stat(path(command)).await?
            } else {
                fs.lstat(path(command)).await?
            };
            Ok(stable_stats(
                &stats,
                command["exact_times"].as_bool() != Some(false),
            ))
        }
        "statfs" => Ok(stable_statfs(&fs.statfs(path(command)).await?)),
        "list" => Ok(json!(
            fs.readdir(path(command))
                .await?
                .into_iter()
                .map(|entry| {
                    json!({
                        "name": entry.name,
                        "parent_path": entry.parent_path,
                        "type": format!("{:?}", entry.file_type),
                    })
                })
                .collect::<Vec<_>>()
        )),
        "utimes" => {
            fs.utimes(
                path(command),
                command["atime_ms"]
                    .as_i64()
                    .expect("atime_ms must be an integer"),
                command["mtime_ms"]
                    .as_i64()
                    .expect("mtime_ms must be an integer"),
            )
            .await?;
            Ok(Value::Null)
        }
        "lutimes" => {
            fs.lutimes(
                path(command),
                command["atime_ms"]
                    .as_i64()
                    .expect("atime_ms must be an integer"),
                command["mtime_ms"]
                    .as_i64()
                    .expect("mtime_ms must be an integer"),
            )
            .await?;
            Ok(Value::Null)
        }
        "chmod" => {
            fs.chmod(
                path(command),
                command["mode"].as_u64().expect("mode must be an integer") as u32,
            )
            .await?;
            Ok(Value::Null)
        }
        "chown" => {
            fs.chown(
                path(command),
                command["uid"].as_u64().expect("uid must be an integer") as u32,
                command["gid"].as_u64().expect("gid must be an integer") as u32,
            )
            .await?;
            Ok(Value::Null)
        }
        "link" => {
            fs.link(
                command["existing_path"]
                    .as_str()
                    .expect("existing_path must be a string"),
                command["new_path"]
                    .as_str()
                    .expect("new_path must be a string"),
            )
            .await?;
            Ok(Value::Null)
        }
        "symlink" => {
            fs.symlink(
                command["target"].as_str().expect("target must be a string"),
                path(command),
            )
            .await?;
            Ok(Value::Null)
        }
        "readlink" => Ok(json!(fs.readlink(path(command)).await?)),
        "rename" => {
            fs.rename(
                command["old_path"]
                    .as_str()
                    .expect("old_path must be a string"),
                command["new_path"]
                    .as_str()
                    .expect("new_path must be a string"),
            )
            .await?;
            Ok(Value::Null)
        }
        "unlink" => {
            fs.unlink(path(command)).await?;
            Ok(Value::Null)
        }
        "rmdir" => {
            fs.rmdir(path(command)).await?;
            Ok(Value::Null)
        }
        "truncate" => {
            fs.truncate(
                path(command),
                command["length"]
                    .as_u64()
                    .expect("length must be an integer"),
            )
            .await?;
            Ok(Value::Null)
        }
        "mknod" => {
            fs.mknod(
                path(command),
                command["mode"].as_u64().expect("mode must be an integer") as u32,
                command["dev"].as_u64().expect("dev must be an integer"),
            )
            .await?;
            Ok(Value::Null)
        }
        "open" => {
            let id = command["id"].as_str().expect("handle id must be a string");
            let handle = fs
                .open(
                    path(command),
                    command["flags"].as_str().expect("flags must be a string"),
                    command["mode"].as_u64().expect("mode must be an integer") as u32,
                )
                .await?;
            let fd = handle.fd();
            handles.insert(id.to_owned(), handle);
            Ok(json!({ "fd": fd }))
        }
        "handle_read" => {
            let handle = handles
                .get(command["id"].as_str().expect("handle id must be a string"))
                .expect("trace handle must have been opened");
            let mut buffer =
                vec![0_u8; command["count"].as_u64().expect("count must be an integer") as usize];
            let count = handle.read(&mut buffer, position(command)).await?;
            buffer.truncate(count);
            Ok(json!(buffer))
        }
        "handle_write" => {
            let handle = handles
                .get(command["id"].as_str().expect("handle id must be a string"))
                .expect("trace handle must have been opened");
            Ok(json!(
                handle.write(&bytes(command), position(command)).await?
            ))
        }
        "handle_stat" => {
            let handle = handles
                .get(command["id"].as_str().expect("handle id must be a string"))
                .expect("trace handle must have been opened");
            Ok(stable_stats(
                &handle.stat().await?,
                command["exact_times"].as_bool() != Some(false),
            ))
        }
        "handle_truncate" => {
            let handle = handles
                .get(command["id"].as_str().expect("handle id must be a string"))
                .expect("trace handle must have been opened");
            handle
                .truncate(
                    command["length"]
                        .as_u64()
                        .expect("length must be an integer"),
                )
                .await?;
            Ok(Value::Null)
        }
        "handle_sync" | "handle_datasync" => {
            let handle = handles
                .get(command["id"].as_str().expect("handle id must be a string"))
                .expect("trace handle must have been opened");
            if op == "handle_sync" {
                handle.sync().await?;
            } else {
                handle.datasync().await?;
            }
            Ok(Value::Null)
        }
        "handle_close" => {
            let handle = handles
                .get(command["id"].as_str().expect("handle id must be a string"))
                .expect("trace handle must have been opened");
            handle.close().await?;
            Ok(Value::Null)
        }
        _ => panic!("unknown core parity operation: {op}"),
    }
}

#[tokio::main]
async fn main() {
    let fs = Loopback::new(MemoryFs::new(mount_rs_core::MemoryOptions {
        uid: 501,
        gid: 20,
        umask: 0,
        root_mode: 0o755,
    }));
    let mut handles = HashMap::new();
    for line in std::io::stdin().lock().lines() {
        let command: Value =
            serde_json::from_str(&line.expect("read trace line")).expect("trace line must be JSON");
        let result = match execute(&fs, &mut handles, &command).await {
            Ok(value) => json!({ "ok": value }),
            Err(error) => json!({ "error": error.code.as_str() }),
        };
        println!("{result}");
    }
}
