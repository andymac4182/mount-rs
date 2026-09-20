//! Minimal Rust SDK consumer example.

use mount_rs_sdk::{Filesystem, Loopback, MemoryOptions};

#[tokio::main]
async fn main() -> mount_rs_sdk::Result<()> {
    let filesystem = Filesystem::memory(MemoryOptions::default());
    let view = Loopback::from_arc(filesystem.driver());
    view.write_file("/quickstart.txt", b"hello from mount-rs-sdk")
        .await?;
    let bytes = view.read_file("/quickstart.txt").await?;
    println!("{}", String::from_utf8_lossy(&bytes));
    filesystem.shutdown().await
}
