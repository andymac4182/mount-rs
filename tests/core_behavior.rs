use mount_rs_core::{
    ErrorCode, Loopback, MemoryFs, MemoryOptions, MkdirOptions, S_IFCHR, S_IFIFO, S_IFSOCK,
};

async fn memory() -> Loopback {
    Loopback::new(MemoryFs::empty())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recursive_mkdir_reports_creation_atomically() {
    use mount_rs_core::FsDriver;
    let fs = MemoryFs::empty();
    let mut tasks = Vec::new();
    for _ in 0..16 {
        let fs = fs.clone();
        tasks.push(tokio::spawn(async move {
            fs.mkdir(
                "/first/second",
                MkdirOptions {
                    recursive: true,
                    mode: None,
                },
            )
            .await
            .unwrap()
        }));
    }
    let mut created = Vec::new();
    for task in tasks {
        if let Some(path) = task.await.unwrap() {
            created.push(path);
        }
    }
    assert_eq!(created, ["/first"]);
    assert_eq!(
        fs.mkdir("/single", MkdirOptions::default()).await.unwrap(),
        None
    );
    assert_eq!(
        fs.mkdir(
            "/",
            MkdirOptions {
                recursive: true,
                mode: None
            }
        )
        .await
        .unwrap(),
        None
    );
}

#[tokio::test]
async fn rename_rejects_resolved_descendants_through_symlinks() {
    let fs = memory().await;
    fs.mkdir("/dir", MkdirOptions::default()).await.unwrap();
    fs.write_file("/dir/file", b"unchanged").await.unwrap();
    fs.symlink("dir", "/alias").await.unwrap();
    assert_eq!(
        fs.rename("/dir", "/alias/file").await.unwrap_err().code,
        ErrorCode::Einval
    );
    assert_eq!(fs.read_file("/dir/file").await.unwrap(), b"unchanged");
}

#[test]
fn malformed_snapshots_are_rejected_before_restoration() {
    let bytes = MemoryFs::empty().snapshot_bytes().unwrap();
    let original: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let mutations: Vec<serde_json::Value> = {
        let mut duplicate = original.clone();
        let root = duplicate["nodes"][0].clone();
        duplicate["nodes"].as_array_mut().unwrap().push(root);
        let mut dangling = original.clone();
        dangling["nodes"][0]["children"] = serde_json::json!([["missing", 999]]);
        let mut cycle = original.clone();
        cycle["nodes"][0]["children"] = serde_json::json!([["self", 1]]);
        let mut invalid_name = original.clone();
        invalid_name["nodes"][0]["children"] = serde_json::json!([["../escape", 1]]);
        let mut invalid_count = original.clone();
        invalid_count["nodes"][0]["subdirs"] = serde_json::json!(1);
        let mut invalid_inode = original.clone();
        invalid_inode["next_ino"] = serde_json::json!(1);
        vec![
            duplicate,
            dangling,
            cycle,
            invalid_name,
            invalid_count,
            invalid_inode,
        ]
    };
    for snapshot in mutations {
        let result = MemoryFs::from_snapshot(&serde_json::to_vec(&snapshot).unwrap());
        assert!(result.is_err(), "accepted invalid snapshot: {snapshot}");
    }
    assert!(MemoryFs::from_snapshot(&bytes).is_ok());
}

#[tokio::test]
async fn snapshot_restores_directory_links_and_opaque_symlinks() {
    let memory = MemoryFs::empty();
    let fs = Loopback::new(memory.clone());
    fs.mkdir("/dir", MkdirOptions::default()).await.unwrap();
    fs.write_file("/dir/file", b"persisted").await.unwrap();
    fs.link("/dir/file", "/alias").await.unwrap();
    fs.symlink("dir/file", "/sym").await.unwrap();
    let restored =
        Loopback::new(MemoryFs::from_snapshot(&memory.snapshot_bytes().unwrap()).unwrap());
    assert_eq!(restored.read_file("/sym").await.unwrap(), b"persisted");
    assert_eq!(restored.stat("/alias").await.unwrap().nlink, 2);
    assert_eq!(restored.stat("/").await.unwrap().nlink, 3);
    assert_eq!(restored.readlink("/sym").await.unwrap(), "dir/file");
}

#[tokio::test]
async fn directory_insertion_order_survives_rename_and_snapshot() {
    use mount_rs_core::FsDriver;
    let memory = MemoryFs::empty();
    for name in ["z", "a", "m"] {
        memory
            .open(&format!("/{name}"), "w", 0o644)
            .await
            .unwrap()
            .close()
            .await
            .unwrap();
    }
    let names = |entries: Vec<mount_rs_core::DirEntry>| {
        entries.into_iter().map(|e| e.name).collect::<Vec<_>>()
    };
    assert_eq!(names(memory.readdir("/").await.unwrap()), ["z", "a", "m"]);
    memory.rename("/z", "/last").await.unwrap();
    assert_eq!(
        names(memory.readdir("/").await.unwrap()),
        ["a", "m", "last"]
    );
    let restored = MemoryFs::from_snapshot(&memory.snapshot_bytes().unwrap()).unwrap();
    assert_eq!(
        names(restored.readdir("/").await.unwrap()),
        ["a", "m", "last"]
    );
    assert!(!memory.capabilities().durable_writes);
}

#[tokio::test]
async fn core_filesystem_operations_match_mountx_shape() {
    let fs = memory().await;
    fs.mkdir(
        "/docs/notes",
        MkdirOptions {
            recursive: true,
            mode: None,
        },
    )
    .await
    .unwrap();
    fs.write_file("/docs/notes/hello.txt", b"hello")
        .await
        .unwrap();
    assert_eq!(
        fs.read_file("/docs/notes/hello.txt").await.unwrap(),
        b"hello"
    );

    fs.link("/docs/notes/hello.txt", "/docs/notes/hard.txt")
        .await
        .unwrap();
    fs.symlink("notes/hello.txt", "/docs/link").await.unwrap();
    assert_eq!(fs.read_file("/docs/link").await.unwrap(), b"hello");
    assert!(fs.lstat("/docs/link").await.unwrap().is_symbolic_link());
    assert_eq!(fs.readlink("/docs/link").await.unwrap(), "notes/hello.txt");

    fs.rename("/docs/notes", "/archive").await.unwrap();
    assert_eq!(fs.read_file("/archive/hello.txt").await.unwrap(), b"hello");
    // The relative symlink remains relative to /docs; renaming its target
    // subtree does not rewrite opaque symlink contents.
    assert_eq!(
        fs.read_file("/docs/link").await.unwrap_err().code,
        ErrorCode::Enoent
    );
    assert_eq!(fs.readdir("/archive").await.unwrap().len(), 2);

    let stat = fs.stat("/archive/hello.txt").await.unwrap();
    assert_eq!(stat.nlink, 2);
    assert_eq!(stat.size, 5);
}

#[tokio::test]
async fn open_handle_survives_unlink() {
    let fs = memory().await;
    fs.write_file("/orphan", b"still here").await.unwrap();
    let handle = fs.open("/orphan", "r+", 0).await.unwrap();
    fs.unlink("/orphan").await.unwrap();
    let mut buffer = vec![0; 10];
    let count = handle.read(&mut buffer, Some(0)).await.unwrap();
    assert_eq!(&buffer[..count], b"still here");
    handle.write(b"X", Some(0)).await.unwrap();
    handle.close().await.unwrap();
    assert!(fs.stat("/orphan").await.is_err());
}

#[tokio::test]
async fn positional_overwrite_preserves_the_remaining_file() {
    let fs = memory().await;
    fs.write_file("/file", b"abcdef").await.unwrap();
    let handle = fs.open("/file", "r+", 0).await.unwrap();
    handle.write(b"XY", Some(1)).await.unwrap();
    handle.write(b"", Some(0)).await.unwrap();
    assert_eq!(handle.stat().await.unwrap().size, 6);
    handle.close().await.unwrap();
    assert_eq!(fs.read_file("/file").await.unwrap(), b"aXYdef");
}

#[tokio::test]
async fn decoded_flags_support_create_without_truncate_and_without_write() {
    let fs = memory().await;
    fs.write_file("/file", b"abcdef").await.unwrap();
    let flags = mount_rs_core::OpenFlags {
        read: false,
        write: true,
        create: true,
        truncate: false,
        append: false,
        exclusive: false,
    };
    let handle = fs.open_flags("/file", flags, 0o640).await.unwrap();
    handle.write(b"XY", Some(0)).await.unwrap();
    handle.close().await.unwrap();
    assert_eq!(fs.read_file("/file").await.unwrap(), b"XYcdef");
    let readonly_create = mount_rs_core::OpenFlags {
        read: true,
        write: false,
        ..flags
    };
    let handle = fs
        .open_flags("/created", readonly_create, 0o640)
        .await
        .unwrap();
    assert_eq!(handle.stat().await.unwrap().mode & 0o777, 0o640);
    assert_eq!(
        handle.write(b"x", None).await.unwrap_err().code,
        ErrorCode::Ebadf
    );
    handle.close().await.unwrap();
}

#[tokio::test]
async fn errors_are_explicit_and_root_is_not_removable() {
    let fs = memory().await;
    let error = fs.stat("/missing").await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Enoent);
    assert_eq!(fs.rmdir("/").await.unwrap_err().code, ErrorCode::Ebusy);
    fs.write_file("/file", b"x").await.unwrap();
    assert_eq!(
        fs.readdir("/file").await.unwrap_err().code,
        ErrorCode::Enotdir
    );
}

