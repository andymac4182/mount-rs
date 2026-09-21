use async_trait::async_trait;
use mount_rs_core::{
    Capabilities, DirEntry, FileHandle, FsDriver, FsError, MemoryFs, Result, S_IFREG, Stats,
};
use mount_rs_fuse::{
    RequestHeader,
    constants::{
        FUSE_ACCESS, FUSE_BATCH_FORGET, FUSE_COPY_FILE_RANGE, FUSE_FALLOCATE, FUSE_INTERRUPT,
        FUSE_IOCTL, FUSE_LINK, FUSE_LOOKUP, FUSE_LSEEK, FUSE_MKDIR, FUSE_MKNOD, FUSE_POLL,
        FUSE_READLINK, FUSE_RENAME, FUSE_RENAME2, FUSE_RMDIR, FUSE_STATFS, FUSE_SYMLINK,
        FUSE_UNLINK,
    },
    protocol::{FuseReplyBody, ProtocolContext, decode_reply_body},
    session::FuseSession,
};
use std::sync::Arc;

fn frame(opcode: u32, nodeid: u64, body: &[u8]) -> Vec<u8> {
    frame_with_credentials(opcode, nodeid, body, 0, 0)
}
fn frame_with_credentials(opcode: u32, nodeid: u64, body: &[u8], uid: u32, gid: u32) -> Vec<u8> {
    let mut bytes = RequestHeader {
        len: (40 + body.len()) as u32,
        opcode,
        unique: 42,
        nodeid,
        uid,
        gid,
        pid: 0,
        total_extlen: 0,
    }
    .encode()
    .to_vec();
    bytes.extend(body);
    bytes
}
fn number(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}
fn errno(bytes: &[u8]) -> i32 {
    i32::from_le_bytes(bytes[4..8].try_into().unwrap())
}
fn name_body(name: &str) -> Vec<u8> {
    let mut body = name.as_bytes().to_vec();
    body.push(0);
    body
}
fn mknod_body(mode: u32, dev: u32, name: &str) -> Vec<u8> {
    let mut body = vec![0; 16];
    body[..4].copy_from_slice(&mode.to_le_bytes());
    body[4..8].copy_from_slice(&dev.to_le_bytes());
    body.extend(name_body(name));
    body
}
fn mkdir_body(mode: u32, name: &str) -> Vec<u8> {
    let mut body = vec![0; 8];
    body[..4].copy_from_slice(&mode.to_le_bytes());
    body.extend(name_body(name));
    body
}
fn symlink_body(name: &str, target: &str) -> Vec<u8> {
    let mut body = name_body(name);
    body.extend(name_body(target));
    body
}
fn rename_body(newdir: u64, old: &str, new: &str) -> Vec<u8> {
    let mut body = newdir.to_le_bytes().to_vec();
    body.extend(name_body(old));
    body.extend(name_body(new));
    body
}
fn link_body(oldnodeid: u64, name: &str) -> Vec<u8> {
    let mut body = oldnodeid.to_le_bytes().to_vec();
    body.extend(name_body(name));
    body
}
fn access_body(mask: u32) -> Vec<u8> {
    let mut body = mask.to_le_bytes().to_vec();
    body.extend([0; 4]);
    body
}
async fn negotiate(session: &mut FuseSession) {
    if session.negotiated.is_some() {
        return;
    }
    let init: Vec<u8> = [7u32, 41, 65536, u32::MAX, u32::MAX]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
    let reply = session.handle(&frame(26, 0, &init)).await.unwrap().unwrap();
    assert_eq!(&reply[4..8], &[0; 4]);
}
async fn request(session: &mut FuseSession, op: u32, node: u64, body: &[u8]) -> Vec<u8> {
    if session.negotiated.is_none() && op != 26 {
        negotiate(session).await;
    }
    let reply = session
        .handle(&frame(op, node, body))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(number(&reply, 8), 42);
    assert_eq!(&reply[4..8], &[0; 4]);
    assert_eq!(
        u32::from_le_bytes(reply[..4].try_into().unwrap()) as usize,
        reply.len()
    );
    reply[16..].to_vec()
}
async fn failed_request(session: &mut FuseSession, op: u32, node: u64, body: &[u8]) -> i32 {
    negotiate(session).await;
    let reply = session
        .handle(&frame(op, node, body))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(number(&reply, 8), 42);
    assert_eq!(reply.len(), 16);
    errno(&reply)
}

