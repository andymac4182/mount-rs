use mount_rs_core::{FsDriver, MemoryFs};
use mount_rs_fuse::{
    RequestHeader,
    constants::{
        FUSE_BATCH_FORGET, FUSE_COPY_FILE_RANGE, FUSE_FALLOCATE, FUSE_INTERRUPT, FUSE_LSEEK,
        FUSE_POLL, FUSE_RENAME2, FUSE_STATFS,
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
async fn request(session: &mut FuseSession, op: u32, node: u64, body: &[u8]) -> Vec<u8> {
    if session.negotiated.is_none() && op != 26 {
        let init: Vec<u8> = [7u32, 41, 65536, u32::MAX, u32::MAX]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect();
        let reply = session.handle(&frame(26, 0, &init)).await.unwrap().unwrap();
        assert_eq!(&reply[4..8], &[0; 4]);
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
async fn access_dispatch_checks_credentials_and_fixed_wire_mask() {
    let fs = Arc::new(MemoryFs::empty());
    let file = fs.open("/owned", "w", 0o640).await.unwrap();
    file.close().await.unwrap();
    fs.chown("/owned", 1000, 2000).await.unwrap();
    fs.chmod("/owned", 0o640).await.unwrap();

    let mut session = FuseSession::new(fs);
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