#[tokio::test]
async fn memory_options_links_and_special_nodes_match_mountx() {
    let fs = Loopback::new(MemoryFs::new(MemoryOptions {
        uid: 501,
        gid: 20,
        umask: 0o077,
        root_mode: 0o755,
    }));
    fs.mkdir(
        "/dir",
        MkdirOptions {
            recursive: false,
            mode: Some(0o777),
        },
    )
    .await
    .unwrap();
    fs.write_file("/file", b"x").await.unwrap();
    let dir = fs.stat("/dir").await.unwrap();
    let file = fs.stat("/file").await.unwrap();
    assert_eq!(dir.mode & 0o777, 0o700);
    assert_eq!(file.mode & 0o777, 0o600);
    assert_eq!((file.uid, file.gid), (501, 20));

    fs.link("/file", "/hard").await.unwrap();
    fs.chmod("/hard", 0o640).await.unwrap();
    assert_eq!(fs.stat("/file").await.unwrap().mode & 0o777, 0o640);
    assert_eq!(fs.stat("/file").await.unwrap().nlink, 2);

    fs.mknod("/fifo", S_IFIFO | 0o644, 0).await.unwrap();
    fs.mknod("/sock", S_IFSOCK | 0o600, 99).await.unwrap();
    fs.mknod("/null", S_IFCHR | 0o666, (1 << 8) | 3)
        .await
        .unwrap();
    assert_eq!(fs.stat("/fifo").await.unwrap().rdev, 0);
    assert_eq!(fs.stat("/sock").await.unwrap().rdev, 0);
    assert_eq!(fs.stat("/null").await.unwrap().rdev, (1 << 8) | 3);
    assert_eq!(
        fs.open("/fifo", "r", 0).await.err().unwrap().code,
        ErrorCode::Enxio
    );
    assert_eq!(
        fs.truncate("/fifo", 0).await.unwrap_err().code,
        ErrorCode::Einval
    );
}

