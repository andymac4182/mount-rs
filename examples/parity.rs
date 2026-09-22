use std::sync::Arc;

use mount_rs_core::{FsDriver, Loopback, MkdirOptions};
use mount_rs_memfs::MemoryFs;
use serde_json::json;

#[tokio::main]
async fn main() {
    let fs = Loopback::from_arc(Arc::new(MemoryFs::empty()) as Arc<dyn FsDriver>);
    fs.mkdir(
        "/workspace/src",
        MkdirOptions {
            recursive: true,
            mode: None,
        },
    )
    .await
    .unwrap();
    fs.write_file("/workspace/src/lib.rs", b"pub fn answer() -> u8 { 42 }\n")
        .await
        .unwrap();
    fs.write_file("/workspace/README.md", b"mount-rs\n")
        .await
        .unwrap();
    fs.link("/workspace/README.md", "/workspace/README-copy.md")
        .await
        .unwrap();
    fs.rename("/workspace/src", "/workspace/source")
        .await
        .unwrap();
    fs.symlink("source/lib.rs", "/workspace/current")
        .await
        .unwrap();

    let mut root = fs.readdir("/workspace").await.unwrap();
    root.sort_by(|left, right| left.name.cmp(&right.name));
    let mut source = fs.readdir("/workspace/source").await.unwrap();
    source.sort_by(|left, right| left.name.cmp(&right.name));
    let stat = fs.stat("/workspace/README.md").await.unwrap();
    let output = json!({
        "root": root.into_iter().map(|entry| json!({"name": entry.name, "type": format!("{:?}", entry.file_type)})).collect::<Vec<_>>(),
        "source": source.into_iter().map(|entry| json!({"name": entry.name, "type": format!("{:?}", entry.file_type)})).collect::<Vec<_>>(),
        "current": String::from_utf8(fs.read_file("/workspace/current").await.unwrap()).unwrap(),
        "readme": String::from_utf8(fs.read_file("/workspace/README.md").await.unwrap()).unwrap(),
        "readme_nlink": stat.nlink,
        "readme_size": stat.size,
        "link_target": fs.readlink("/workspace/current").await.unwrap(),
    });
    println!("{output}");
}