struct NoMknodDriver {
    inner: Arc<MemoryFs>,
}

#[async_trait]
impl FsDriver for NoMknodDriver {
    fn capabilities(&self) -> Capabilities {
        let mut capabilities = self.inner.capabilities();
        capabilities.mknod = false;
        capabilities
    }

    async fn stat(&self, path: &str) -> Result<Stats> {
        self.inner.stat(path).await
    }

    async fn readdir(&self, path: &str) -> Result<Vec<DirEntry>> {
        self.inner.readdir(path).await
    }

    async fn open(&self, path: &str, flags: &str, mode: u32) -> Result<Arc<dyn FileHandle>> {
        self.inner.open(path, flags, mode).await
    }

    async fn mknod(&self, _path: &str, _mode: u32, _dev: u64) -> Result<()> {
        Err(FsError::enosys("mknod"))
    }
}

#[tokio::test]
async fn lifecycle_requires_handshake_and_rejects_requests_after_destroy() {
    let mut session = FuseSession::new(Arc::new(MemoryFs::empty()));
    let reply = session
        .handle(&frame(3, 1, &[0; 16]))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(i32::from_le_bytes(reply[4..8].try_into().unwrap()), -5);
    request(&mut session, 3, 1, &[0; 16]).await;
    request(&mut session, 38, 0, &[]).await;
    let reply = session
        .handle(&frame(3, 1, &[0; 16]))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(i32::from_le_bytes(reply[4..8].try_into().unwrap()), -19);
    session.destroy().await;
}

#[tokio::test]
async fn simple_namespace_operations_roundtrip_and_cleanup_inode_paths() {
    let fs = Arc::new(MemoryFs::empty());
    let mut session = FuseSession::new(fs.clone());

    let directory = number(
        &request(&mut session, FUSE_MKDIR, 1, &mkdir_body(0o750, "dir")).await,
        0,
    );
    let target = number(
        &request(
            &mut session,
            FUSE_MKNOD,
            1,
            &mknod_body(S_IFREG | 0o640, 0x1234, "target"),
        )
        .await,
        0,
    );
    let symlink = number(
        &request(
            &mut session,
            FUSE_SYMLINK,
            directory,
            &symlink_body("alias", "../target"),
        )
        .await,
        0,
    );
    assert_eq!(
        request(&mut session, FUSE_READLINK, symlink, &[]).await,
        b"../target"
    );

    let hardlink = request(
        &mut session,
        FUSE_LINK,
        1,
        &link_body(target, "target-hard"),
    )
    .await;
    assert_eq!(number(&hardlink, 0), target);
    assert_eq!(fs.stat("/target").await.unwrap().nlink, 2);

    request(
        &mut session,
        FUSE_RENAME,
        1,
        &rename_body(1, "dir", "moved"),
    )
    .await;
    assert_eq!(session.inodes.require_path(directory).unwrap(), "/moved");
    assert_eq!(
        session.inodes.require_path(symlink).unwrap(),
        "/moved/alias"
    );
    assert!(fs.lstat("/moved/alias").await.is_ok());

    assert_eq!(
        failed_request(&mut session, FUSE_RMDIR, 1, &name_body("moved")).await,
        -39
    );
    assert!(session.inodes.at("/moved").is_some());

    request(&mut session, FUSE_UNLINK, 1, &name_body("target-hard")).await;
    assert!(session.inodes.at("/target-hard").is_none());
    assert_eq!(fs.stat("/target").await.unwrap().nlink, 1);

    request(&mut session, FUSE_UNLINK, directory, &name_body("alias")).await;
    assert!(session.inodes.at("/moved/alias").is_none());
    assert!(session.inodes.get(symlink).unwrap().paths.is_empty());

    request(&mut session, FUSE_RMDIR, 1, &name_body("moved")).await;
    assert!(fs.lstat("/moved").await.is_err());
    assert!(session.inodes.at("/moved").is_none());
    assert!(session.inodes.get(directory).unwrap().paths.is_empty());
}

