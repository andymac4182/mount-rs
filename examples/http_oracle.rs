// Run the Rust HTTP fixtures used by `scripts/check-http-parity.mjs`.
//
// The example intentionally does not mount anything or contact a remote
// service. It starts one authenticated, loopback-only S3 gateway and one
// authenticated WebDAV server over independent in-memory drivers, prints
// their ephemeral URLs, and waits for EOF on stdin so the Node harness can
// shut them down cleanly on macOS and Linux.

use std::io::{self, Read, Write};
use std::sync::Arc;

use async_trait::async_trait;
use mount_rs_core::{Capabilities, DirEntry, FileHandle, FsDriver, MemoryFs, MkdirOptions, Stats};
use mount_rs_s3::{Credentials, S3ServerOptions, create_s3_server};
use mount_rs_webdav::{WebdavServerOptions, create_webdav_server};

const ACCESS_KEY: &str = "AKIAMOUNTX7GATEWAY9";
const SECRET_KEY: &str = "test-secret-key";
const REGION: &str = "us-east-1";
const WEBDAV_USER: &str = "ada";
const WEBDAV_PASSWORD: &str = "a pass:word";
const FIXED_MTIME_MS: i64 = 1_600_000_000_000;

#[derive(Clone)]
struct FixedMtimeDriver {
    inner: MemoryFs,
}

impl FixedMtimeDriver {
    fn new(inner: MemoryFs) -> Self {
        Self { inner }
    }

    fn fix_stats(&self, mut stats: Stats) -> Stats {
        stats.atime_ms = FIXED_MTIME_MS;
        stats.mtime_ms = FIXED_MTIME_MS;
        stats.ctime_ms = FIXED_MTIME_MS;
        stats.birthtime_ms = FIXED_MTIME_MS;
        stats
    }
}

#[async_trait]
impl FsDriver for FixedMtimeDriver {
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }

    async fn stat(&self, path: &str) -> mount_rs_core::Result<Stats> {
        Ok(self.fix_stats(self.inner.stat(path).await?))
    }

    async fn lstat(&self, path: &str) -> mount_rs_core::Result<Stats> {
        Ok(self.fix_stats(self.inner.lstat(path).await?))
    }

    async fn readdir(&self, path: &str) -> mount_rs_core::Result<Vec<DirEntry>> {
        self.inner.readdir(path).await
    }

    async fn open(
        &self,
        path: &str,
        flags: &str,
        mode: u32,
    ) -> mount_rs_core::Result<Arc<dyn FileHandle>> {
        self.inner.open(path, flags, mode).await
    }

    async fn mkdir(
        &self,
        path: &str,
        options: MkdirOptions,
    ) -> mount_rs_core::Result<Option<String>> {
        self.inner.mkdir(path, options).await
    }

    async fn rmdir(&self, path: &str) -> mount_rs_core::Result<()> {
        self.inner.rmdir(path).await
    }

    async fn unlink(&self, path: &str) -> mount_rs_core::Result<()> {
        self.inner.unlink(path).await
    }

    async fn rename(&self, old_path: &str, new_path: &str) -> mount_rs_core::Result<()> {
        self.inner.rename(old_path, new_path).await
    }

    async fn utimes(&self, path: &str, atime_ms: i64, mtime_ms: i64) -> mount_rs_core::Result<()> {
        self.inner.utimes(path, atime_ms, mtime_ms).await
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let credentials = Credentials::new(ACCESS_KEY, SECRET_KEY);
    let s3 = create_s3_server(
        MemoryFs::empty(),
        S3ServerOptions::default(),
        Some(credentials),
        Some(REGION.to_owned()),
    )
    .await?;

    let webdav = create_webdav_server(
        Arc::new(FixedMtimeDriver::new(MemoryFs::empty())),
        WebdavServerOptions::default().with_credentials(WEBDAV_USER, WEBDAV_PASSWORD),
    )?;
    webdav.listen().await?;

    println!(
        "MOUNT_RS_HTTP_ORACLE_READY {}",
        serde_json::json!({
            "s3": s3.url(),
            "webdav": webdav.url(),
        })
    );
    io::stdout().flush()?;

    // A blocking stdin read is isolated from the Tokio workers. The harness
    // closes the pipe after the differential run, which gives both servers a
    // normal close path without requiring platform-specific signal APIs.
    tokio::task::spawn_blocking(|| {
        let mut byte = [0_u8; 1];
        let _ = io::stdin().read(&mut byte);
    })
    .await?;

    s3.close().await?;
    webdav.close().await?;
    Ok(())
}
