//! Graceful restart coverage for a disk-backed local PGlite socket server.
//!
//! This harness owns the Node server and its temporary `PGLITE_DATA_DIR`, so
//! it does not use an ambient database or credentials. It explicitly shuts
//! down and drops the first split-store filesystem before sending SIGTERM to
//! the server, then starts a fresh server process against the same data
//! directory and reopens the same volume. The result is evidence of the
//! configured server's graceful-restart persistence only; it is not a
//! power-loss or fsync durability claim.

use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::Loopback;
use mount_rs_pglite::{PgliteBlockStore, PgliteMetadataStore, PgliteStorageOptions};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

const RUN_ENV: &str = "MOUNT_RS_RUN_PGLITE_SERVER_LIFECYCLE";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const SIGNAL_TIMEOUT: Duration = Duration::from_secs(5);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);
const PAYLOAD: &[u8] = b"graceful-pglite-restart-payload";

type PgliteFilesystem = ChunkedFs<PgliteMetadataStore, PgliteBlockStore>;

struct OpenFilesystem {
    filesystem: PgliteFilesystem,
    metadata: PgliteMetadataStore,
    blocks: PgliteBlockStore,
}

struct PgliteServer {
    child: Child,
    connection_string: String,
}

impl PgliteServer {
    async fn start(data_dir: &Path) -> Result<Self, String> {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
            .map_err(|error| format!("reserve PGlite TCP port: {error}"))?;
        let port = listener
            .local_addr()
            .map_err(|error| format!("read reserved PGlite TCP port: {error}"))?
            .port();
        drop(listener);

        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/pglite/server.mjs");
        if !script.is_file() {
            return Err(format!(
                "PGlite server helper is missing: {}",
                script.display()
            ));
        }
        let mut child = Command::new("node")
            .arg(&script)
            .env("PGLITE_PORT", port.to_string())
            .env("PGLITE_MAX_CONNECTIONS", "8")
            .env("PGLITE_DATA_DIR", data_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| format!("start PGlite server: {error}"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "PGlite server did not expose a readiness stdout pipe".to_owned())?;
        let mut stdout = BufReader::new(stdout);
        let ready = tokio::time::timeout(STARTUP_TIMEOUT, async {
            let mut line = String::new();
            loop {
                line.clear();
                let length = stdout
                    .read_line(&mut line)
                    .await
                    .map_err(|error| format!("read PGlite server readiness: {error}"))?;
                if length == 0 {
                    return Err("PGlite server exited before PGLITE_READY".to_owned());
                }
                if line.starts_with("PGLITE_READY ") {
                    return Ok(());
                }
            }
        })
        .await;
        drop(stdout);
        match ready {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                terminate_child(&mut child).await;
                return Err(error);
            }
            Err(error) => {
                terminate_child(&mut child).await;
                return Err(format!(
                    "PGlite server startup exceeded {STARTUP_TIMEOUT:?}: {error}"
                ));
            }
        }

        Ok(Self {
            child,
            connection_string: format!(
                "postgresql://postgres:postgres@127.0.0.1:{port}/postgres?sslmode=disable"
            ),
        })
    }

    fn connection_string(&self) -> &str {
        &self.connection_string
    }

    async fn stop(&mut self) -> Result<(), String> {
        let Some(pid) = self.child.id() else {
            return Ok(());
        };
        if self
            .child
            .try_wait()
            .map_err(|error| format!("inspect PGlite server process {pid}: {error}"))?
            .is_some()
        {
            return Ok(());
        }

        let signal = tokio::time::timeout(
            SIGNAL_TIMEOUT,
            Command::new("kill")
                .arg("-TERM")
                .arg(pid.to_string())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .status(),
        )
        .await;
        match signal {
            Ok(Ok(status)) if status.success() => {}
            Ok(Ok(status)) => {
                terminate_child(&mut self.child).await;
                return Err(format!("send SIGTERM to PGlite server failed: {status}"));
            }
            Ok(Err(error)) => {
                terminate_child(&mut self.child).await;
                return Err(format!("send SIGTERM to PGlite server: {error}"));
            }
            Err(_) => {
                terminate_child(&mut self.child).await;
                return Err(format!("send SIGTERM exceeded {SIGNAL_TIMEOUT:?}"));
            }
        }

        match tokio::time::timeout(SHUTDOWN_TIMEOUT, self.child.wait()).await {
            Ok(Ok(status)) if status.success() => Ok(()),
            Ok(Ok(status)) => Err(format!("PGlite server exited after SIGTERM with {status}")),
            Ok(Err(error)) => Err(format!("wait for PGlite server shutdown: {error}")),
            Err(_) => {
                terminate_child(&mut self.child).await;
                Err(format!(
                    "PGlite server did not shut down within {SHUTDOWN_TIMEOUT:?}"
                ))
            }
        }
    }
}

async fn terminate_child(child: &mut Child) {
    let _ = child.start_kill();
    let _ = tokio::time::timeout(SIGNAL_TIMEOUT, child.wait()).await;
}

fn chunked_options(owner: &str) -> ChunkedOptions {
    ChunkedOptions::fixed(owner, 4096).expect("fixed-size chunking configuration")
}

fn pglite_options(key: &str) -> PgliteStorageOptions {
    // The server is configured with a disk data directory, but the provider
    // must not turn a wire acknowledgement into a general durability claim.
    PgliteStorageOptions::new(key).with_durable(false)
}