#[tokio::test]
async fn simple_namespace_errors_preserve_backend_and_inode_state() {
    let fs = Arc::new(MemoryFs::empty());
    let mut session = FuseSession::new(fs.clone());
    let directory = number(
        &request(&mut session, FUSE_MKDIR, 1, &mkdir_body(0o755, "dir")).await,
        0,
    );
    let file = number(
        &request(
            &mut session,
            FUSE_MKNOD,
            1,
            &mknod_body(S_IFREG | 0o644, 0, "file"),
        )
        .await,
        0,
    );
    request(
        &mut session,
        FUSE_MKNOD,
        directory,
        &mknod_body(S_IFREG | 0o600, 0, "child"),
    )
    .await;

    assert_eq!(
        failed_request(
            &mut session,
            FUSE_SYMLINK,
            1,
            &symlink_body("file", "target")
        )
        .await,
        -17
    );
    assert_eq!(
        failed_request(
            &mut session,
            FUSE_MKNOD,
            1,
            &mknod_body(S_IFREG | 0o600, 0, "file"),
        )
        .await,
        -17
    );
    assert_eq!(
        failed_request(&mut session, FUSE_MKDIR, 1, &mkdir_body(0o755, "dir")).await,
        -17
    );
    assert_eq!(
        failed_request(&mut session, FUSE_UNLINK, 1, &name_body("dir")).await,
        -21
    );
    assert_eq!(
        failed_request(&mut session, FUSE_RMDIR, 1, &name_body("file")).await,
        -20
    );
    assert_eq!(
        failed_request(&mut session, FUSE_RMDIR, 1, &name_body("dir")).await,
        -39
    );
    assert_eq!(
        failed_request(&mut session, FUSE_UNLINK, 1, &name_body("missing")).await,
        -2
    );
    assert_eq!(
        failed_request(
            &mut session,
            FUSE_RENAME,
            1,
            &rename_body(1, "missing", "new")
        )
        .await,
        -2
    );
    assert_eq!(
        failed_request(&mut session, FUSE_LINK, 1, &link_body(u64::MAX, "alias")).await,
        -116
    );

    assert!(fs.lstat("/dir").await.is_ok());
    assert!(fs.lstat("/dir/child").await.is_ok());
    assert!(fs.lstat("/file").await.is_ok());
    assert!(fs.lstat("/new").await.is_err());
    assert!(session.inodes.at("/dir").is_some());
    assert!(session.inodes.at("/dir/child").is_some());
    assert_eq!(session.inodes.require_path(file).unwrap(), "/file");
}

#[tokio::test]
async fn mknod_falls_back_only_for_regular_files_without_driver_support() {
    let fs = Arc::new(MemoryFs::empty());
    let driver = Arc::new(NoMknodDriver { inner: fs.clone() });
    let mut session = FuseSession::new(driver);

    let regular = request(
        &mut session,
        FUSE_MKNOD,
        1,
        &mknod_body(S_IFREG | 0o600, 0, "regular"),
    )
    .await;
    assert_eq!(
        number(&regular, 0),
        session.inodes.at("/regular").unwrap().nodeid
    );
    assert_eq!(fs.lstat("/regular").await.unwrap().mode & 0o7777, 0o600);

    assert_eq!(
        failed_request(
            &mut session,
            FUSE_MKNOD,
            1,
            &mknod_body(0o010_600, 0x22, "fifo"),
        )
        .await,
        -38
    );
    assert!(fs.lstat("/fifo").await.is_err());
    assert!(session.inodes.at("/fifo").is_none());
}

