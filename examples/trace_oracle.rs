use mount_rs_core::{Loopback, MemoryFs, MkdirOptions, Result};
use serde_json::{Value, json};
use std::io::{self, BufRead};

async fn execute(fs: &Loopback, command: &Value) -> Result<Value> {
    let op = command[0].as_str().unwrap();
    let path = command[1].as_str().unwrap();
    let other = command[2].as_str().unwrap_or("");
    match op {
        "write" => fs.write_file(path, other.as_bytes()).await?,
        "mkdir" => {
            fs.mkdir(
                path,
                MkdirOptions {
                    recursive: false,
                    mode: Some(0o755),
                },
            )
            .await?;
        }
        "mkdir_recursive" => {
            return Ok(json!(
                fs.mkdir(
                    path,
                    MkdirOptions {
                        recursive: true,
                        mode: Some(0o755),
                    }
                )
                .await?
            ));
        }
        "rename" => fs.rename(path, other).await?,
        "link" => fs.link(path, other).await?,
        "symlink" => fs.symlink(path, other).await?,
        "readlink" => return Ok(json!(fs.readlink(path).await?)),
        "chmod" => fs.chmod(path, command[2].as_u64().unwrap() as u32).await?,
        "chown" => {
            fs.chown(path, command[2].as_u64().unwrap() as u32, 5678)
                .await?
        }
        "unlink" => fs.unlink(path).await?,
        "rmdir" => fs.rmdir(path).await?,
        "truncate" => fs.truncate(path, command[2].as_u64().unwrap()).await?,
        "read" => return Ok(json!(fs.read_file(path).await?)),
        "list" => {
            return Ok(json!(
                fs.readdir(path)
                    .await?
                    .into_iter()
                    .map(|entry| entry.name)
                    .collect::<Vec<_>>()
            ));
        }
        "stat" | "lstat" => {
            let stats = if op == "stat" {
                fs.stat(path).await?
            } else {
                fs.lstat(path).await?
            };
            return Ok(json!([
                stats.mode,
                stats.size,
                stats.nlink,
                stats.uid,
                stats.gid
            ]));
        }
        _ => panic!("unknown operation"),
    }
    Ok(Value::Null)
}
#[tokio::main]
async fn main() {
    let fs = match std::env::args().nth(1).as_deref().unwrap_or("memory") {
        "memory" => Loopback::new(MemoryFs::empty()),
        "sqlite" => Loopback::new(mount_rs_sqlite::open_sqlite_memory().await.unwrap()),
        "object-store" => Loopback::new(
            mount_rs_r2::open_object_store(
                std::sync::Arc::new(object_store::memory::InMemory::new()),
                "trace/state",
            )
            .await
            .unwrap(),
        ),
        "pglite" => {
            let url = std::env::var("PGLITE_DATABASE_URL").expect("PGLITE_DATABASE_URL required");
            Loopback::new(
                mount_rs_pglite::connect_pglite_with_key(
                    &url,
                    format!("trace-{}", std::process::id()),
                )
                .await
                .unwrap(),
            )
        }
        name => panic!("unknown backend {name}"),
    };
    for line in io::stdin().lock().lines() {
        let command: Value = serde_json::from_str(&line.unwrap()).unwrap();
        let result = match execute(&fs, &command).await {
            Ok(value) => json!({"ok":value}),
            Err(error) => {
                if error.code == mount_rs_core::ErrorCode::Eio {
                    eprintln!("trace backend error: {error}");
                }
                json!({"error":error.code.as_str()})
            }
        };
        println!("{result}");
    }
}
