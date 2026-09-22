use std::collections::HashMap;
use std::io::{self, Read};
use std::sync::{Arc, Mutex};

use mount_rs_core::{FileHandle, Loopback, MkdirOptions, Stats};
use mount_rs_memfs::{MemoryFs, MemoryOptions};
use serde::Deserialize;
use serde_json::{Map, Value, json};

type Handles = Arc<Mutex<HashMap<String, Arc<dyn FileHandle>>>>;

#[derive(Debug, Deserialize)]
struct Packet {
    packet: String,
    oracle_revision: String,
    scenarios: Vec<Scenario>,
    unsupported: Vec<Value>,
}

#[derive(Debug, Deserialize)]
struct Scenario {
    id: String,
    #[allow(dead_code)]
    purpose: String,
    setup: Vec<Value>,
    rounds: Vec<Round>,
    observe: Vec<Value>,
}

#[derive(Debug, Deserialize)]
struct Round {
    mode: String,
    ops: Vec<Value>,
}

fn text<'a>(command: &'a Value, key: &str) -> &'a str {
    command[key]
        .as_str()
        .unwrap_or_else(|| panic!("command field {key} must be a string: {command}"))
}

fn number(command: &Value, key: &str) -> u64 {
    command[key]
        .as_u64()
        .unwrap_or_else(|| panic!("command field {key} must be an unsigned integer: {command}"))
}

fn bytes(command: &Value) -> Vec<u8> {
    command["data"]
        .as_array()
        .unwrap_or_else(|| panic!("command data must be an array: {command}"))
        .iter()
        .map(|value| {
            value
                .as_u64()
                .and_then(|value| u8::try_from(value).ok())
                .unwrap_or_else(|| panic!("command data must contain bytes: {command}"))
        })
        .collect()
}

