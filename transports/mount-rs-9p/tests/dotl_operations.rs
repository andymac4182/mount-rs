use mount_rs_9p::*;
use mount_rs_core::types::S_IFREG;
use mount_rs_memfs::MemoryFs;

fn frame<F>(type_: u8, tag: u16, write: F) -> Vec<u8>
where
    F: FnOnce(&mut P9Writer) -> Result<(), P9Error>,
{
    encode_message(type_, tag, 256, write).expect("test message encodes")
}

async fn call(session: &P9Session, request: Vec<u8>) -> Vec<u8> {
    session
        .handle_call(&request)
        .await
        .expect("complete request gets a response")
}

fn response_type(response: &[u8]) -> u8 {
    decode_message(response)
        .expect("response frame decodes")
        .0
        .type_
}

async fn negotiate_and_attach(session: &P9Session) {
    let version = call(
        session,
        frame(P9_TVERSION, P9_NOTAG, |writer| {
            write_tversion(
                writer,
                &Tversion {
                    msize: 8192,
                    version: P9_VERSION_DOTL.to_owned(),
                },
            )
        }),
    )
    .await;
    assert_eq!(response_type(&version), P9_RVERSION);

    let attach = call(
        session,
        frame(P9_TATTACH, 1, |writer| {
            write_tattach(
                writer,
                &Tattach {
                    fid: 1,
                    afid: P9_NOFID,
                    uname: "dotl-test".to_owned(),
                    aname: String::new(),
                    n_uname: u32::MAX,
                },
            )
        }),
    )
    .await;
    assert_eq!(response_type(&attach), P9_RATTACH);
}

async fn clone_root(session: &P9Session, tag: u16, fid: u32) {
    let response = call(
        session,
        frame(P9_TWALK, tag, |writer| {
            write_twalk(
                writer,
                &Twalk {
                    fid: 1,
                    newfid: fid,
                    wnames: Vec::new(),
                },
            )
        }),
    )
    .await;
    assert_eq!(response_type(&response), P9_RWALK);
    let (_, walk) = decode_message_as(&response, read_rwalk).expect("Rwalk decodes");
    assert!(walk.wqids.is_empty());
}

