//! Loopback-only server for the unmodified upstream NFS clients and suite.
use mount_rs_memfs::MemoryFs;
use mount_rs_nfs::{NfsServer, NfsServerOptions};
use std::io::{Read, Write};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = NfsServer::new(MemoryFs::empty(), NfsServerOptions::default());
    let address = server.listen().await?;
    println!("{}", address.port());
    std::io::stdout().flush()?;
    tokio::task::spawn_blocking(|| {
        let _ = std::io::stdin().read(&mut [0_u8; 1]);
    })
    .await?;
    server.close().await?;
    Ok(())
}