fn position(command: &Value) -> Option<u64> {
    command.get("position").and_then(Value::as_u64)
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

fn result_value(result: mount_rs_core::Result<Value>) -> Value {
    match result {
        Ok(value) => json!({ "ok": value }),
        Err(error) => json!({ "error": error.code.as_str() }),
    }
}

fn with_result(command: &Value, result: Value) -> Value {
    let mut entry = command
        .as_object()
        .cloned()
        .unwrap_or_else(|| panic!("command must be an object: {command}"));
    entry.insert("result".to_owned(), result);
    Value::Object(entry)
}

fn handle(handles: &Handles, id: &str) -> Arc<dyn FileHandle> {
    handles
        .lock()
        .expect("handle table lock poisoned")
        .get(id)
        .cloned()
        .unwrap_or_else(|| panic!("unknown handle {id}"))
}

async fn execute(
    fs: &Loopback,
    handles: &Handles,
    command: &Value,
) -> mount_rs_core::Result<Value> {
    match text(command, "op") {
        "mkdir" | "mkdir_recursive" => Ok(json!(
            fs.mkdir(
                text(command, "path"),
                MkdirOptions {
                    recursive: text(command, "op") == "mkdir_recursive",
                    mode: command
                        .get("mode")
                        .and_then(Value::as_u64)
                        .map(|mode| mode as u32),
                },
            )
            .await?
        )),
        "write_file" => {
            fs.write_file(text(command, "path"), &bytes(command))
                .await?;
            Ok(Value::Null)
        }
        "read" => Ok(json!(fs.read_file(text(command, "path")).await?)),
        "read_chunks" => {
            let mut chunks: Vec<Vec<u8>> = fs
                .read_file(text(command, "path"))
                .await?
                .chunks(number(command, "chunk_size") as usize)
                .map(ToOwned::to_owned)
                .collect();
            if command.get("sort").and_then(Value::as_str) == Some("bytes") {
                chunks.sort();
            }
            Ok(json!(chunks))
        }
        "list" => {
            let mut entries: Vec<Value> = fs
                .readdir(text(command, "path"))
                .await?
                .into_iter()
                .map(|entry| {
                    json!({
                        "name": entry.name,
                        "type": format!("{:?}", entry.file_type),
                    })
                })
                .collect();
            if command.get("sort").and_then(Value::as_str) == Some("name") {
                entries.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
            }
            Ok(json!(entries))
        }
        "stat" | "lstat" => {
            let stats = if text(command, "op") == "stat" {
                fs.stat(text(command, "path")).await?
            } else {
                fs.lstat(text(command, "path")).await?
            };
            Ok(stable_stats(&stats))
        }
        "rename" => {
            fs.rename(text(command, "old_path"), text(command, "new_path"))
                .await?;
            Ok(Value::Null)
        }
        "unlink" => {
            fs.unlink(text(command, "path")).await?;
            Ok(Value::Null)
        }
        "rmdir" => {
            fs.rmdir(text(command, "path")).await?;
            Ok(Value::Null)
        }
        "truncate" => {
            fs.truncate(text(command, "path"), number(command, "length"))
                .await?;
            Ok(Value::Null)
        }
        "chmod" => {
            fs.chmod(text(command, "path"), number(command, "mode") as u32)
                .await?;
            Ok(Value::Null)
        }
        "chown" => {
            fs.chown(
                text(command, "path"),
                number(command, "uid") as u32,
                number(command, "gid") as u32,
            )
            .await?;
            Ok(Value::Null)
        }
        "symlink" => {
            fs.symlink(text(command, "target"), text(command, "path"))
                .await?;
            Ok(Value::Null)
        }
        "readlink" => Ok(json!(fs.readlink(text(command, "path")).await?)),
        "utimes" => {
            fs.utimes(
                text(command, "path"),
                command["atime_ms"]
                    .as_i64()
                    .expect("atime_ms must be signed"),
                command["mtime_ms"]
                    .as_i64()
                    .expect("mtime_ms must be signed"),
            )
            .await?;
            Ok(Value::Null)
        }
        "lutimes" => {
            fs.lutimes(
                text(command, "path"),
                command["atime_ms"]
                    .as_i64()
                    .expect("atime_ms must be signed"),
                command["mtime_ms"]
                    .as_i64()
                    .expect("mtime_ms must be signed"),
            )
            .await?;
            Ok(Value::Null)
        }
        "mknod" => {
            fs.mknod(
                text(command, "path"),
                number(command, "mode") as u32,
                number(command, "dev"),
            )
            .await?;
            Ok(Value::Null)
        }
        "open" => {
            let id = text(command, "id").to_owned();
            let opened = fs
                .open(
                    text(command, "path"),
                    text(command, "flags"),
                    number(command, "mode") as u32,
                )
                .await?;
            handles
                .lock()
                .expect("handle table lock poisoned")
                .insert(id, opened);
            Ok(Value::Null)
        }
        "open_discard" => {
            let opened = fs
                .open(
                    text(command, "path"),
                    text(command, "flags"),
                    number(command, "mode") as u32,
                )
                .await?;
            opened.close().await?;
            Ok(Value::Null)
        }
        "handle_read" => {
            let opened = handle(handles, text(command, "id"));
            let mut buffer = vec![0; number(command, "count") as usize];
            let count = opened.read(&mut buffer, position(command)).await?;
            buffer.truncate(count);
            Ok(json!(buffer))
        }
        "handle_write" => {
            let opened = handle(handles, text(command, "id"));
            Ok(json!(
                opened.write(&bytes(command), position(command)).await?
            ))
        }
        "handle_stat" => Ok(stable_stats(
            &handle(handles, text(command, "id")).stat().await?,
        )),
        "handle_truncate" => {
            handle(handles, text(command, "id"))
                .truncate(number(command, "length"))
                .await?;
            Ok(Value::Null)
        }
        "handle_sync" => {
            handle(handles, text(command, "id")).sync().await?;
            Ok(Value::Null)
        }
        "handle_datasync" => {
            handle(handles, text(command, "id")).datasync().await?;
            Ok(Value::Null)
        }
        "handle_close" => {
            handle(handles, text(command, "id")).close().await?;
            Ok(Value::Null)
        }
        operation => panic!("unknown concurrency operation {operation}"),
    }
}

async fn run_command(fs: &Loopback, handles: &Handles, command: &Value) -> Value {
    with_result(command, result_value(execute(fs, handles, command).await))
}

async fn run_round(fs: &Loopback, handles: &Handles, round: &Round) -> Value {
    let mut tasks = Vec::with_capacity(round.ops.len());
    for command in &round.ops {
        let fs = fs.clone();
        let handles = handles.clone();
        let command = command.clone();
        tasks.push(tokio::spawn(async move {
            run_command(&fs, &handles, &command).await
        }));
    }

    let mut operations = Vec::with_capacity(tasks.len());
    for task in tasks {
        operations.push(task.await.expect("concurrency operation task panicked"));
    }
    if round.mode == "multiset" {
        for operation in &mut operations {
            operation
                .as_object_mut()
                .expect("operation report must be an object")
                .remove("actor");
        }
        operations.sort_by_key(|operation| operation["result"].to_string());
    } else if round.mode != "ordered" {
        panic!("unknown round mode {}", round.mode);
    }
    json!({ "mode": round.mode, "operations": operations })
}

async fn run_scenario(scenario: &Scenario) -> Value {
    let fs = Loopback::new(MemoryFs::new(MemoryOptions {
        uid: 501,
        gid: 20,
        umask: 0,
        root_mode: 0o755,
    }));
    let handles = Arc::new(Mutex::new(HashMap::new()));

    let mut setup = Vec::with_capacity(scenario.setup.len());
    for command in &scenario.setup {
        setup.push(run_command(&fs, &handles, command).await);
    }

    let mut rounds = Vec::with_capacity(scenario.rounds.len());
    for round in &scenario.rounds {
        rounds.push(run_round(&fs, &handles, round).await);
    }

    let mut observations = Vec::with_capacity(scenario.observe.len());
    for command in &scenario.observe {
        observations.push(run_command(&fs, &handles, command).await);
    }

    json!({
        "id": scenario.id,
        "setup": setup,
        "rounds": rounds,
        "observations": observations,
    })
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input).expect("read stdin");
    let packet: Packet = if input.trim().is_empty() {
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scenarios.json"
        )))
        .expect("parse bundled scenarios")
    } else {
        serde_json::from_str(&input).expect("parse scenario packet")
    };

    let mut scenarios = Vec::with_capacity(packet.scenarios.len());
    for scenario in &packet.scenarios {
        scenarios.push(run_scenario(scenario).await);
    }

    let mut report = Map::new();
    report.insert("packet".to_owned(), json!(packet.packet));
    report.insert("oracle_revision".to_owned(), json!(packet.oracle_revision));
    report.insert("engine".to_owned(), json!("mount-rs"));
    report.insert("scenarios".to_owned(), json!(scenarios));
    report.insert("unsupported".to_owned(), json!(packet.unsupported));
    println!("{}", Value::Object(report));
}
