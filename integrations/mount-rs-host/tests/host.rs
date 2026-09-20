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
    assert_eq!(driver.readlink("/link").await.expect("readlink"), "/inside");
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
            .is_some_and(|path| path.ends_with("/taken"))
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
    assert_eq!(
        driver.stat("/dir/original").await.expect("stat chmod").mode & 0o777,
        0o600
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