#[tokio::test]
async fn directory_links_and_statfs_stay_consistent() {
    let fs = memory().await;
    assert_eq!(fs.stat("/").await.unwrap().nlink, 2);
    fs.mkdir("/a", MkdirOptions::default()).await.unwrap();
    fs.mkdir("/b", MkdirOptions::default()).await.unwrap();
    assert_eq!(fs.stat("/").await.unwrap().nlink, 4);
    fs.rmdir("/b").await.unwrap();
    assert_eq!(fs.stat("/").await.unwrap().nlink, 3);

    let before = fs.statfs("/").await.unwrap();
    fs.write_file("/a/file", &vec![b'x'; 64 * 1024])
        .await
        .unwrap();
    let after = fs.statfs("/").await.unwrap();
    assert_eq!(before.blocks_free - after.blocks_free, 16);
    assert_eq!(before.files_free - after.files_free, 1);
    fs.truncate("/a/file", 0).await.unwrap();
    assert_eq!(
        fs.statfs("/").await.unwrap().blocks_free,
        before.blocks_free
    );
}

#[tokio::test]
async fn paths_symlinks_and_rename_do_not_retain_stale_nodes() {
    let fs = memory().await;
    fs.mkdir(
        "/one/deep",
        MkdirOptions {
            recursive: true,
            mode: None,
        },
    )
    .await
    .unwrap();
    fs.mkdir(
        "/two/deep",
        MkdirOptions {
            recursive: true,
            mode: None,
        },
    )
    .await
    .unwrap();
    fs.write_file("/one/deep/file", b"one").await.unwrap();
    fs.write_file("/two/deep/file", b"two").await.unwrap();
    fs.symlink("/one", "/link").await.unwrap();
    assert_eq!(fs.read_file("/link/deep/file").await.unwrap(), b"one");
    fs.unlink("/link").await.unwrap();
    fs.symlink("/two", "/link").await.unwrap();
    assert_eq!(fs.read_file("/link/deep/file").await.unwrap(), b"two");
    fs.rename("/two/deep", "/two/moved").await.unwrap();
    assert_eq!(
        fs.stat("/link/deep/file").await.unwrap_err().code,
        ErrorCode::Enoent
    );
    assert_eq!(fs.read_file("/link/moved/file").await.unwrap(), b"two");

    fs.mkdir(
        "/Dir",
        MkdirOptions {
            recursive: false,
            mode: None,
        },
    )
    .await
    .unwrap();
    fs.mkdir(
        "/dir",
        MkdirOptions {
            recursive: false,
            mode: None,
        },
    )
    .await
    .unwrap();
    fs.write_file("/Dir/file", b"upper").await.unwrap();
    fs.write_file("/dir/file", b"lower").await.unwrap();
    assert_eq!(fs.read_file("/dir/../Dir/file").await.unwrap(), b"upper");
    assert_eq!(
        fs.stat("/Dir/../../../dir").await.unwrap().ino,
        fs.stat("/dir").await.unwrap().ino
    );
}

