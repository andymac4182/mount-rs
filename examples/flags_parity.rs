use mount_rs_core::{Loopback, MemoryFs, OpenFlags};
use serde_json::json;

#[tokio::main]
async fn main() {
    let fs = Loopback::new(MemoryFs::empty());
    fs.write_file("/file", b"abcdef").await.unwrap();
    let flags = OpenFlags {
        read: true,
        write: true,
        create: true,
        truncate: false,
        append: false,
        exclusive: false,
    };
    let h = fs.open_flags("/file", flags, 0o640).await.unwrap();
    h.write(b"XY", Some(1)).await.unwrap();
    h.write(b"", Some(0)).await.unwrap();
    let mut cursor = [0; 2];
    h.read(&mut cursor, None).await.unwrap();
    h.close().await.unwrap();
    let readonly = OpenFlags {
        write: false,
        ..flags
    };
    let h = fs.open_flags("/new", readonly, 0o640).await.unwrap();
    let denied = h.write(b"x", None).await.unwrap_err().code.as_str();
    h.close().await.unwrap();
    fs.chown("/file", 123, 456).await.unwrap();
    fs.chown("/file", u32::MAX, 789).await.unwrap();
    let stat = fs.stat("/file").await.unwrap();
    println!(
        "{}",
        json!({"data": fs.read_file("/file").await.unwrap(), "cursor": cursor, "new_mode": fs.stat("/new").await.unwrap().mode & 0o777, "denied": denied, "owner": [stat.uid, stat.gid]})
    );
}
