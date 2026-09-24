//! Real Linux FUSE MRC3 directory checkout qualification. Opt-in only.
#![cfg(target_os = "linux")]
use mount_rs_chunked::{ChunkedFs, ChunkedOptions, OwnershipMode};
use mount_rs_core::delegation::{DelegatedPublish, DelegatedRecovery, DirectoryGrant};
use mount_rs_core::storage::{BlockStore, MetadataStore};
use mount_rs_core::{ErrorCode, Loopback, MkdirOptions};
use mount_rs_fuse::mount::{MountOptions, mount};
use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};
use std::{io::Write, path::Path, process::Stdio, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, Command},
};

fn options(owner: &str, scope: &str) -> ChunkedOptions {
    ChunkedOptions::fixed(owner, 4096)
        .unwrap()
        .with_ownership_mode(OwnershipMode::Shared)
        .with_checkout_path(scope)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "private child service; invoke through main native shared qualification"]
async fn native_shared_service_child() {
    let metadata = std::env::var("MOUNT_RS_SHARED_METADATA").unwrap();
    let blocks = std::env::var("MOUNT_RS_SHARED_BLOCKS").unwrap();
    let mountpoint = std::env::var("MOUNT_RS_SHARED_MOUNT").unwrap();
    let scope = std::env::var("MOUNT_RS_SHARED_SCOPE").unwrap();
    let owner = std::env::var("MOUNT_RS_SHARED_OWNER").unwrap();
    let fs = ChunkedFs::open(
        SqliteMetadataStore::open(metadata).unwrap(),
        SqliteBlockStore::open(blocks).unwrap(),
        options(&owner, &scope),
    )
    .await
    .unwrap();
    let mounted = mount(
        Arc::new(fs.clone()),
        mountpoint,
        MountOptions {
            default_permissions: false,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    println!("SHARED_READY");
    std::io::stdout().flush().unwrap();
    let line = tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).unwrap();
        line
    })
    .await
    .unwrap();
    assert_eq!(line.trim(), "quit");
    mounted.unmount().await.unwrap();
    fs.checkin_scope().await.unwrap();
    fs.shutdown().await.unwrap();
}

async fn service(
    metadata: &Path,
    blocks: &Path,
    mountpoint: &Path,
    scope: &str,
    owner: &str,
) -> Child {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "native_shared_service_child",
            "--nocapture",
        ])
        .env("MOUNT_RS_SHARED_METADATA", metadata)
        .env("MOUNT_RS_SHARED_BLOCKS", blocks)
        .env("MOUNT_RS_SHARED_MOUNT", mountpoint)
        .env("MOUNT_RS_SHARED_SCOPE", scope)
        .env("MOUNT_RS_SHARED_OWNER", owner)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let mut line = String::new();
            assert!(
                stdout.read_line(&mut line).await.unwrap() > 0,
                "child exited before native mount ready"
            );
            if line.trim() == "SHARED_READY" {
                break;
            }
        }
    })
    .await
    .expect("native child readiness deadline");
    // Child helper prints the test result on exit; keep a reader draining it.
    tokio::spawn(async move {
        let mut sink = tokio::io::sink();
        let _ = tokio::io::copy(&mut stdout, &mut sink).await;
    });
    child
}

async fn graceful(mut child: Child) {
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"quit\n")
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(20), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
}

async fn python(path: &Path, verify: bool) {
    let mut command = Command::new("python3");
    command
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/sqlite_shared_qualification.py"
        ))
        .arg(path)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .kill_on_drop(true);
    if verify {
        command.arg("--verify-only");
    }
    let status = tokio::time::timeout(Duration::from_secs(180), command.status())
        .await
        .expect("real SQLite workload deadline")
        .unwrap();
    assert!(
        status.success(),
        "real shared-checkout SQLite failed: {status}"
    );
}