async fn open_filesystem(url: &str, key: &str, owner: &str) -> Result<OpenFilesystem, String> {
    let options = pglite_options(key);
    let metadata = PgliteMetadataStore::connect_with_options(url, options.clone())
        .await
        .map_err(|error| format!("connect PGlite metadata: {error}"))?;
    let blocks = match PgliteBlockStore::connect_with_options(url, options).await {
        Ok(blocks) => blocks,
        Err(error) => {
            let _ = metadata.close().await;
            return Err(format!("connect PGlite blocks: {error}"));
        }
    };
    let filesystem =
        match ChunkedFs::open(metadata.clone(), blocks.clone(), chunked_options(owner)).await {
            Ok(filesystem) => filesystem,
            Err(error) => {
                let _ = metadata.close().await;
                let _ = blocks.close().await;
                return Err(format!("open split PGlite filesystem: {error}"));
            }
        };
    Ok(OpenFilesystem {
        filesystem,
        metadata,
        blocks,
    })
}

async fn shutdown_filesystem(opened: &OpenFilesystem, phase: &str) -> Result<(), String> {
    // Dropping tokio-postgres clients is asynchronous. Explicitly close both
    // providers after the lease is released so the server cannot race its
    // own disk/database shutdown with detached connection-task teardown.
    let filesystem = opened.filesystem.shutdown().await;
    let metadata = opened.metadata.close().await;
    let blocks = opened.blocks.close().await;
    filesystem
        .err()
        .map(|error| format!("{phase} filesystem shutdown: {error}"))
        .or_else(|| {
            metadata
                .err()
                .map(|error| format!("{phase} metadata close: {error}"))
        })
        .or_else(|| {
            blocks
                .err()
                .map(|error| format!("{phase} blocks close: {error}"))
        })
        .map_or(Ok(()), Err)
}

fn finish_phase(operation: Result<(), String>, shutdown: Result<(), String>) -> Result<(), String> {
    match (operation, shutdown) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(operation), Ok(())) => Err(operation),
        (Ok(()), Err(shutdown)) => Err(shutdown),
        (Err(operation), Err(shutdown)) => Err(format!("{operation}; {shutdown}")),
    }
}

async fn write_and_close(url: &str, key: &str) -> Result<(), String> {
    let opened = open_filesystem(url, key, "pglite-server-first").await?;
    let operation: Result<(), String> = async {
        let loopback = Loopback::new(opened.filesystem.clone());
        loopback
            .write_file("/restart", PAYLOAD)
            .await
            .map_err(|error| format!("write restart payload: {error}"))?;
        opened
            .filesystem
            .syncfs()
            .await
            .map_err(|error| format!("sync PGlite restart payload: {error}"))
    }
    .await;
    let shutdown = shutdown_filesystem(&opened, "first PGlite filesystem").await;
    drop(opened);
    finish_phase(operation, shutdown)
}

async fn reopen_and_read(url: &str, key: &str) -> Result<(), String> {
    let opened = open_filesystem(url, key, "pglite-server-reopen").await?;
    let operation: Result<(), String> = async {
        let loopback = Loopback::new(opened.filesystem.clone());
        let payload = loopback
            .read_file("/restart")
            .await
            .map_err(|error| format!("read restart payload after server restart: {error}"))?;
        if payload != PAYLOAD {
            return Err(format!(
                "PGlite restart payload mismatch: expected {} bytes, got {}",
                PAYLOAD.len(),
                payload.len()
            ));
        }
        Ok(())
    }
    .await;
    let shutdown = shutdown_filesystem(&opened, "reopened PGlite filesystem").await;
    drop(opened);
    finish_phase(operation, shutdown)
}

async fn run_lifecycle() -> Result<(), String> {
    let data_dir =
        tempfile::tempdir().map_err(|error| format!("create PGlite data directory: {error}"))?;
    let key = format!(
        "pglite-restart/{}/{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| format!("system clock before Unix epoch: {error}"))?
            .as_nanos()
    );

    let mut first = PgliteServer::start(data_dir.path()).await?;
    let write_result = write_and_close(first.connection_string(), &key).await;
    let stop_result = first.stop().await;
    if let Err(error) = write_result {
        return Err(format!(
            "first server phase: {error}; shutdown={stop_result:?}"
        ));
    }
    stop_result?;

    let mut second = PgliteServer::start(data_dir.path()).await?;
    let reopen_result = reopen_and_read(second.connection_string(), &key).await;
    let second_stop = second.stop().await;
    if let Err(error) = reopen_result {
        return Err(format!(
            "reopen server phase: {error}; shutdown={second_stop:?}"
        ));
    }
    second_stop
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Node PGlite dependencies and explicit disk-server opt-in"]
async fn pglite_disk_server_graceful_restart_preserves_split_store_state() {
    assert_eq!(
        std::env::var(RUN_ENV).as_deref(),
        Ok("1"),
        "set {RUN_ENV}=1 to run the local disk-backed PGlite restart harness"
    );
    if !cfg!(any(target_os = "macos", target_os = "linux")) {
        panic!("PGlite lifecycle harness supports macOS and Linux only");
    }
    if let Err(error) = run_lifecycle().await {
        panic!("PGlite graceful restart acceptance failed: {error}");
    }
}