#[tokio::test]
async fn snapshots_do_not_resurrect_unlinked_open_files() {
    let fs = MemoryFs::empty();
    let loopback = Loopback::new(fs.clone());
    loopback.write_file("/orphan", b"orphan").await.unwrap();
    let handle = loopback.open("/orphan", "r+", 0).await.unwrap();
    loopback.unlink("/orphan").await.unwrap();
    let snapshot = fs.snapshot_bytes().unwrap();
    let restored = MemoryFs::from_snapshot(&snapshot).unwrap();
    let restored = Loopback::new(restored);
    assert_eq!(
        restored.stat("/orphan").await.unwrap_err().code,
        ErrorCode::Enoent
    );
    assert_eq!(
        restored.statfs("/").await.unwrap().files_free,
        1024 * 1024 - 1
    );
    assert_eq!(handle.read(&mut [0; 6], Some(0)).await.unwrap(), 6);
    handle.close().await.unwrap();
}

#[tokio::test]
async fn chown_sentinel_keeps_the_selected_owner() {
    let fs = memory().await;
    fs.write_file("/file", b"x").await.unwrap();
    fs.chown("/file", 123, 456).await.unwrap();
    fs.chown("/file", u32::MAX, 789).await.unwrap();
    let stat = fs.stat("/file").await.unwrap();
    assert_eq!((stat.uid, stat.gid), (123, 789));
}

#[tokio::test]
async fn oversized_memory_files_fail_with_efbig_without_mutating_data() {
    let fs = memory().await;
    fs.write_file("/file", b"x").await.unwrap();
    assert_eq!(
        fs.truncate("/file", 1_000_000_000_000_000)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Efbig
    );
    let handle = fs.open("/file", "r+", 0).await.unwrap();
    assert_eq!(
        handle
            .truncate(1_000_000_000_000_000)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Efbig
    );
    handle.close().await.unwrap();
    assert_eq!(fs.read_file("/file").await.unwrap(), b"x");
}
