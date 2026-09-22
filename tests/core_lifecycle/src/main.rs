use std::collections::HashMap;
use std::io::{self, BufRead};
use std::sync::Arc;

use mount_rs_core::{FileHandle, Loopback, MkdirOptions, Stats};
use mount_rs_memfs::{MemoryFs, MemoryOptions};
use serde_json::{Value, json};

fn path(command: &Value) -> &str {
    command["path"]
        .as_str()
        .expect("lifecycle path must be a string")
}

fn bytes(command: &Value) -> Vec<u8> {
    command["data"]
        .as_array()
        .expect("lifecycle data must be an array")
        .iter()
        .map(|value| value.as_u64().expect("lifecycle byte must be an integer") as u8)
        .collect()
}

fn position(command: &Value) -> Option<u64> {
    command["position"].as_u64()
}

fn stable_stats(stats: &Stats) -> Value {
    json!({
        "mode": stats.mode,
        "nlink": stats.nlink,
        "uid": stats.uid,
        "gid": stats.gid,
        "rdev": stats.rdev,
        "size": stats.size,
        "blksize": stats.blksize,
        "blocks": stats.blocks,
        "kind": format!("{:?}", stats.file_type()),
    })
}

async fn execute(
    fs: &Loopback,
    handles: &mut HashMap<String, Arc<dyn FileHandle>>,
    command: &Value,
) -> mount_rs_core::Result<Value> {
    let op = command["op"]
        .as_str()
        .expect("lifecycle operation must be a string");
    match op {
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
        "write_file" => {
            fs.write_file(path(command), &bytes(command)).await?;
            Ok(Value::Null)
        }
        "read_file" => Ok(json!(fs.read_file(path(command)).await?)),
        "unlink" => {
            fs.unlink(path(command)).await?;
            Ok(Value::Null)
        }
        "open" => {
            let id = command["id"]
                .as_str()
                .expect("lifecycle handle id must be a string");
            let handle = fs
                .open(
                    path(command),
                    command["flags"]
                        .as_str()
                        .expect("lifecycle flags must be a string"),
                    command["mode"].as_u64().unwrap_or(0) as u32,
                )
                .await?;
            let fd = handle.fd();
            handles.insert(id.to_owned(), handle);
            Ok(json!({ "fd": fd }))
        }
        "handle_read" => {
            let id = command["id"]
                .as_str()
                .expect("lifecycle handle id must be a string");
            let handle = handles.get(id).expect("lifecycle handle must be open");
            let mut buffer = vec![
                0_u8;
                command["count"]
                    .as_u64()
                    .expect("lifecycle count must be an integer")
                    as usize
            ];
            let count = handle.read(&mut buffer, position(command)).await?;
            buffer.truncate(count);
            Ok(json!(buffer))
        }
        "handle_write" => {
            let id = command["id"]
                .as_str()
                .expect("lifecycle handle id must be a string");
            let handle = handles.get(id).expect("lifecycle handle must be open");
            Ok(json!(
                handle.write(&bytes(command), position(command)).await?
            ))
        }
        "handle_stat" => {
            let id = command["id"]
                .as_str()
                .expect("lifecycle handle id must be a string");
            let handle = handles.get(id).expect("lifecycle handle must be open");
            Ok(stable_stats(&handle.stat().await?))
        }
        "handle_truncate" => {
            let id = command["id"]
                .as_str()
                .expect("lifecycle handle id must be a string");
            let handle = handles.get(id).expect("lifecycle handle must be open");
            handle
                .truncate(
                    command["length"]
                        .as_u64()
                        .expect("lifecycle length must be an integer"),
                )
                .await?;
            Ok(Value::Null)
        }
        "handle_sync" | "handle_datasync" => {
            let id = command["id"]
                .as_str()
                .expect("lifecycle handle id must be a string");
            let handle = handles.get(id).expect("lifecycle handle must be open");
            if op == "handle_sync" {
                handle.sync().await?;
            } else {
                handle.datasync().await?;
            }
            Ok(Value::Null)
        }
        "handle_close" => {
            let id = command["id"]
                .as_str()
                .expect("lifecycle handle id must be a string");
            let handle = handles.get(id).expect("lifecycle handle must be open");
            handle.close().await?;
            Ok(Value::Null)
        }
        _ => panic!("unknown core lifecycle operation: {op}"),
    }
}

#[tokio::main]
async fn main() {
    let fs = Loopback::new(MemoryFs::new(MemoryOptions {
        uid: 501,
        gid: 20,
        umask: 0,
        root_mode: 0o755,
    }));
    let mut handles = HashMap::new();
    for line in io::stdin().lock().lines() {
        let command: Value = serde_json::from_str(&line.expect("read lifecycle trace line"))
            .expect("lifecycle trace line must be JSON");
        let result = match execute(&fs, &mut handles, &command).await {
            Ok(value) => json!({ "ok": value }),
            Err(error) => json!({ "error": error.code.as_str() }),
        };
        println!("{result}");
    }
}
