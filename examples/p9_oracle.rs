//! Loopback 9P server for the unmodified upstream conformance client.
use mount_rs_9p::{P9Server, P9ServerOptions};
use mount_rs_core::{Loopback, MemoryFs};
use std::io::{Read, Write};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let read_only = std::env::args().skip(1).any(|arg| arg == "--read-only");
    let memory = MemoryFs::empty();
    if read_only {
        Loopback::new(memory.clone())
            .write_file("/read-only.txt", b"read-only over 9P")
            .await?;
    }
    let options = P9ServerOptions {
        read_only,
        ..P9ServerOptions::default()
    };
    let server = Arc::new(P9Server::bind(memory, options).await?);
    let task = server.start()?;
    println!("{}", server.local_addr()?.port());
    std::io::stdout().flush()?;
    tokio::task::spawn_blocking(|| {
        let _ = std::io::stdin().read(&mut [0_u8; 1]);
    })
    .await?;
    server.close().await?;
    task.await??;
    Ok(())
}
