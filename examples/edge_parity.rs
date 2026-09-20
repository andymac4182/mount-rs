mod support;
use mount_rs_core::{ErrorCode, S_IFIFO};
use serde_json::json;

fn error_code<T>(result: mount_rs_core::Result<T>) -> Option<&'static str> {
    result.err().map(|error| match error.code {
        ErrorCode::Eloop => "ELOOP",
        ErrorCode::Enxio => "ENXIO",
        _ => error.code.as_str(),
    })
}

#[tokio::main]
async fn main() {
    let fs = support::parity_filesystem().await;
    fs.write_file("/flags", b"abcdef").await.unwrap();
    let append = fs.open("/flags", "a", 0).await.unwrap();
    append.write(b"-g", None).await.unwrap();
    append.close().await.unwrap();
    fs.truncate("/flags", 3).await.unwrap();
    fs.truncate("/flags", 5).await.unwrap();
    fs.chmod("/flags", 0o640).await.unwrap();
    fs.chown("/flags", 123, 456).await.unwrap();
    fs.utimes("/flags", 1_700_000_000_000, 1_700_000_001_000)
        .await
        .unwrap();

    fs.symlink("two", "/one").await.unwrap();
    fs.symlink("one", "/two").await.unwrap();
    fs.mknod("/fifo", S_IFIFO | 0o644, 0).await.unwrap();
    let flags = fs.stat("/flags").await.unwrap();
    let fifo = fs.stat("/fifo").await.unwrap();
    let statfs = fs.statfs("/").await.unwrap();
    let fifo_open_error = error_code(fs.open("/fifo", "r", 0).await);
    let output = json!({
        "append": fs.read_file("/flags").await.unwrap(),
        "flags": {
            "mode": flags.mode & 0o7777,
            "uid": flags.uid,
            "gid": flags.gid,
            "atime_ms": flags.atime_ms,
            "mtime_ms": flags.mtime_ms,
        },
        "symlink_loop": error_code(fs.stat("/one").await),
        "fifo": { "mode": fifo.mode, "is_fifo": fifo.is_fifo(), "open_error": fifo_open_error },
        "statfs": {
            "filesystem_type": statfs.filesystem_type,
            "block_size": statfs.block_size,
            "blocks": statfs.blocks,
            "blocks_free": statfs.blocks_free,
            "blocks_available": statfs.blocks_available,
            "files": statfs.files,
            "files_free": statfs.files_free,
        },
    });
    println!("{output}");
}
