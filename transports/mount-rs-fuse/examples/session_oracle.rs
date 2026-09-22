use mount_rs_core::FsDriver;
use mount_rs_fuse::session::FuseSession;
use mount_rs_memfs::MemoryFs;
use std::{
    io::{self, BufRead},
    sync::Arc,
};

#[tokio::main]
async fn main() {
    let fs = Arc::new(MemoryFs::empty());
    let file = fs.open("/file", "w", 0o644).await.unwrap();
    file.write(b"abcdef", None).await.unwrap();
    file.close().await.unwrap();
    let mut session = FuseSession::new(fs);
    for line in io::stdin().lock().lines() {
        let line = line.unwrap();
        let message: Vec<u8> = line
            .as_bytes()
            .chunks_exact(2)
            .map(|v| u8::from_str_radix(std::str::from_utf8(v).unwrap(), 16).unwrap())
            .collect();
        match session.handle(&message).await.unwrap() {
            Some(reply) => println!(
                "{}",
                reply.iter().map(|v| format!("{v:02x}")).collect::<String>()
            ),
            None => println!("none"),
        }
    }
}
