use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use mount_rs_core::{ErrorCode, FsDriver, MkdirOptions, OpenFlags};
use mount_rs_host::HostFs;

struct TempDir(PathBuf);

#[tokio::test]
async fn positional_append_write_does_not_advance_read_cursor() {
    let root = TempDir::new();
    fs::write(root.path().join("file"), b"abc").unwrap();
    let driver = HostFs::new(root.path());
    let handle = driver.open("/file", "a+", 0o600).await.unwrap();
    handle.write(b"X", Some(0)).await.unwrap();
    let expected = fs::read(root.path().join("file")).unwrap();
    let mut first = [0];
    assert_eq!(handle.read(&mut first, None).await.unwrap(), 1);
    assert_eq!(first[0], expected[0]);
    handle.close().await.unwrap();
}

#[tokio::test]
async fn implicit_append_write_uses_the_end_of_the_file() {
    let root = TempDir::new();
    fs::write(root.path().join("file"), b"one").unwrap();
    let driver = HostFs::new(root.path());
    let handle = driver.open("/file", "a", 0o600).await.unwrap();

    assert_eq!(handle.write(b"-two", None).await.unwrap(), 4);
    handle.close().await.unwrap();
    assert_eq!(fs::read(root.path().join("file")).unwrap(), b"one-two");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn state_survives_a_fresh_driver_after_clean_close_and_sync() {
    let root = TempDir::new();
    {
        let driver = HostFs::new(root.path());
        driver
            .mkdir(
                "/state",
                MkdirOptions {
                    recursive: false,
                    mode: Some(0o750),
                },
            )
            .await
            .expect("create state directory");
        let handle = driver
            .open("/state/payload", "w+", 0o600)
            .await
            .expect("open payload");
        assert_eq!(handle.write(b"restart-state", Some(0)).await.unwrap(), 13);
        handle.sync().await.expect("sync payload");
        handle.close().await.expect("close payload");
        driver
            .rename("/state/payload", "/state/committed")
            .await
            .expect("commit payload");
    }

    let driver = HostFs::new(root.path());
    let mut bytes = [0_u8; 32];
    let handle = driver
        .open("/state/committed", "r", 0)
        .await
        .expect("reopen committed payload");
    let count = handle
        .read(&mut bytes, Some(0))
        .await
        .expect("read committed payload");
    handle.close().await.expect("close reopened payload");
    assert_eq!(&bytes[..count], b"restart-state");
    assert_eq!(driver.stat("/state/committed").await.unwrap().size, 13);
    let entries = driver
        .readdir("/state")
        .await
        .expect("list state directory");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "committed");
}

impl TempDir {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nonce = NEXT.fetch_add(1, Ordering::Relaxed);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "mount-rs-host-{}-{timestamp}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create test directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rooted_symlinks_cannot_escape_and_lstat_preserves_links() {
    let sandbox = TempDir::new();
    let root_path = sandbox.path().join("root");
    let outside_path = sandbox.path().join("outside");
    fs::create_dir(&root_path).expect("create root");
    fs::create_dir(&outside_path).expect("create outside");
    fs::create_dir(root_path.join("inside")).expect("create inside");
    fs::write(root_path.join("inside/data"), b"safe").expect("write data");
    fs::write(outside_path.join("secret"), b"outside root").expect("write secret");

    let driver = HostFs::new(&root_path);
    driver
        .symlink("/inside", "/link")
        .await
        .expect("create absolute virtual symlink");
    assert_eq!(
        driver.readlink("/link").await.expect("readlink"),
        fs::read_link(root_path.join("link"))
            .unwrap()
            .to_string_lossy()
    );
    assert_eq!(
        driver
            .stat("/link/data")
            .await
            .expect("stat through link")
            .size,
        4
    );
    assert_eq!(
        driver.lstat("/link").await.expect("lstat link").mode & 0o170000,
        0o120000
    );