#[tokio::test]
async fn namespace_name_limit_matches_statfs_and_rejects_long_frames() {
    let fs = Arc::new(MemoryFs::empty());
    let mut session = FuseSession::new(fs.clone());
    let maximum = "m".repeat(255);
    request(&mut session, FUSE_MKDIR, 1, &mkdir_body(0o755, &maximum)).await;
    assert!(fs.lstat(&format!("/{maximum}")).await.is_ok());

    let file = number(
        &request(
            &mut session,
            FUSE_MKNOD,
            1,
            &mknod_body(S_IFREG | 0o600, 0, "file"),
        )
        .await,
        0,
    );
    let too_long = "n".repeat(256);
    let cases = [
        (FUSE_LOOKUP, 1, name_body(&too_long)),
        (FUSE_SYMLINK, 1, symlink_body(&too_long, "target")),
        (FUSE_MKNOD, 1, mknod_body(S_IFREG | 0o600, 0, &too_long)),
        (FUSE_MKDIR, 1, mkdir_body(0o755, &too_long)),
        (FUSE_UNLINK, 1, name_body(&too_long)),
        (FUSE_RMDIR, 1, name_body(&too_long)),
        (FUSE_RENAME, 1, rename_body(1, &too_long, "new")),
        (FUSE_LINK, 1, link_body(file, &too_long)),
    ];
    for (opcode, nodeid, body) in cases {
        assert_eq!(
            failed_request(&mut session, opcode, nodeid, &body).await,
            -36,
            "opcode {opcode}"
        );
    }
    assert!(fs.lstat("/new").await.is_err());
    assert!(session.inodes.at("/new").is_none());
    assert!(session.inodes.at(&format!("/{too_long}")).is_none());
}

