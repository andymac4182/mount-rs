use std::sync::Arc;

use mount_rs_core::{ErrorCode, FsDriver, Loopback, MkdirOptions, OpenFlags};
use mount_rs_memfs::MemoryFs;
use mount_rs_persist::{PersistedFs, StateStore};
use mount_rs_r2::R2Store;
use mount_rs_r2_fs::open_object_store;
use mount_rs_sqlite::SqliteStore;
use mount_rs_sqlite_fs::open_sqlite_memory;
use object_store::ObjectStore;
use object_store::memory::InMemory;
use serde_json::{Value, json};

async fn scenario(driver: Arc<dyn FsDriver>) -> Value {
    let fs = Loopback::from_arc(driver);
    let first_created = fs
        .mkdir(
            "/workspace/src",
            MkdirOptions {
                recursive: true,
                mode: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(first_created.as_deref(), Some("/workspace"));
    assert_eq!(
        fs.mkdir(
            "/workspace/src",
            MkdirOptions {
                recursive: true,
                mode: None
            }
        )
        .await
        .unwrap(),
        None
    );
    fs.write_file("/workspace/src/lib.rs", b"pub fn answer() -> u8 { 42 }\n")
        .await
        .unwrap();
    fs.write_file("/workspace/README.md", b"mount-rs\n")
        .await
        .unwrap();
    let binary_flags = OpenFlags {
        read: true,
        write: true,
        create: true,
        truncate: false,
        append: false,
        exclusive: false,
    };
    let binary = fs
        .open_flags("/workspace/data.bin", binary_flags, 0o640)
        .await
        .unwrap();
    assert_eq!(binary.write(&[0, 255, 1], Some(0)).await.unwrap(), 3);
    binary.close().await.unwrap();
    let append_flags = OpenFlags {
        read: false,
        write: true,
        create: true,
        truncate: false,
        append: true,
        exclusive: false,
    };
    let append = fs
        .open_flags("/workspace/data.bin", append_flags, 0)
        .await
        .unwrap();
    assert_eq!(append.write(&[9, 0], None).await.unwrap(), 2);
    append.close().await.unwrap();
    fs.truncate("/workspace/data.bin", 4).await.unwrap();
    fs.link("/workspace/README.md", "/workspace/README-copy.md")
        .await
        .unwrap();
    fs.rename("/workspace/src", "/workspace/source")
        .await
        .unwrap();
    fs.symlink("source/lib.rs", "/workspace/current")
        .await
        .unwrap();

    scenario_observation(&fs).await
}

async fn scenario_observation(fs: &Loopback) -> Value {
    let mut root = fs.readdir("/workspace").await.unwrap();
    root.sort_by(|left, right| left.name.cmp(&right.name));
    let mut source = fs.readdir("/workspace/source").await.unwrap();
    source.sort_by(|left, right| left.name.cmp(&right.name));
    let stat = fs.stat("/workspace/README.md").await.unwrap();
    let binary_stat = fs.stat("/workspace/data.bin").await.unwrap();
    let current = fs.read_file("/workspace/current").await.unwrap();
    json!({
        "root": root.into_iter().map(|entry| json!({"name": entry.name, "type": format!("{:?}", entry.file_type)})).collect::<Vec<_>>(),
        "source": source.into_iter().map(|entry| json!({"name": entry.name, "type": format!("{:?}", entry.file_type)})).collect::<Vec<_>>(),
        "current": String::from_utf8(current).unwrap(),
        "readme": String::from_utf8(fs.read_file("/workspace/README.md").await.unwrap()).unwrap(),
        "readme_nlink": stat.nlink,
        "readme_size": stat.size,
        "binary": fs.read_file("/workspace/data.bin").await.unwrap(),
        "binary_mode": binary_stat.mode & 0o777,
        "binary_size": binary_stat.size,
        "link_target": fs.readlink("/workspace/current").await.unwrap(),
    })
}

async fn seed_persisted_state(fs: &Loopback) {
    fs.mkdir(
        "/persisted/dir",
        MkdirOptions {
            recursive: true,
            mode: None,
        },
    )
    .await
    .unwrap();
    fs.write_file("/persisted/dir/file", b"durable")
        .await
        .unwrap();
    fs.link("/persisted/dir/file", "/persisted/dir/hard")
        .await
        .unwrap();
    fs.symlink("file", "/persisted/dir/current").await.unwrap();
    fs.chmod("/persisted/dir/file", 0o640).await.unwrap();
    fs.chown("/persisted/dir/file", 123, 456).await.unwrap();
    fs.utimes("/persisted/dir/file", 1_700_000_000_000, 1_700_000_001_000)
        .await
        .unwrap();
    fs.mknod("/persisted/dir/fifo", mount_rs_core::S_IFIFO | 0o644, 0)
        .await
        .unwrap();
}

async fn persisted_observation(fs: &Loopback) -> Value {
    let stat = fs.stat("/persisted/dir/file").await.unwrap();
    let link = fs.lstat("/persisted/dir/current").await.unwrap();
    let fifo = fs.lstat("/persisted/dir/fifo").await.unwrap();
    let statfs = fs.statfs("/").await.unwrap();
    json!({
        "data": String::from_utf8(fs.read_file("/persisted/dir/current").await.unwrap()).unwrap(),
        "nlink": stat.nlink,
        "mode": stat.mode & 0o7777,
        "uid": stat.uid,
        "gid": stat.gid,
        "mtime_ms": stat.mtime_ms,
        "link_target": fs.readlink("/persisted/dir/current").await.unwrap(),
        "link_is_symlink": link.is_symbolic_link(),
        "fifo_mode": fifo.mode,
        "statfs": statfs,
    })
}

async fn seed_conflict_files(fs: &Loopback) {
    fs.write_file("/left", b"base").await.unwrap();
    fs.write_file("/right", b"base").await.unwrap();
}

fn unique_state_key(prefix: &str) -> String {
    format!(
        "{prefix}/{}/{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}

async fn cleanup_live_r2_snapshot(config: &mount_rs_r2::R2Config) {
    R2Store::from_config(config)
        .unwrap()
        .delete_snapshot()
        .await
        .unwrap();
    let verified = R2Store::from_config(config).unwrap();
    assert!(
        verified.load_versioned().await.unwrap().snapshot.is_none(),
        "live R2 snapshot remained after exact-key cleanup"
    );
}

#[tokio::test]
async fn memory_sqlite_and_r2_have_identical_observable_results() {
    let memory = scenario(Arc::new(MemoryFs::empty())).await;
    let sqlite = scenario(Arc::new(open_sqlite_memory().await.unwrap())).await;
    let object_store: Arc<dyn ObjectStore> = Arc::new(InMemory::new());
    let r2 = scenario(Arc::new(
        open_object_store(object_store, "parity/state.json")
            .await
            .unwrap(),
    ))
    .await;
    assert_eq!(memory, sqlite);
    assert_eq!(memory, r2);
}

#[tokio::test]
async fn sqlite_and_r2_snapshots_survive_reopen() {
    let sqlite_store = SqliteStore::in_memory().unwrap();
    let sqlite = PersistedFs::open(sqlite_store.clone()).await.unwrap();
    let sqlite_loopback = Loopback::from_arc(Arc::new(sqlite));
    seed_persisted_state(&sqlite_loopback).await;
    let sqlite_expected = persisted_observation(&sqlite_loopback).await;
    let reopened_sqlite = PersistedFs::open(sqlite_store).await.unwrap();
    let reopened_sqlite = Loopback::from_arc(Arc::new(reopened_sqlite));
    assert_eq!(
        persisted_observation(&reopened_sqlite).await,
        sqlite_expected
    );

    let object_store: Arc<dyn ObjectStore> = Arc::new(InMemory::new());
    let r2_store = R2Store::new(object_store, "reopen/state.json");
    let r2 = PersistedFs::open(r2_store.clone()).await.unwrap();
    let r2_loopback = Loopback::from_arc(Arc::new(r2));
    seed_persisted_state(&r2_loopback).await;
    let r2_expected = persisted_observation(&r2_loopback).await;
    let reopened_r2 = PersistedFs::open(r2_store).await.unwrap();
    let reopened_r2 = Loopback::from_arc(Arc::new(reopened_r2));
    assert_eq!(persisted_observation(&reopened_r2).await, r2_expected);
}

#[tokio::test]
async fn separate_sqlite_instances_reject_stale_whole_snapshot_writes() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("state.db");
    let seed = PersistedFs::open(SqliteStore::open(&database).unwrap())
        .await
        .unwrap();
    seed_conflict_files(&Loopback::from_arc(Arc::new(seed))).await;

    let left = PersistedFs::open(SqliteStore::open(&database).unwrap())
        .await
        .unwrap();
    let right = PersistedFs::open(SqliteStore::open(&database).unwrap())
        .await
        .unwrap();
    let left = Loopback::from_arc(Arc::new(left));
    let right = Loopback::from_arc(Arc::new(right));
    let left_handle = left.open("/left", "r+", 0).await.unwrap();
    let right_handle = right.open("/right", "r+", 0).await.unwrap();

    left_handle.write(b"LEFT", Some(0)).await.unwrap();
    let error = right_handle.write(b"RIGHT", Some(0)).await.unwrap_err();
    assert!(
        error.is(ErrorCode::Eagain),
        "unexpected stale-write error: {error}"
    );

    let reopened = PersistedFs::open(SqliteStore::open(&database).unwrap())
        .await
        .unwrap();
    let reopened = Loopback::from_arc(Arc::new(reopened));
    assert_eq!(reopened.read_file("/left").await.unwrap(), b"LEFT");
    assert_eq!(reopened.read_file("/right").await.unwrap(), b"base");
}

#[tokio::test]
async fn separate_r2_instances_reject_stale_whole_snapshot_writes() {
    let object_store: Arc<dyn ObjectStore> = Arc::new(InMemory::new());
    let state_key = unique_state_key("r2-conflict");
    let reopen_store = Arc::clone(&object_store);
    let seed = PersistedFs::open(R2Store::new(Arc::clone(&object_store), state_key.clone()))
        .await
        .unwrap();
    seed_conflict_files(&Loopback::from_arc(Arc::new(seed))).await;

    let left = PersistedFs::open(R2Store::new(Arc::clone(&object_store), state_key.clone()))
        .await
        .unwrap();
    let right = PersistedFs::open(R2Store::new(Arc::clone(&object_store), state_key.clone()))
        .await
        .unwrap();
    let left = Loopback::from_arc(Arc::new(left));
    let right = Loopback::from_arc(Arc::new(right));
    let left_handle = left.open("/left", "r+", 0).await.unwrap();
    let right_handle = right.open("/right", "r+", 0).await.unwrap();

    left_handle.write(b"LEFT", Some(0)).await.unwrap();
    let error = right_handle.write(b"RIGHT", Some(0)).await.unwrap_err();
    assert!(
        error.is(ErrorCode::Eagain),
        "unexpected stale-write error: {error}"
    );

    let reopened = PersistedFs::open(R2Store::new(reopen_store, state_key))
        .await
        .unwrap();
    let reopened = Loopback::from_arc(Arc::new(reopened));
    assert_eq!(reopened.read_file("/left").await.unwrap(), b"LEFT");
    assert_eq!(reopened.read_file("/right").await.unwrap(), b"base");
}

#[tokio::test]
#[ignore = "requires real PGlite; scripts/test-pglite.sh runs this explicitly"]
async fn pglite_matches_the_same_contract_when_a_socket_is_configured() {
    let url = std::env::var_os("PGLITE_DATABASE_URL")
        .expect("PGLITE_DATABASE_URL is required for the live PGlite integration test");
    let state_key = unique_state_key("pglite-parity");
    let filesystem =
        mount_rs_pglite_fs::connect_pglite_with_key(url.to_str().unwrap(), state_key.clone())
            .await
            .unwrap();
    let actual = scenario(Arc::new(filesystem)).await;
    let expected = scenario(Arc::new(MemoryFs::empty())).await;
    assert_eq!(actual, expected);

    let reopened = mount_rs_pglite_fs::connect_pglite_with_key(url.to_str().unwrap(), state_key)
        .await
        .unwrap();
    let reopened = Loopback::from_arc(Arc::new(reopened));
    assert_eq!(scenario_observation(&reopened).await, expected);
    drop(reopened);

    let conflict_key = unique_state_key("pglite-conflict");
    let seed =
        mount_rs_pglite_fs::connect_pglite_with_key(url.to_str().unwrap(), conflict_key.clone())
            .await
            .unwrap();
    let seed = Loopback::from_arc(Arc::new(seed));
    seed_conflict_files(&seed).await;
    drop(seed);
    {
        let left = mount_rs_pglite_fs::connect_pglite_with_key(
            url.to_str().unwrap(),
            conflict_key.clone(),
        )
        .await
        .unwrap();
        let right = mount_rs_pglite_fs::connect_pglite_with_key(
            url.to_str().unwrap(),
            conflict_key.clone(),
        )
        .await
        .unwrap();
        let left = Loopback::from_arc(Arc::new(left));
        let right = Loopback::from_arc(Arc::new(right));
        let left_handle = left.open("/left", "r+", 0).await.unwrap();
        let right_handle = right.open("/right", "r+", 0).await.unwrap();
        left_handle.write(b"LEFT", Some(0)).await.unwrap();
        let error = right_handle.write(b"RIGHT", Some(0)).await.unwrap_err();
        assert!(
            error.is(ErrorCode::Eagain),
            "unexpected stale-write error: {error}"
        );
    }
    let reopened = mount_rs_pglite_fs::connect_pglite_with_key(url.to_str().unwrap(), conflict_key)
        .await
        .unwrap();
    let reopened = Loopback::from_arc(Arc::new(reopened));
    assert_eq!(reopened.read_file("/left").await.unwrap(), b"LEFT");
    assert_eq!(reopened.read_file("/right").await.unwrap(), b"base");
}

#[tokio::test]
#[ignore = "requires dedicated Cloudflare R2 credentials; run explicitly with --ignored"]
async fn cloudflare_r2_matches_the_same_contract_when_configured() {
    let required = [
        "R2_ENDPOINT",
        "R2_BUCKET",
        "R2_ACCESS_KEY_ID",
        "R2_SECRET_ACCESS_KEY",
    ];
    assert!(
        required.iter().all(|name| std::env::var_os(name).is_some()),
        "R2_ENDPOINT, R2_BUCKET, R2_ACCESS_KEY_ID, and R2_SECRET_ACCESS_KEY are required"
    );
    let mut config = mount_rs_r2::R2Config::from_env().unwrap();
    config.state_key = unique_state_key("r2-parity");
    let store = mount_rs_r2::R2Store::from_config(&config).unwrap();
    let filesystem = mount_rs_persist::PersistedFs::open(store.clone())
        .await
        .unwrap();
    let actual = scenario(Arc::new(filesystem)).await;
    let expected = scenario(Arc::new(MemoryFs::empty())).await;
    assert_eq!(actual, expected);
    cleanup_live_r2_snapshot(&config).await;
}

#[tokio::test]
#[ignore = "requires dedicated Cloudflare R2 credentials; run explicitly with --ignored"]
async fn cloudflare_r2_rejects_concurrent_snapshot_publication_with_fresh_clients() {
    let required = [
        "R2_ENDPOINT",
        "R2_BUCKET",
        "R2_ACCESS_KEY_ID",
        "R2_SECRET_ACCESS_KEY",
    ];
    assert!(
        required.iter().all(|name| std::env::var_os(name).is_some()),
        "R2_ENDPOINT, R2_BUCKET, R2_ACCESS_KEY_ID, and R2_SECRET_ACCESS_KEY are required"
    );
    let mut config = mount_rs_r2::R2Config::from_env().unwrap();
    config.state_key = unique_state_key("r2-concurrent-cas");

    let seed = PersistedFs::open(R2Store::from_config(&config).unwrap())
        .await
        .unwrap();
    let seed = Loopback::from_arc(Arc::new(seed));
    seed.write_file("/base", b"base").await.unwrap();
    drop(seed);

    // Each filesystem has a newly constructed authenticated R2 client and
    // loads the same ETag before racing its first publication.
    let left = Loopback::from_arc(Arc::new(
        PersistedFs::open(R2Store::from_config(&config).unwrap())
            .await
            .unwrap(),
    ));
    let right = Loopback::from_arc(Arc::new(
        PersistedFs::open(R2Store::from_config(&config).unwrap())
            .await
            .unwrap(),
    ));
    let (left_result, right_result) = tokio::join!(
        left.write_file("/left", b"left"),
        right.write_file("/right", b"right")
    );
    let winner = match (left_result, right_result) {
        (Ok(()), Err(error)) => {
            assert!(
                error.is(ErrorCode::Eagain),
                "stale live R2 writer returned the wrong error: {error}"
            );
            ("/left", b"left".as_slice(), "/right")
        }
        (Err(error), Ok(())) => {
            assert!(
                error.is(ErrorCode::Eagain),
                "stale live R2 writer returned the wrong error: {error}"
            );
            ("/right", b"right".as_slice(), "/left")
        }
        (Ok(()), Ok(())) => panic!("both live R2 snapshot writers committed the same ETag"),
        (Err(left), Err(right)) => {
            panic!("both live R2 snapshot writers failed: left={left}; right={right}")
        }
    };
    drop(left);
    drop(right);

    // Reopen with another fresh client and prove that the conditional winner
    // is the only concurrent publication visible in the committed snapshot.
    let reopened = PersistedFs::open(R2Store::from_config(&config).unwrap())
        .await
        .unwrap();
    let reopened = Loopback::from_arc(Arc::new(reopened));
    assert_eq!(reopened.read_file("/base").await.unwrap(), b"base");
    assert_eq!(reopened.read_file(winner.0).await.unwrap(), winner.1);
    assert!(
        reopened
            .stat(winner.2)
            .await
            .unwrap_err()
            .is(ErrorCode::Enoent),
        "losing live R2 writer unexpectedly appeared in the committed snapshot"
    );
    cleanup_live_r2_snapshot(&config).await;
}