#[tokio::test]
async fn exercises_dotl_mutation_attribute_lock_and_link_operations() {
    // This is a rootless wire test: it exercises the protocol/session contract
    // on both macOS and Linux, not a native kernel mount.
    let session = P9Session::new(MemoryFs::empty());
    negotiate_and_attach(&session).await;

    let mkdir = call(
        &session,
        frame(P9_TMKDIR, 2, |writer| {
            write_tmkdir(
                writer,
                &Tmkdir {
                    dfid: 1,
                    name: "dir".to_owned(),
                    mode: 0o755,
                    gid: u32::MAX,
                },
            )
        }),
    )
    .await;
    assert_eq!(response_type(&mkdir), P9_RMKDIR);
    decode_message_as(&mkdir, read_rmkdir).expect("Rmkdir decodes");

    let symlink = call(
        &session,
        frame(P9_TSYMLINK, 3, |writer| {
            write_tsymlink(
                writer,
                &Tsymlink {
                    dfid: 1,
                    name: "link".to_owned(),
                    symtgt: "dir/file".to_owned(),
                    gid: u32::MAX,
                },
            )
        }),
    )
    .await;
    assert_eq!(response_type(&symlink), P9_RSYMLINK);
    decode_message_as(&symlink, read_rsymlink).expect("Rsymlink decodes");

    let link_fid = 2;
    let walk_link = call(
        &session,
        frame(P9_TWALK, 4, |writer| {
            write_twalk(
                writer,
                &Twalk {
                    fid: 1,
                    newfid: link_fid,
                    wnames: vec!["link".to_owned()],
                },
            )
        }),
    )
    .await;
    assert_eq!(response_type(&walk_link), P9_RWALK);
    decode_message_as(&walk_link, read_rwalk).expect("link walk decodes");

    let readlink = call(
        &session,
        frame(P9_TREADLINK, 5, |writer| {
            write_fid_request(writer, FidRequest { fid: link_fid });
            Ok(())
        }),
    )
    .await;
    assert_eq!(response_type(&readlink), P9_RREADLINK);
    let (_, target) = decode_message_as(&readlink, read_rreadlink).expect("Rreadlink decodes");
    assert_eq!(target.target, "dir/file");

    let file_fid = 3;
    clone_root(&session, 6, file_fid).await;
    let create = call(
        &session,
        frame(P9_TLCREATE, 7, |writer| {
            write_tlcreate(
                writer,
                &Tlcreate {
                    fid: file_fid,
                    name: "file".to_owned(),
                    flags: P9_O_RDWR | P9_O_CREAT,
                    mode: 0o640,
                    gid: u32::MAX,
                },
            )
        }),
    )
    .await;
    assert_eq!(response_type(&create), P9_RLCREATE);
    decode_message_as(&create, read_rlcreate).expect("Rlcreate decodes");

    let fsync = call(
        &session,
        frame(P9_TFSYNC, 8, |writer| {
            write_tfsync(
                writer,
                Tfsync {
                    fid: file_fid,
                    datasync: 0,
                },
            );
            Ok(())
        }),
    )
    .await;
    assert_eq!(response_type(&fsync), P9_RFSYNC);

    let setattr = call(
        &session,
        frame(P9_TSETATTR, 9, |writer| {
            write_tsetattr(
                writer,
                Tsetattr {
                    fid: file_fid,
                    valid: P9_SETATTR_MODE,
                    mode: 0o600,
                    uid: 0,
                    gid: 0,
                    size: 0,
                    atime: P9Time { sec: 0, nsec: 0 },
                    mtime: P9Time { sec: 0, nsec: 0 },
                },
            );
            Ok(())
        }),
    )
    .await;
    assert_eq!(response_type(&setattr), P9_RSETATTR);

    let link = call(
        &session,
        frame(P9_TLINK, 10, |writer| {
            write_tlink(
                writer,
                &Tlink {
                    dfid: 1,
                    fid: file_fid,
                    name: "hard".to_owned(),
                },
            )
        }),
    )
    .await;
    assert_eq!(response_type(&link), P9_RLINK);

    let rename = call(
        &session,
        frame(P9_TRENAMEAT, 11, |writer| {
            write_trenameat(
                writer,
                &Trenameat {
                    olddirfid: 1,
                    oldname: "hard".to_owned(),
                    newdirfid: 1,
                    newname: "renamed".to_owned(),
                },
            )
        }),
    )
    .await;
    assert_eq!(response_type(&rename), P9_RRENAMEAT);

    let unlink = call(
        &session,
        frame(P9_TUNLINKAT, 12, |writer| {
            write_tunlinkat(
                writer,
                &Tunlinkat {
                    dirfid: 1,
                    name: "renamed".to_owned(),
                    flags: 0,
                },
            )
        }),
    )
    .await;
    assert_eq!(response_type(&unlink), P9_RUNLINKAT);

    let mknod = call(
        &session,
        frame(P9_TMKNOD, 13, |writer| {
            write_tmknod(
                writer,
                &Tmknod {
                    dfid: 1,
                    name: "node".to_owned(),
                    mode: S_IFREG | 0o600,
                    major: 0,
                    minor: 0,
                    gid: u32::MAX,
                },
            )
        }),
    )
    .await;
    assert_eq!(response_type(&mknod), P9_RMKNOD);
    decode_message_as(&mknod, read_rmknod).expect("Rmknod decodes");

    let statfs = call(
        &session,
        frame(P9_TSTATFS, 14, |writer| {
            write_tstatfs(writer, FidRequest { fid: 1 });
            Ok(())
        }),
    )
    .await;
    assert_eq!(response_type(&statfs), P9_RSTATFS);
    decode_message_as(&statfs, read_rstatfs).expect("Rstatfs decodes");

    let lock = call(
        &session,
        frame(P9_TLOCK, 15, |writer| {
            write_tlock(
                writer,
                &Tlock {
                    fid: file_fid,
                    type_: P9_LOCK_TYPE_WRLCK,
                    flags: 0,
                    start: 0,
                    length: 16,
                    proc_id: 10,
                    client_id: "dotl-test".to_owned(),
                },
            )
        }),
    )
    .await;
    assert_eq!(response_type(&lock), P9_RLOCK);
    let (_, status) = decode_message_as(&lock, read_rlock).expect("Rlock decodes");
    assert_eq!(status.status, P9_LOCK_SUCCESS);

    let getlock = call(
        &session,
        frame(P9_TGETLOCK, 16, |writer| {
            write_tgetlock(
                writer,
                &Tgetlock {
                    fid: file_fid,
                    type_: P9_LOCK_TYPE_WRLCK,
                    start: 0,
                    length: 16,
                    proc_id: 10,
                    client_id: "dotl-test".to_owned(),
                },
            )
        }),
    )
    .await;
    assert_eq!(response_type(&getlock), P9_RGETLOCK);
    let (_, holder) = decode_message_as(&getlock, read_rgetlock).expect("Rgetlock decodes");
    assert_eq!(holder.type_, P9_LOCK_TYPE_UNLCK);
}

#[tokio::test]
async fn refuses_unsupported_auth_xattr_and_legacy_requests_with_enotsup() {
    let session = P9Session::new(MemoryFs::empty());
    negotiate_and_attach(&session).await;

    let auth = call(
        &session,
        frame(P9_TAUTH, 20, |writer| {
            write_tauth(
                writer,
                &Tauth {
                    afid: P9_NOFID,
                    uname: "guest".to_owned(),
                    aname: String::new(),
                    n_uname: u32::MAX,
                },
            )
        }),
    )
    .await;
    assert_eq!(response_type(&auth), P9_RLERROR);
    let (_, auth_error) = decode_message_as(&auth, read_rlerror).expect("Tauth error decodes");
    assert_eq!(auth_error.ecode, 95);

    let xattr = call(&session, frame(P9_TXATTRWALK, 21, |_| Ok(()))).await;
    assert_eq!(response_type(&xattr), P9_RLERROR);
    let (_, xattr_error) = decode_message_as(&xattr, read_rlerror).expect("xattr error decodes");
    assert_eq!(xattr_error.ecode, 95);

    let legacy = call(
        &session,
        frame(P9_TOPEN, 22, |writer| {
            writer.u32(1);
            writer.u8(0);
            Ok(())
        }),
    )
    .await;
    assert_eq!(response_type(&legacy), P9_RLERROR);
    let (_, legacy_error) = decode_message_as(&legacy, read_rlerror).expect("legacy error decodes");
    assert_eq!(legacy_error.ecode, 95);
}
