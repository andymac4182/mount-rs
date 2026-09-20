//! Loopback 9P server for the unmodified upstream conformance client.
use mount_rs_9p::{P9Server, P9ServerOptions};
use mount_rs_core::MemoryFs;
use std::io::{Read, Write};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = Arc::new(P9Server::bind(MemoryFs::empty(), P9ServerOptions::default()).await?);
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
