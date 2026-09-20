//! Small child process used by the Linux SQLite-hosting crash harness.
//!
//! The service owns the FUSE session and a split-store `ChunkedFs`.  It does
//! not handle SIGKILL: an abrupt process death is deliberately part of the
//! acceptance test, leaving the provider lease for the next process to fence
//! or wait out.  A line containing `quit` on stdin is the separate graceful
//! shutdown path.

use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_fuse::mount::{MountMode, MountOptions, mount};
use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};
use std::env;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::time::Duration;

// Short enough to bound crash recovery, but long enough for one SQLite/FUSE
// operation on a loaded Linux CI runner.
const DEFAULT_LEASE_TTL_MS: u64 = 5_000;
const CHUNK_SIZE: usize = 4 * 1024;

struct Arguments {
    metadata: PathBuf,
    blocks: PathBuf,
    mountpoint: PathBuf,
    owner: String,
    lease_ttl: Duration,
}

fn usage() -> &'static str {
    "usage: sqlite_mount_service <metadata.sqlite> <blocks.sqlite> <mountpoint> <owner> [lease_ttl_ms]"
}

fn arguments() -> Result<Arguments, String> {
    let mut values = env::args_os().skip(1);
    let Some(metadata) = values.next() else {
        return Err(usage().to_owned());
    };
    let Some(blocks) = values.next() else {
        return Err(usage().to_owned());
    };
    let Some(mountpoint) = values.next() else {
        return Err(usage().to_owned());
    };
    let Some(owner) = values.next() else {
        return Err(usage().to_owned());
    };
    let owner = owner
        .into_string()
        .map_err(|_| "owner must be valid UTF-8".to_owned())?;
    if owner.is_empty() {
        return Err("owner must not be empty".to_owned());
    }
    let lease_ttl_ms = values
        .next()
        .map(|value| {
            value
                .into_string()
                .map_err(|_| "lease_ttl_ms must be valid UTF-8".to_owned())?
                .parse::<u64>()
                .map_err(|_| "lease_ttl_ms must be an integer number of milliseconds".to_owned())
        })
        .transpose()?
        .unwrap_or(DEFAULT_LEASE_TTL_MS);
    if values.next().is_some() {
        return Err(usage().to_owned());
    }
    if lease_ttl_ms == 0 {
        return Err("lease_ttl_ms must be greater than zero".to_owned());
    }

    Ok(Arguments {
        metadata: PathBuf::from(metadata),
        blocks: PathBuf::from(blocks),
        mountpoint: PathBuf::from(mountpoint),
        owner,
        lease_ttl: Duration::from_millis(lease_ttl_ms),
    })
}

fn mount_mode() -> Result<MountMode, String> {
    match env::var("MOUNT_RS_FUSE_MODE")
        .unwrap_or_else(|_| "auto".to_owned())
        .to_ascii_lowercase()
        .as_str()
    {
        "auto" => Ok(MountMode::Auto),
        "rootless" => Ok(MountMode::Rootless),
        "privileged" => Ok(MountMode::Privileged),
        value => Err(format!(
            "MOUNT_RS_FUSE_MODE must be auto, rootless, or privileged (got {value:?})"
        )),
    }
}

fn announce(line: &str) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{line}")?;
    stdout.flush()
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = arguments().map_err(io::Error::other)?;
    let mode = mount_mode().map_err(io::Error::other)?;

    let metadata = SqliteMetadataStore::open(&arguments.metadata)?;
    let blocks = SqliteBlockStore::open(&arguments.blocks)?;
    let options =
        ChunkedOptions::fixed(arguments.owner, CHUNK_SIZE)?.with_lease_ttl(arguments.lease_ttl);
    let filesystem = ChunkedFs::open(metadata, blocks, options).await?;

    let mount_options = MountOptions {
        mode,
        fsname: "mount-rs-sqlite-service".to_owned(),
        default_permissions: false,
        init_timeout: Duration::from_secs(15),
        unmount_timeout: Duration::from_secs(10),
        ..MountOptions::default()
    };
    let mounted = match mount(
        std::sync::Arc::new(filesystem.clone()),
        &arguments.mountpoint,
        mount_options,
    )
    .await
    {
        Ok(mounted) => mounted,
        Err(error) => {
            let _ = filesystem.shutdown().await;
            return Err(Box::new(error));
        }
    };

    announce("READY")?;
    tokio::task::spawn_blocking(|| -> io::Result<()> {
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            if line?.trim() == "quit" {
                break;
            }
        }
        Ok(())
    })
    .await??;
    #[cfg(target_os = "linux")]
    {
        // Make graceful shutdown exercise the same block-before-metadata
        // durability barrier exposed to FUSE fsync/fsyncdir callers.
        filesystem.syncfs().await?;
        mounted.unmount().await?;
        filesystem.shutdown().await?;
        announce("STOPPED")?;
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (mounted, filesystem);
        Err(io::Error::other("native FUSE is unsupported on this platform").into())
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    if let Err(error) = run().await {
        let _ = announce(&format!("ERROR {error}"));
        eprintln!("sqlite mount service: {error}");
        std::process::exit(1);
    }
}