    driver
        .symlink("/../../outside/secret", "/absolute-outside")
        .await
        .expect("create absolute target");
    let error = driver
        .stat("/absolute-outside")
        .await
        .expect_err("absolute target must stay virtual");
    assert_eq!(error.code, ErrorCode::Enoent);

    driver
        .symlink("../../secret", "/relative-outside")
        .await
        .expect("create relative target");
    let error = driver
        .stat("/relative-outside")
        .await
        .expect_err("relative target must stay below root");
    assert_eq!(error.code, ErrorCode::Enoent);

    assert_eq!(
        driver
            .stat("/../secret")
            .await
            .expect_err("path is rooted")
            .code,
        ErrorCode::Enoent
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handles_keep_files_open_and_enforce_open_mode() {
    let root = TempDir::new();
    let driver = HostFs::new(root.path());
    let handle = driver.open("/file", "w+", 0o640).await.expect("open file");

    assert_eq!(handle.write(b"abc", None).await.expect("write"), 3);
    let mut bytes = [0_u8; 3];
    assert_eq!(
        handle
            .read(&mut bytes, Some(0))
            .await
            .expect("positional read"),
        3
    );
    assert_eq!(&bytes, b"abc");

    driver.unlink("/file").await.expect("unlink open file");
    assert_eq!(handle.stat().await.expect("stat unlinked handle").size, 3);
    handle.truncate(1).await.expect("truncate handle");
    assert_eq!(handle.stat().await.expect("stat truncated handle").size, 1);
    handle.close().await.expect("close handle");
    assert_eq!(
        handle
            .stat()
            .await
            .expect_err("closed handle is invalid")
            .code,
        ErrorCode::Ebadf
    );

    let read_only = driver
        .open("/new", "r", 0)
        .await
        .err()
        .expect("missing read-only file");
    assert_eq!(read_only.code, ErrorCode::Enoent);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exclusive_create_rejects_dangling_symlinks() {
    let root = TempDir::new();
    let driver = HostFs::new(root.path());
    driver
        .symlink("/missing", "/link")
        .await
        .expect("create dangling link");

    let error = driver
        .open("/link", "wx", 0o600)
        .await
        .err()
        .expect("exclusive create must see link");
    assert_eq!(error.code, ErrorCode::Eexist);
    assert_eq!(
        driver.lstat("/link").await.expect("lstat").mode & 0o170000,
        0o120000
    );

    fs::write(root.path().join("taken"), b"x").expect("write taken path");
    let error = driver
        .symlink("whatever", "/taken")
        .await
        .expect_err("symlink over existing path");
    assert_eq!(error.code, ErrorCode::Eexist);
    assert_eq!(error.path.as_deref(), Some("whatever"));
    assert!(
        error
            .dest
            .as_deref()
            .is_some_and(|path| Path::new(path).ends_with("taken"))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recursive_mkdir_reports_first_creation_and_root_is_busy() {
    let root = TempDir::new();
    let driver = HostFs::new(root.path());

    assert_eq!(
        driver
            .mkdir(
                "/a/b/c",
                MkdirOptions {
                    recursive: true,
                    mode: Some(0o750),
                },
            )
            .await
            .expect("recursive mkdir"),
        Some("/a".to_owned())
    );
    assert_eq!(
        driver
            .mkdir(
                "/a/b/c",
                MkdirOptions {
                    recursive: true,
                    mode: Some(0o750),
                },
            )
            .await
            .expect("repeat recursive mkdir"),
        None
    );
    assert_eq!(
        driver
            .mkdir(
                "/a/b/c",
                MkdirOptions {
                    recursive: false,
                    mode: Some(0o750),
                },
            )
            .await
            .expect_err("existing non-recursive directory")
            .code,
        ErrorCode::Eexist
    );
    fs::write(root.path().join("file"), b"x").expect("write file");
    assert_eq!(
        driver
            .mkdir(
                "/file",
                MkdirOptions {
                    recursive: true,
                    mode: Some(0o750),
                },
            )
            .await
            .expect_err("recursive mkdir over a file")
            .code,
        ErrorCode::Eexist
    );
    assert_eq!(
        driver
            .rmdir("/")
            .await
            .expect_err("root cannot be removed")
            .code,
        ErrorCode::Ebusy
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn symlink_loop_is_bounded() {
    let root = TempDir::new();
    let driver = HostFs::new(root.path());
    for index in 0..=40 {
        let next = format!("/link{}", index + 1);
        driver
            .symlink(&next, &format!("/link{index}"))
            .await
            .expect("create link in loop");
    }
    let error = driver
        .stat("/link0")
        .await
        .expect_err("loop must be bounded");
    assert_eq!(error.code, ErrorCode::Eloop);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn decoded_open_flags_match_string_open() {
    let root = TempDir::new();
    let driver = HostFs::new(root.path());
    let flags = OpenFlags::parse("w+", "/decoded").expect("parse flags");
    let handle = driver
        .open_flags("/decoded", flags, 0o600)
        .await
        .expect("open decoded flags");
    assert_eq!(handle.write(b"ok", None).await.expect("write"), 2);
    handle.close().await.expect("close");
    assert_eq!(driver.stat("/decoded").await.expect("stat").size, 2);

    let read_only_create = driver
        .open_flags(
            "/read-only-created",
            OpenFlags {
                read: true,
                write: false,
                create: true,
                truncate: false,
                append: false,
                exclusive: false,
            },
            0o600,
        )
        .await
        .expect("POSIX permits O_RDONLY | O_CREAT");
    read_only_create
        .write(b"not writable", Some(0))
        .await
        .expect_err("creation must not grant handle write access");
    read_only_create
        .close()
        .await
        .expect("close read-only handle");
    assert_eq!(
        driver
            .stat("/read-only-created")
            .await
            .expect("stat read-only-created")
            .size,
        0
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn decoded_read_only_truncate_rejects_before_touching_host_files() {
    let root = TempDir::new();
    fs::write(root.path().join("existing"), b"preserved").expect("seed host file");
    let driver = HostFs::new(root.path());
    let read_only_truncate = OpenFlags {
        read: true,
        write: false,
        create: false,
        truncate: true,
        append: false,
        exclusive: false,
    };

    let existing = driver
        .open_flags("/existing", read_only_truncate, 0o600)
        .await;
    if let Ok(handle) = &existing {
        handle.close().await.expect("close unexpected handle");
    }
    assert_eq!(
        fs::read(root.path().join("existing")).expect("read host file"),
        b"preserved"
    );
    assert_eq!(
        existing.err().expect("reject truncate").code,
        ErrorCode::Einval
    );

    let missing = driver
        .open_flags(
            "/missing",
            OpenFlags {
                create: true,
                ..read_only_truncate
            },
            0o600,
        )
        .await;
    if let Ok(handle) = &missing {
        handle.close().await.expect("close unexpected handle");
    }
    assert!(!root.path().join("missing").exists());
    assert_eq!(
        missing.err().expect("reject truncate").code,
        ErrorCode::Einval
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn directory_metadata_links_times_and_statfs_use_host_state() {
    let root = TempDir::new();
    let driver = HostFs::new(root.path());
    driver
        .mkdir(
            "/dir",
            MkdirOptions {
                recursive: false,
                mode: Some(0o750),
            },
        )
        .await
        .expect("mkdir");
    let root_entries = driver.readdir("/").await.expect("readdir root");
    assert_eq!(root_entries.len(), 1);
    assert_eq!(root_entries[0].name, "dir");
    assert_eq!(root_entries[0].parent_path, "/");
    assert!(root_entries[0].is_directory());

    let file = driver
        .open("/dir/original", "w", 0o640)
        .await
        .expect("open original");
    file.write(b"payload", None).await.expect("write original");
    file.close().await.expect("close original");
    driver
        .link("/dir/original", "/dir/alias")
        .await
        .expect("hard link");
    let original = driver.stat("/dir/original").await.expect("stat original");
    let alias = driver.stat("/dir/alias").await.expect("stat alias");
    assert_eq!(original.ino, alias.ino);
    assert_eq!(original.nlink, 2);

    driver
        .chmod("/dir/alias", 0o600)
        .await
        .expect("chmod alias");
    #[cfg(windows)]
    let expected_mode = windows_oracle("mode", &root.path().join("dir/original"))
        .parse::<u32>()
        .unwrap();
    #[cfg(not(windows))]
    let expected_mode = 0o600;
    assert_eq!(
        driver.stat("/dir/original").await.expect("stat chmod").mode & 0o777,
        expected_mode
    );
    driver
        .utimes("/dir/original", 1_000, 2_000)
        .await
        .expect("utimes");
    let timed = driver.stat("/dir/alias").await.expect("stat timed link");
    assert_eq!(timed.atime_ms, 1_000);
    assert_eq!(timed.mtime_ms, 2_000);

    driver
        .rename("/dir/alias", "/moved")
        .await
        .expect("rename alias");
    driver
        .unlink("/dir/original")
        .await
        .expect("unlink original");
    assert_eq!(driver.stat("/moved").await.expect("stat moved").nlink, 1);
    assert!(driver.statfs("/").await.expect("statfs").block_size > 0);
    driver.rmdir("/dir").await.expect("remove empty directory");
}

#[cfg(windows)]
fn windows_oracle(operation: &str, path: &Path) -> String {
    windows_oracle_with_args(operation, path, &[])
}

#[cfg(windows)]
fn windows_oracle_with_args(operation: &str, path: &Path, args: &[&str]) -> String {
    let result = std::process::Command::new("node")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/windows-oracle.mjs"))
        .args([std::ffi::OsStr::new(operation), path.as_os_str()])
        .args(args)
        .output()
        .expect("Node is required for Windows HostFs parity tests");
    assert!(
        result.status.success(),
        "Node oracle: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap().trim().to_owned()
}

#[cfg(windows)]
#[tokio::test]
async fn windows_metadata_and_volume_match_node() {
    let root = TempDir::new();
    let driver = HostFs::new(root.path());
    let handle = driver.open("/file", "w+", 0o600).await.unwrap();
    handle.write(&[42; 8193], Some(0)).await.unwrap();
    driver.link("/file", "/alias").await.unwrap();
    let stats = handle.stat().await.unwrap();
    assert_ne!(stats.ino, 0);
    assert_eq!(stats.nlink, 2);
    assert_eq!(stats.ino, driver.stat("/alias").await.unwrap().ino);
    handle.close().await.unwrap();
    for mode in [0o400, 0o600] {
        driver.chmod("/file", mode).await.unwrap();
        driver.utimes("/file", 1_234_000, 5_678_000).await.unwrap();
        let stats = driver.stat("/alias").await.unwrap();
        let observed = format!(
            "{},{},{},{},{},{},{},{},{},{}",
            stats.dev,
            stats.ino,
            stats.nlink,
            stats.mode,
            stats.size,
            stats.atime_ms,
            stats.mtime_ms,
            stats.ctime_ms,
            stats.birthtime_ms,
            stats.blocks
        );
        assert_eq!(observed, windows_oracle("stat", &root.path().join("alias")));
    }
    let volume = driver.statfs("/file").await.unwrap();
    assert!(volume.blocks > 0 && volume.block_size > 0);
    assert!(volume.blocks_free <= volume.blocks && volume.blocks_available <= volume.blocks_free);
    assert_eq!(
        format!(
            "{},{},{},{},{}",
            volume.filesystem_type,
            volume.block_size,
            volume.blocks,
            volume.files,
            volume.files_free
        ),
        windows_oracle("statfs", &root.path().join("file"))
    );
    assert_eq!(
        driver.statfs("/missing").await.unwrap_err().code,
        ErrorCode::Enoent
    );
    // Node's ownership methods intentionally do not emulate POSIX ownership.
    driver.chown("/missing", 123, 456).await.unwrap();
    assert_eq!(windows_oracle("chown", &root.path().join("missing")), "ok");
}

#[cfg(windows)]
#[tokio::test]
async fn windows_hard_link_metadata_and_open_handle_match_node() {
    let root = TempDir::new();
    let driver = HostFs::new(root.path());
    let handle = driver.open("/rust-hard-links", "w+", 0o600).await.unwrap();
    handle.write(b"payload", None).await.unwrap();
    driver
        .link("/rust-hard-links", "/rust-hard-links.first")
        .await
        .unwrap();
    driver
        .link("/rust-hard-links", "/rust-hard-links.second")
        .await
        .unwrap();

    let before = [
        driver.stat("/rust-hard-links").await.unwrap(),
        driver.stat("/rust-hard-links.first").await.unwrap(),
        driver.stat("/rust-hard-links.second").await.unwrap(),
    ];
    driver.unlink("/rust-hard-links.first").await.unwrap();
    let after_unlink = driver.stat("/rust-hard-links").await.unwrap();
    driver
        .rename("/rust-hard-links", "/rust-hard-links.moved")
        .await
        .unwrap();
    let after_rename = driver.stat("/rust-hard-links.moved").await.unwrap();
    let handle_stat = handle.stat().await.unwrap();
    assert_eq!(handle_stat.ino, after_rename.ino);

    let observed = format!(
        "{},{},{},{},{},{},{}",
        before.iter().all(|stats| stats.ino == before[0].ino),
        before[0].nlink,
        before[1].nlink,
        before[2].nlink,
        after_unlink.nlink,
        after_rename.nlink,
        after_rename.ino == before[0].ino
    );
    assert_eq!(
        observed,
        windows_oracle("hard-links", &root.path().join("node-hard-links"))
    );
    driver.unlink("/rust-hard-links.second").await.unwrap();
    driver.unlink("/rust-hard-links.moved").await.unwrap();
    handle.close().await.unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn windows_read_only_hard_link_delete_and_handle_lifetime_match_node() {
    let root = TempDir::new();
    let driver = HostFs::new(root.path());
    let flags = OpenFlags {
        read: true,
        write: false,
        create: true,
        truncate: false,
        append: false,
        exclusive: false,
    };
    let handle = driver
        .open_flags("/rust-readonly-links", flags, 0o400)
        .await
        .unwrap();
    assert_eq!(
        handle.write(b"x", None).await.unwrap_err().code,
        ErrorCode::Ebadf
    );
    driver
        .link("/rust-readonly-links", "/rust-readonly-links.first")
        .await
        .unwrap();
    let before = [
        driver.stat("/rust-readonly-links").await.unwrap(),
        driver.stat("/rust-readonly-links.first").await.unwrap(),
    ];
    assert_eq!(before[0].mode & 0o777, 0o444);
    assert_eq!(before[1].mode & 0o777, 0o444);
    driver.unlink("/rust-readonly-links.first").await.unwrap();
    let after_alias = handle.stat().await.unwrap();
    assert_eq!(after_alias.mode & 0o777, 0o444);
    driver.unlink("/rust-readonly-links").await.unwrap();
    let after_final = handle.stat().await.unwrap();
    assert_eq!(after_final.mode & 0o777, 0o444);
    handle.close().await.unwrap();
    let missing = driver.stat("/rust-readonly-links").await.unwrap_err().code;
    assert_eq!(missing, ErrorCode::Enoent);

    let observed = format!(
        "EBADF,{},{},{},{}",
        before[0].nlink, before[1].nlink, after_alias.nlink, after_final.nlink
    );
    assert_eq!(
        format!("{observed},ENOENT"),
        windows_oracle(
            "readonly-hard-links",
            &root.path().join("node-readonly-links")
        )
    );
}

#[cfg(windows)]
#[tokio::test]
async fn windows_read_only_creation_matches_node_and_preserves_existing_data() {
    let root = TempDir::new();
    let driver = HostFs::new(root.path());
    let flags = OpenFlags {
        read: true,
        write: false,
        create: true,
        truncate: false,
        append: false,
        exclusive: false,
    };
    let handle = driver
        .open_flags("/rust-created", flags, 0o600)
        .await
        .unwrap();
    let error = handle.write(b"x", None).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Ebadf);
    let size = handle.stat().await.unwrap().size;
    handle.close().await.unwrap();
    assert_eq!(
        format!("{size},EBADF"),
        windows_oracle("read-create", &root.path().join("node-created"))
    );
    for (index, mode) in [("writable", 0o600), ("readonly", 0o400)] {
        let rust_path = format!("/rust-created-{index}");
        let node_path = root.path().join(format!("node-created-{index}"));
        let handle = driver.open_flags(&rust_path, flags, mode).await.unwrap();
        let error = handle.write(b"x", None).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::Ebadf);
        let stats = handle.stat().await.unwrap();
        handle.close().await.unwrap();
        let mode_arg = format!("{mode:o}");
        assert_eq!(
            format!("{},EBADF,{}", stats.size, stats.mode & 0o777),
            windows_oracle_with_args("read-create-mode", &node_path, &[&mode_arg])
        );
    }
    fs::write(root.path().join("rust-created"), b"preserved").unwrap();
    let handle = driver
        .open_flags("/rust-created", flags, 0o600)
        .await
        .unwrap();
    let mut bytes = [0; 9];
    assert_eq!(handle.read(&mut bytes, Some(0)).await.unwrap(), 9);
    assert_eq!(&bytes, b"preserved");
    handle.close().await.unwrap();
    let exclusive = OpenFlags {
        exclusive: true,
        ..flags
    };
    assert_eq!(
        driver
            .open_flags("/rust-created", exclusive, 0o600)
            .await
            .err()
            .unwrap()
            .code,
        ErrorCode::Eexist
    );
    let handle = driver
        .open_flags("/exclusive-new", exclusive, 0o600)
        .await
        .unwrap();
    handle.close().await.unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn windows_open_handle_survives_rename_unlink_and_positional_io() {
    let root = TempDir::new();
    let driver = HostFs::new(root.path());
    let handle = driver.open("/rust-handle", "w+", 0o600).await.unwrap();
    handle.write(b"a", None).await.unwrap();
    driver
        .rename("/rust-handle", "/rust-handle.moved")
        .await
        .unwrap();
    handle.write(b"b", None).await.unwrap();
    let mut bytes = [0_u8; 2];
    assert_eq!(handle.read(&mut bytes, Some(0)).await.unwrap(), 2);
    assert_eq!(&bytes, b"ab");
    driver.unlink("/rust-handle.moved").await.unwrap();
    assert_eq!(handle.stat().await.unwrap().size, 2);
    handle.close().await.unwrap();
    assert_eq!(
        driver.stat("/rust-handle.moved").await.unwrap_err().code,
        ErrorCode::Enoent
    );
    assert_eq!(
        windows_oracle("handle-lifecycle", &root.path().join("node-handle")),
        "ab,2,ENOENT"
    );
}

#[cfg(windows)]
#[tokio::test]
async fn windows_long_symlink_creation_uses_extended_path_fallback() {
    let sandbox = TempDir::new();
    let mut root = sandbox.path().to_path_buf();
    while root.to_string_lossy().len() < 258 {
        let remaining = 258 - root.to_string_lossy().len() - 1;
        let component = "x".repeat(remaining.clamp(1, 40));
        root.push(component);
        fs::create_dir(&root).expect("create long root component");
    }
    assert!(root.to_string_lossy().len() >= 258);
    let driver = HostFs::new(&root);
    driver
        .symlink("target", "/long-link")
        .await
        .expect("long symlink creation");
    driver
        .unlink("/long-link")
        .await
        .expect("long symlink cleanup");
}

#[cfg(windows)]
#[tokio::test]
async fn windows_symlink_hints_and_host_prefixes_stay_rooted() {
    use mount_rs_host::HostSymlinkType;
    let root = TempDir::new();
    let outside = TempDir::new();
    fs::write(outside.path().join("secret"), b"outside").unwrap();
    fs::create_dir(root.path().join("directory")).unwrap();
    fs::write(root.path().join("directory/data"), b"safe").unwrap();
    let driver = HostFs::new(root.path());
    let capabilities = driver.capabilities();
    assert!(capabilities.symlinks);
    assert!(capabilities.hardlinks);
    assert!(capabilities.handles);
    driver.symlink("directory", "/inferred").await.unwrap();
    driver
        .symlink("/directory", "/absolute-inferred")
        .await
        .unwrap();
    driver
        .symlink_with_type("later", "/dangling-dir", Some(HostSymlinkType::Directory))
        .await
        .unwrap();
    driver
        .symlink_with_type("later-file", "/dangling-file", Some(HostSymlinkType::File))
        .await
        .unwrap();
    assert_eq!(windows_oracle("symlinks", root.path()), "ok");
    use std::os::windows::fs::FileTypeExt;
    for name in ["inferred", "dangling-dir", "dangling-file"] {
        let actual = fs::symlink_metadata(root.path().join(name))
            .unwrap()
            .file_type();
        let expected = fs::symlink_metadata(root.path().join(format!("node-{name}")))
            .unwrap()
            .file_type();
        assert_eq!(actual.is_symlink_dir(), expected.is_symlink_dir());
        assert_eq!(actual.is_symlink_file(), expected.is_symlink_file());
        assert_eq!(
            driver.readlink(&format!("/{name}")).await.unwrap(),
            fs::read_link(root.path().join(format!("node-{name}")))
                .unwrap()
                .to_string_lossy()
        );
    }
    assert!(
        fs::symlink_metadata(root.path().join("inferred"))
            .unwrap()
            .file_type()
            .is_symlink_dir()
    );
    assert!(
        fs::symlink_metadata(root.path().join("absolute-inferred"))
            .unwrap()
            .file_type()
            .is_symlink_dir()
    );
    assert!(
        fs::symlink_metadata(root.path().join("dangling-dir"))
            .unwrap()
            .file_type()
            .is_symlink_dir()
    );
    assert!(
        fs::symlink_metadata(root.path().join("dangling-file"))
            .unwrap()
            .file_type()
            .is_symlink_file()
    );
    assert_eq!(driver.stat("/inferred/data").await.unwrap().size, 4);
    assert_eq!(
        driver.stat("/absolute-inferred/data").await.unwrap().size,
        4
    );
    driver.lutimes("/dangling-dir", 1_000, 2_000).await.unwrap();
    assert_eq!(driver.lstat("/dangling-dir").await.unwrap().mtime_ms, 2_000);
    // The raw host link exists, but the rooted driver cannot follow it outside.
    std::os::windows::fs::symlink_file(outside.path().join("secret"), root.path().join("escape"))
        .unwrap();
    assert_eq!(
        driver.stat("/escape").await.unwrap_err().code,
        ErrorCode::Enoent
    );
    for path in [
        r"/..\secret",
        r"/C:\secret",
        "/file:stream",
        "/NUL",
        "/CON.txt",
        "/dir. /file",
    ] {
        assert_eq!(
            driver.stat(path).await.unwrap_err().code,
            ErrorCode::Einval,
            "{path}"
        );
    }
}