#[tokio::test]
async fn access_dispatch_checks_credentials_and_fixed_wire_mask() {
    let fs = Arc::new(MemoryFs::empty());
    let file = fs.open("/owned", "w", 0o640).await.unwrap();
    file.close().await.unwrap();
    fs.chown("/owned", 1000, 2000).await.unwrap();
    fs.chmod("/owned", 0o640).await.unwrap();
    fs.symlink("owned", "/alias").await.unwrap();

    let mut session = FuseSession::new(fs.clone());
    let inode = number(&request(&mut session, 1, 1, b"owned\0").await, 0);
    let access = |mask: u32| {
        let mut body = mask.to_le_bytes().to_vec();
        body.extend([0; 4]);
        body
    };
    let success = session
        .handle(&frame_with_credentials(34, inode, &access(6), 1000, 2000))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&success[4..8], &[0; 4]);
    assert_eq!(success.len(), 16);

    let group_read = session
        .handle(&frame_with_credentials(34, inode, &access(4), 3000, 2000))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&group_read[4..8], &[0; 4]);

    let other_read = session
        .handle(&frame_with_credentials(34, inode, &access(4), 3000, 4000))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        i32::from_le_bytes(other_read[4..8].try_into().unwrap()),
        -13
    );

    let invalid_mask = session
        .handle(&frame_with_credentials(34, inode, &access(8), 1000, 2000))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        i32::from_le_bytes(invalid_mask[4..8].try_into().unwrap()),
        -22
    );

    let alias = number(&request(&mut session, FUSE_LOOKUP, 1, b"alias\0").await, 0);
    let through_symlink = session
        .handle(&frame_with_credentials(
            FUSE_ACCESS,
            alias,
            &access_body(6),
            1000,
            2000,
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&through_symlink[4..8], &[0; 4]);

    fs.chmod("/owned", 0o600).await.unwrap();
    let root_read = session
        .handle(&frame_with_credentials(
            FUSE_ACCESS,
            inode,
            &access_body(4),
            0,
            0,
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&root_read[4..8], &[0; 4]);
    let root_execute = session
        .handle(&frame_with_credentials(
            FUSE_ACCESS,
            inode,
            &access_body(1),
            0,
            0,
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(errno(&root_execute), -13);
}

#[tokio::test]
async fn batch_forget_is_validated_before_no_reply_inode_release() {
    let fs = Arc::new(MemoryFs::empty());
    for path in ["/one", "/two"] {
        let file = fs.open(path, "w", 0o644).await.unwrap();
        file.close().await.unwrap();
    }
    let mut session = FuseSession::new(fs);
    let one = number(&request(&mut session, 1, 1, b"one\0").await, 0);
    let two = number(&request(&mut session, 1, 1, b"two\0").await, 0);

    let mut malformed = 1u32.to_le_bytes().to_vec();
    malformed.extend([0; 4]);
    let rejected = session
        .handle(&frame(FUSE_BATCH_FORGET, 0, &malformed))
        .await;
    assert!(rejected.is_err());
    assert_eq!(session.inodes.get(one).unwrap().nlookup, 1);
    assert_eq!(session.inodes.get(two).unwrap().nlookup, 1);

    let mut batch = 2u32.to_le_bytes().to_vec();
    batch.extend([0; 4]);
    for inode in [one, two] {
        batch.extend(inode.to_le_bytes());
        batch.extend(1u64.to_le_bytes());
    }
    assert!(
        session
            .handle(&frame(FUSE_BATCH_FORGET, 0, &batch))
            .await
            .unwrap()
            .is_none()
    );
    assert!(session.inodes.get(one).is_none());
    assert!(session.inodes.get(two).is_none());
}

#[tokio::test]
async fn interrupt_rejects_bad_wire_and_classifies_unknown_target_without_teardown() {
    let fs = Arc::new(MemoryFs::empty());
    let file = fs.open("/still-alive", "w", 0o644).await.unwrap();
    file.close().await.unwrap();
    let mut session = FuseSession::new(fs);
    let entry = request(&mut session, 1, 1, b"still-alive\0").await;
    assert_eq!(entry.len(), 128);

    let malformed = session
        .handle(&frame(FUSE_INTERRUPT, 0, &[0; 7]))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(i32::from_le_bytes(malformed[4..8].try_into().unwrap()), -22);

    let target = 0x6162_6364_6566_6768u64.to_le_bytes();
    let interrupt = session
        .handle(&frame(FUSE_INTERRUPT, 0, &target))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(i32::from_le_bytes(interrupt[4..8].try_into().unwrap()), -11);
    assert_eq!(number(&interrupt, 8), 42);

    assert_eq!(
        number(&request(&mut session, 1, 1, b"still-alive\0").await, 0),
        number(&entry, 0)
    );
}

#[tokio::test]
async fn poll_rejects_bad_wire_and_keeps_valid_poll_at_explicit_enosys_boundary() {
    let mut session = FuseSession::new(Arc::new(MemoryFs::empty()));
    let init: Vec<u8> = [7u32, 41, 65536, u32::MAX, u32::MAX]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
    session.handle(&frame(26, 0, &init)).await.unwrap().unwrap();

    let malformed = session
        .handle(&frame(FUSE_POLL, 1, &[0; 23]))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(i32::from_le_bytes(malformed[4..8].try_into().unwrap()), -22);

    let mut trailing = vec![0; 24];
    trailing.push(0);
    let trailing_reply = session
        .handle(&frame(FUSE_POLL, 1, &trailing))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        i32::from_le_bytes(trailing_reply[4..8].try_into().unwrap()),
        -22
    );

    let valid = session
        .handle(&frame(FUSE_POLL, 1, &[0; 24]))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(i32::from_le_bytes(valid[4..8].try_into().unwrap()), -38);
    assert_eq!(number(&valid, 8), 42);

    // The unsupported POLL path is observationally safe: it does not tear
    // down the negotiated session or mutate the inode table.
    let entry = session
        .handle(&frame(1, 1, b"missing\0"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(i32::from_le_bytes(entry[4..8].try_into().unwrap()), -2);
}

#[tokio::test]
async fn advanced_operations_fail_closed_and_remain_explicitly_unsupported() {
    let fs = Arc::new(MemoryFs::empty());
    let file = fs.open("/old", "w", 0o644).await.unwrap();
    file.close().await.unwrap();
    let mut session = FuseSession::new(fs.clone());
    let init: Vec<u8> = [7u32, 41, 65536, u32::MAX, u32::MAX]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
    session.handle(&frame(26, 0, &init)).await.unwrap().unwrap();

    let errno = |reply: &[u8]| i32::from_le_bytes(reply[4..8].try_into().unwrap());

    for (opcode, size) in [
        (FUSE_FALLOCATE, 32usize),
        (FUSE_LSEEK, 24usize),
        (FUSE_COPY_FILE_RANGE, 56usize),
    ] {
        let short = session
            .handle(&frame(opcode, 1, &vec![0; size - 1]))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(errno(&short), -22, "short {opcode} frame");

        let mut trailing_body = vec![0; size];
        trailing_body.push(0);
        let trailing = session
            .handle(&frame(opcode, 1, &trailing_body))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(errno(&trailing), -22, "trailing {opcode} frame");
    }

    // RENAME2 has a fixed header followed by exactly two NUL-terminated
    // names. The missing second name is rejected before any backend call.
    let mut malformed_rename2 = vec![0; 16];
    malformed_rename2.extend(b"old\0");
    let malformed = session
        .handle(&frame(FUSE_RENAME2, 1, &malformed_rename2))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(errno(&malformed), -22);

    let mut rename2 = vec![0; 16];
    rename2[..8].copy_from_slice(&1u64.to_le_bytes());
    rename2.extend(b"old\0new\0");
    for (opcode, body) in [
        (FUSE_FALLOCATE, vec![0; 32]),
        (FUSE_RENAME2, rename2),
        (FUSE_LSEEK, vec![0; 24]),
        (FUSE_COPY_FILE_RANGE, vec![0; 56]),
    ] {
        let reply = session
            .handle(&frame(opcode, 1, &body))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(errno(&reply), -38, "valid {opcode} frame");
    }

    // ENOSYS is a boundary, not a best-effort rename/copy/seek. The
    // filesystem contents and negotiated session remain unchanged.
    assert!(fs.lstat("/old").await.is_ok());
    assert!(fs.lstat("/new").await.is_err());
    let lookup = session
        .handle(&frame(1, 1, b"old\0"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(errno(&lookup), 0);
}

fn io_body(handle: u64, offset: u64, size: u32) -> Vec<u8> {
    let mut body = vec![0; 40];
    body[..8].copy_from_slice(&handle.to_le_bytes());
    body[8..16].copy_from_slice(&offset.to_le_bytes());
    body[16..20].copy_from_slice(&size.to_le_bytes());
    body
}
fn release_body(handle: u64) -> Vec<u8> {
    let mut body = vec![0; 24];
    body[..8].copy_from_slice(&handle.to_le_bytes());
    body
}

fn ioctl_body(declared_input_size: u32, input: &[u8]) -> Vec<u8> {
    let mut body = vec![0; 32];
    body[..8].copy_from_slice(&u64::MAX.to_le_bytes());
    body[12..16].copy_from_slice(&0x1234u32.to_le_bytes());
    body[16..24].copy_from_slice(&0x5678u64.to_le_bytes());
    body[24..28].copy_from_slice(&declared_input_size.to_le_bytes());
    body[28..32].copy_from_slice(&8u32.to_le_bytes());
    body.extend(input);
    body
}

#[tokio::test]
async fn ioctl_is_strictly_framed_and_explicitly_unsupported_without_mutation() {
    let fs = Arc::new(MemoryFs::empty());
    let file = fs.open("/stable", "w", 0o644).await.unwrap();
    file.close().await.unwrap();
    let before = fs.lstat("/stable").await.unwrap();
    let mut session = FuseSession::new(fs.clone());
    let init: Vec<u8> = [7u32, 41, 65536, u32::MAX, u32::MAX]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
    session.handle(&frame(26, 0, &init)).await.unwrap().unwrap();

    let errno = |reply: &[u8]| i32::from_le_bytes(reply[4..8].try_into().unwrap());
    for body in [vec![0; 31], ioctl_body(3, b"ab"), ioctl_body(0, b"\xA5")] {
        let reply = session
            .handle(&frame(FUSE_IOCTL, 0, &body))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(errno(&reply), -22);
    }

    for body in [ioctl_body(0, b""), ioctl_body(3, b"abc")] {
        let reply = session
            .handle(&frame(FUSE_IOCTL, 0, &body))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(errno(&reply), -38);
    }

    let after = fs.lstat("/stable").await.unwrap();
    assert_eq!(after.mode, before.mode);
    assert_eq!(after.size, before.size);
    assert!(session.negotiated.is_some());
    let lookup = session
        .handle(&frame(1, 1, b"stable\0"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(errno(&lookup), 0);
}

#[tokio::test]
async fn directory_and_symlink_mutations_through_frames() {
    let fs = Arc::new(MemoryFs::empty());
    let mut session = FuseSession::new(fs.clone());
    let mut mkdir = vec![0; 8];
    mkdir[..4].copy_from_slice(&0o750u32.to_le_bytes());
    mkdir.extend(b"old\0");
    let directory = number(&request(&mut session, 9, 1, &mkdir).await, 0);
    let link = number(
        &request(&mut session, 6, directory, b"link\0../target\0").await,
        0,
    );
    assert_eq!(request(&mut session, 5, link, &[]).await, b"../target");
    let mut rename = 1u64.to_le_bytes().to_vec();
    rename.extend(b"old\0new\0");
    request(&mut session, 12, 1, &rename).await;
    assert_eq!(session.inodes.require_path(directory).unwrap(), "/new");
    assert_eq!(session.inodes.require_path(link).unwrap(), "/new/link");
    assert_eq!(fs.lstat("/new").await.unwrap().mode & 0o7777, 0o750);
    let reply = session
        .handle(&frame(11, 1, b"new\0"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(i32::from_le_bytes(reply[4..8].try_into().unwrap()), -39);
    request(&mut session, 10, directory, b"link\0").await;
    request(&mut session, 11, 1, b"new\0").await;
    assert!(fs.lstat("/new").await.is_err());
    assert!(session.inodes.require_path(link).is_err());
}

#[tokio::test]
async fn create_sync_and_statfs_roundtrip() {
    let fs = Arc::new(MemoryFs::empty());
    let mut session = FuseSession::new(fs.clone());
    let mut create = vec![0; 16];
    create[..4].copy_from_slice(&0x42u32.to_le_bytes());
    create[4..8].copy_from_slice(&0o640u32.to_le_bytes());
    create.extend(b"created\0");
    let reply = request(&mut session, 35, 1, &create).await;
    assert_eq!(reply.len(), 144);
    let inode = number(&reply, 0);
    let handle = number(&reply, 128);
    let mut write = io_body(handle, 0, 4);
    write.extend(b"data");
    request(&mut session, 16, inode, &write).await;
    let mut sync = vec![0; 16];
    sync[..8].copy_from_slice(&handle.to_le_bytes());
    request(&mut session, 20, inode, &sync).await;
    sync[8] = 1;
    request(&mut session, 20, inode, &sync).await;
    let stats = request(&mut session, 17, inode, &[]).await;
    assert_eq!(stats.len(), 80);
    assert_eq!(number(&stats, 24), fs.statfs("/").await.unwrap().files);
    let decoded = decode_reply_body(
        FUSE_STATFS,
        &stats,
        Some(ProtocolContext {
            minor: 41,
            setxattr_ext: false,
        }),
    )
    .unwrap();
    assert!(
        matches!(decoded, FuseReplyBody::Statfs(value) if value.namelen == 255 && value.bsize == 4096)
    );
    assert_eq!(fs.stat("/created").await.unwrap().mode & 0o7777, 0o640);
    request(&mut session, 18, inode, &release_body(handle)).await;
}

#[tokio::test]
async fn readlink_uses_typed_wire_encoding_and_rejects_embedded_nul() {
    let fs = Arc::new(MemoryFs::empty());
    fs.symlink("target/λ", "/unicode").await.unwrap();
    // MemoryFs intentionally permits this driver-level value so the transport
    // must enforce the FUSE wire invariant, just like the pinned oracle.
    fs.symlink("target\0tail", "/nul").await.unwrap();

    let mut session = FuseSession::new(fs);
    let unicode = number(&request(&mut session, 1, 1, b"unicode\0").await, 0);
    assert_eq!(
        request(&mut session, 5, unicode, &[]).await,
        "target/λ".as_bytes()
    );

    let nul = number(&request(&mut session, 1, 1, b"nul\0").await, 0);
    let reply = session.handle(&frame(5, nul, &[])).await.unwrap().unwrap();
    assert_eq!(i32::from_le_bytes(reply[4..8].try_into().unwrap()), -5);
    assert_eq!(reply.len(), 16);
}

#[tokio::test]
async fn directory_pages_use_stable_cookies_and_refresh_on_rewind() {
    let fs = Arc::new(MemoryFs::empty());
    let file = fs.open("/a", "w", 0o644).await.unwrap();
    file.close().await.unwrap();
    let mut session = FuseSession::new(fs.clone());
    let opened = request(&mut session, 27, 1, &[0; 8]).await;
    let handle = number(&opened, 0);
    assert!(
        request(&mut session, 28, 1, &io_body(handle, 0, 31))
            .await
            .is_empty()
    );
    let first = request(&mut session, 28, 1, &io_body(handle, 0, 32)).await;
    assert_eq!(&first[24..25], b".");
    assert_eq!(number(&first, 8), 1);
    let second = request(&mut session, 28, 1, &io_body(handle, 1, 32)).await;
    assert_eq!(&second[24..26], b"..");
    let third = request(&mut session, 28, 1, &io_body(handle, 2, 32)).await;
    assert_eq!(&third[24..25], b"a");
    let file = fs.open("/b", "w", 0o644).await.unwrap();
    file.close().await.unwrap();
    assert!(
        request(&mut session, 28, 1, &io_body(handle, 3, 32))
            .await
            .is_empty()
    );
    let refreshed = request(&mut session, 28, 1, &io_body(handle, 0, 4096)).await;
    assert_eq!(refreshed.len(), 128);
    assert_eq!(&refreshed[120..121], b"b");
    assert!(session.inodes.at("/a").is_none());
    assert!(
        request(&mut session, 44, 1, &io_body(handle, 2, 159))
            .await
            .is_empty()
    );
    assert!(session.inodes.at("/a").is_none());
    let plus = request(&mut session, 44, 1, &io_body(handle, 2, 160)).await;
    assert_eq!(plus.len(), 160);
    let id = number(&plus, 0);
    assert_eq!(session.inodes.get(id).unwrap().nlookup, 1);
    session
        .handle(&frame(2, id, &1u64.to_le_bytes()))
        .await
        .unwrap();
    assert!(session.inodes.get(id).is_none());
    request(&mut session, 29, 1, &release_body(handle)).await;
    let reply = session
        .handle(&frame(28, 1, &io_body(handle, 0, 32)))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(i32::from_le_bytes(reply[4..8].try_into().unwrap()), -9);
}

#[tokio::test]
async fn file_io_and_orphan_handles_through_fuse_frames() {
    let fs = Arc::new(MemoryFs::empty());
    let initial = fs.open("/file", "w", 0o644).await.unwrap();
    initial.write(b"abcdef", None).await.unwrap();
    initial.close().await.unwrap();
    let mut session = FuseSession::new(fs.clone());
    let entry = request(&mut session, 1, 1, b"file\0").await;
    assert_eq!(entry.len(), 128);
    let inode = number(&entry, 0);
    let opened = request(&mut session, 14, inode, &[2, 0, 0, 0, 0, 0, 0, 0]).await;
    let handle = number(&opened, 0);
    let mut write = io_body(handle, 1, 2);
    write.extend(b"XY");
    assert_eq!(
        request(&mut session, 16, inode, &write).await,
        [2, 0, 0, 0, 0, 0, 0, 0]
    );
    assert_eq!(
        request(&mut session, 15, inode, &io_body(handle, 0, 32)).await,
        b"aXYdef"
    );
    fs.unlink("/file").await.unwrap();
    session.inodes.unbind("/file");
    let mut getattr = vec![0; 16];
    getattr[0] = 1;
    getattr[8..].copy_from_slice(&handle.to_le_bytes());
    let attributes = request(&mut session, 3, inode, &getattr).await;
    assert_eq!(attributes.len(), 104);
    assert_eq!(number(&attributes, 24), 6);
    request(&mut session, 18, inode, &release_body(handle)).await;
    let reply = session
        .handle(&frame(15, inode, &io_body(handle, 0, 1)))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(i32::from_le_bytes(reply[4..8].try_into().unwrap()), -9);
    assert!(
        session
            .handle(&frame(2, inode, &1u64.to_le_bytes()))
            .await
            .unwrap()
            .is_none()
    );
}