async fn stale_publish(metadata: &SqliteMetadataStore, grant: &DirectoryGrant) {
    let state = metadata.delegation_state().await.unwrap().unwrap();
    let loaded = metadata.load().await.unwrap();
    let result = metadata
        .publish_delegated(
            &DelegatedPublish {
                backing: state.backing,
                token: grant.token.clone(),
                expected_revision: loaded.revision,
            },
            loaded.namespace.unwrap(),
        )
        .await;
    assert_eq!(
        result.unwrap_err().code,
        ErrorCode::Estale,
        "retired owner publication must fail"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires explicit Linux kernel FUSE MRC3 directory checkout qualification"]
async fn shared_checkout_native_sqlite_disjoint_handoff_and_crash_recovery() {
    assert_eq!(
        std::env::var("MOUNT_RS_RUN_NATIVE_FUSE").as_deref(),
        Ok("1")
    );
    // Deliberately retain paths on failure rather than recursively removing mounts.
    let root = tempfile::tempdir().unwrap().keep();
    let metadata_path = root.join("metadata.sqlite");
    let blocks_path = root.join("blocks.sqlite");
    let metadata = SqliteMetadataStore::open(&metadata_path).unwrap();
    let blocks = SqliteBlockStore::open(&blocks_path).unwrap();
    let bootstrap = ChunkedFs::open(
        metadata.clone(),
        blocks.clone(),
        ChunkedOptions::fixed("native-shared-bootstrap", 4096).unwrap(),
    )
    .await
    .unwrap();
    let loopback = Loopback::new(bootstrap.clone());
    for path in ["/left", "/right", "/left/nested"] {
        loopback.mkdir(path, MkdirOptions::default()).await.unwrap();
    }
    drop(loopback);
    bootstrap.shutdown().await.unwrap();
    drop(bootstrap);
    let backing = blocks.prepare_concurrent_backing().await.unwrap();
    metadata
        .prepare_delegated_mode(backing, metadata.load().await.unwrap().revision)
        .await
        .unwrap();
    let left_mount = root.join("mount-left");
    let right_mount = root.join("mount-right");
    for path in [&left_mount, &right_mount] {
        std::fs::create_dir(path).unwrap();
    }
    let left = service(
        &metadata_path,
        &blocks_path,
        &left_mount,
        "/left",
        "native-left-first",
    )
    .await;
    let right = service(
        &metadata_path,
        &blocks_path,
        &right_mount,
        "/right",
        "native-right",
    )
    .await;
    let grants = metadata.delegation_state().await.unwrap().unwrap();
    assert_eq!(grants.grants.len(), 2);
    let left_grant = grants
        .grants
        .values()
        .find(|g| g.token.owner.starts_with("native-left-first-"))
        .unwrap()
        .clone();
    let right_grant = grants
        .grants
        .values()
        .find(|g| g.token.owner.starts_with("native-right-"))
        .unwrap()
        .clone();
    for scope in ["/left", "/", "/left/nested"] {
        let result = ChunkedFs::open(
            SqliteMetadataStore::open(&metadata_path).unwrap(),
            SqliteBlockStore::open(&blocks_path).unwrap(),
            options("native-overlap", scope),
        )
        .await;
        assert_eq!(
            result.err().expect("overlapping grant was accepted").code,
            ErrorCode::Estale
        );
    }
    let left_directory = left_mount.join("left");
    let right_directory = right_mount.join("right");
    tokio::join!(
        python(&left_directory, false),
        python(&right_directory, false)
    );
    graceful(left).await;
    stale_publish(&metadata, &left_grant).await;
    let mut handed = service(
        &metadata_path,
        &blocks_path,
        &left_mount,
        "/left",
        "native-left-handoff",
    )
    .await;
    python(&left_mount.join("left"), true).await;
    let grant = metadata
        .delegation_state()
        .await
        .unwrap()
        .unwrap()
        .grants
        .values()
        .find(|g| g.token.owner.starts_with("native-left-handoff-"))
        .unwrap()
        .clone();
    handed.start_kill().unwrap();
    let killed = handed.wait().await.unwrap();
    assert_eq!(
        std::os::unix::process::ExitStatusExt::signal(&killed),
        Some(9)
    );
    assert!(
        Command::new("fusermount3")
            .args(["-u", "-z", "--"])
            .arg(&left_mount)
            .status()
            .await
            .unwrap()
            .success()
    );
    let blocked = ChunkedFs::open(
        SqliteMetadataStore::open(&metadata_path).unwrap(),
        SqliteBlockStore::open(&blocks_path).unwrap(),
        options("native-before-recovery", "/left"),
    )
    .await;
    assert_eq!(
        blocked.err().expect("crashed grant was bypassed").code,
        ErrorCode::Estale
    );
    let recovery = DelegatedRecovery {
        backing,
        root: grant.token.root,
        expected_fence: grant.token.fence,
    };
    let mut wrong = recovery.clone();
    wrong.expected_fence += 1;
    assert_eq!(
        metadata.recover(&wrong).await.unwrap_err().code,
        ErrorCode::Estale
    );
    metadata.recover(&recovery).await.unwrap();
    stale_publish(&metadata, &grant).await;
    assert_eq!(
        metadata
            .delegation_state()
            .await
            .unwrap()
            .unwrap()
            .grants
            .get(&right_grant.token.root),
        Some(&right_grant),
        "recovering left must preserve disjoint right ownership"
    );
    let recovered = service(
        &metadata_path,
        &blocks_path,
        &left_mount,
        "/left",
        "native-left-recovered",
    )
    .await;
    let recovered_authority = metadata.delegation_state().await.unwrap().unwrap();
    let recovered_grant = recovered_authority.grants.get(&grant.token.root).unwrap();
    assert!(
        recovered_grant
            .token
            .owner
            .starts_with("native-left-recovered-")
    );
    assert!(recovered_grant.token.fence > grant.token.fence);
    tokio::join!(
        python(&left_directory, true),
        python(&right_directory, true)
    );
    graceful(recovered).await;
    graceful(right).await;
    assert!(
        metadata
            .delegation_state()
            .await
            .unwrap()
            .unwrap()
            .grants
            .is_empty()
    );
    for path in [&left_mount, &right_mount] {
        assert!(
            !Command::new("mountpoint")
                .args(["-q"])
                .arg(path)
                .status()
                .await
                .unwrap()
                .success()
        );
        std::fs::remove_dir(path).unwrap();
    }
    drop(metadata);
    drop(blocks);
    // No live mount remains; remove only the known provider files.
    for base in [&metadata_path, &blocks_path] {
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let path = std::path::PathBuf::from(format!("{}{suffix}", base.display()));
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => panic!("provider cleanup: {e}"),
            }
        }
    }
    std::fs::remove_dir(root).unwrap();
}
