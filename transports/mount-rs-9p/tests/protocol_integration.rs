use std::sync::Arc;
use std::time::Duration;

use mount_rs_9p::{
    P9_GETATTR_ALL, P9_NOFID, P9_NOTAG, P9_O_CREAT, P9_O_RDWR, P9_O_TRUNC, P9_RATTACH, P9_RGETATTR,
    P9_RLCREATE, P9_RREAD, P9_RREADDIR, P9_RVERSION, P9_RWALK, P9_RWRITE, P9_TATTACH, P9_TGETATTR,
    P9_TLCREATE, P9_TLOPEN, P9_TREAD, P9_TREADDIR, P9_TVERSION, P9_TWALK, P9_TWRITE, P9Server,
    P9ServerOptions, P9Session, P9SessionOptions, Tattach, Tgetattr, Tlcreate, Tlopen, Tread,
    Treaddir, Tversion, Twalk, Twrite, decode_message_as, encode_message, read_dirents,
    read_rattach, read_rgetattr, read_rlopen, read_rread, read_rreaddir, read_rversion, read_rwalk,
    read_rwrite, write_tattach, write_tgetattr, write_tlcreate, write_tlopen, write_tread,
    write_treaddir, write_tversion, write_twalk, write_twrite,
};
use mount_rs_memfs::MemoryFs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

fn frame<F>(type_: u8, tag: u16, write: F) -> Vec<u8>
where
    F: FnOnce(&mut mount_rs_9p::P9Writer) -> Result<(), mount_rs_9p::P9Error>,
{
    encode_message(type_, tag, 256, write).expect("test message encodes")
}

async fn call(session: &P9Session, request: Vec<u8>) -> Vec<u8> {
    session
        .handle_call(&request)
        .await
        .expect("complete request gets a response")
}

fn assert_type(bytes: &[u8], expected: u8) {
    let (header, _) = mount_rs_9p::decode_message(bytes).expect("valid response frame");
    assert_eq!(header.type_, expected);
}

fn version_request(tag: u16) -> Vec<u8> {
    frame(P9_TVERSION, tag, |writer| {
        write_tversion(
            writer,
            &Tversion {
                msize: 8192,
                version: "9P2000.L".to_owned(),
            },
        )
    })
}

async fn negotiate(session: &P9Session) {
    let response = call(session, version_request(P9_NOTAG)).await;
    assert_type(&response, P9_RVERSION);
    let (_, version) = decode_message_as(&response, read_rversion).expect("Rversion decodes");
    assert_eq!(version.version, "9P2000.L");
    assert_eq!(version.msize, 8192);
}

async fn attach(session: &P9Session, tag: u16, fid: u32) {
    let response = call(
        session,
        frame(P9_TATTACH, tag, |writer| {
            write_tattach(
                writer,
                &Tattach {
                    fid,
                    afid: P9_NOFID,
                    uname: "rootless-test".to_owned(),
                    aname: String::new(),
                    n_uname: u32::MAX,
                },
            )
        }),
    )
    .await;
    assert_type(&response, P9_RATTACH);
    decode_message_as(&response, read_rattach).expect("Rattach decodes");
}

#[tokio::test]
async fn session_lifecycle_exercises_lifecycle_io_attrs_and_readdir() {
    // This test intentionally uses only the rootless session API; it does not
    // require a kernel 9P client or mount privileges on either macOS or Linux.
    let session = P9Session::with_options(
        Arc::new(MemoryFs::empty()),
        P9SessionOptions {
            msize: Some(8192),
            ..P9SessionOptions::default()
        },
    );
    negotiate(&session).await;
    attach(&session, 1, 1).await;

    let response = call(
        &session,
        frame(P9_TWALK, 2, |writer| {
            write_twalk(
                writer,
                &Twalk {
                    fid: 1,
                    newfid: 2,
                    wnames: Vec::new(),
                },
            )
        }),
    )
    .await;
    assert_type(&response, P9_RWALK);
    let (_, walked) = decode_message_as(&response, read_rwalk).expect("Rwalk decodes");
    assert!(walked.wqids.is_empty());

    let response = call(
        &session,
        frame(P9_TLCREATE, 3, |writer| {
            write_tlcreate(
                writer,
                &Tlcreate {
                    fid: 2,
                    name: "hello".to_owned(),
                    flags: P9_O_RDWR | P9_O_CREAT | P9_O_TRUNC,
                    mode: 0o666,
                    gid: u32::MAX,
                },
            )
        }),
    )
    .await;
    assert_type(&response, P9_RLCREATE);
    decode_message_as(&response, read_rlopen).expect("Rlcreate decodes");

    let payload = b"hello from 9P".to_vec();
    let response = call(
        &session,
        frame(P9_TWRITE, 4, |writer| {
            write_twrite(
                writer,
                &Twrite {
                    fid: 2,
                    offset: 0,
                    data: payload.clone(),
                },
            );
            Ok(())
        }),
    )
    .await;
    assert_type(&response, P9_RWRITE);
    let (_, written) = decode_message_as(&response, read_rwrite).expect("Rwrite decodes");
    assert_eq!(written.count as usize, payload.len());

    let response = call(
        &session,
        frame(P9_TREAD, 5, |writer| {
            write_tread(
                writer,
                Tread {
                    fid: 2,
                    offset: 0,
                    count: 4096,
                },
            );
            Ok(())
        }),
    )
    .await;
    assert_type(&response, P9_RREAD);
    let (_, read) = decode_message_as(&response, read_rread).expect("Rread decodes");
    assert_eq!(read.data, payload);

    let response = call(
        &session,
        frame(P9_TGETATTR, 6, |writer| {
            write_tgetattr(
                writer,
                Tgetattr {
                    fid: 2,
                    request_mask: P9_GETATTR_ALL,
                },
            );
            Ok(())
        }),
    )
    .await;
    assert_type(&response, P9_RGETATTR);
    let (_, attrs) = decode_message_as(&response, read_rgetattr).expect("Rgetattr decodes");
    assert_eq!(attrs.size, payload.len() as u64);

    let response = call(
        &session,
        frame(P9_TWALK, 7, |writer| {
            write_twalk(
                writer,
                &Twalk {
                    fid: 1,
                    newfid: 3,
                    wnames: Vec::new(),
                },
            )
        }),
    )
    .await;
    decode_message_as(&response, read_rwalk).expect("directory walk decodes");
    let response = call(
        &session,
        frame(P9_TLOPEN, 8, |writer| {
            write_tlopen(writer, Tlopen { fid: 3, flags: 0 });
            Ok(())
        }),
    )
    .await;
    assert_type(&response, mount_rs_9p::P9_RLOPEN);
    decode_message_as(&response, read_rlopen).expect("directory open decodes");
    let response = call(
        &session,
        frame(P9_TREADDIR, 9, |writer| {
            write_treaddir(
                writer,
                Treaddir {
                    fid: 3,
                    offset: 0,
                    count: 4096,
                },
            );
            Ok(())
        }),
    )
    .await;
    assert_type(&response, P9_RREADDIR);
    let (_, listing) = decode_message_as(&response, read_rreaddir).expect("Rreaddir decodes");
    let entries = read_dirents(&listing.data).expect("directory entries decode");
    assert!(entries.iter().any(|entry| entry.name == "hello"));

    for (tag, fid) in [(10, 2), (11, 3), (12, 1)] {
        let response = call(
            &session,
            frame(mount_rs_9p::P9_TCLUNK, tag, |writer| {
                mount_rs_9p::write_fid_request(writer, mount_rs_9p::FidRequest { fid });
                Ok(())
            }),
        )
        .await;
        assert_type(&response, mount_rs_9p::P9_RCLUNK);
    }
    assert_eq!(session.inflight(), 0);
    assert_eq!(session.stats().errors, 0);
}

async fn read_frame(stream: &mut TcpStream) -> Vec<u8> {
    let mut header = [0_u8; 7];
    stream.read_exact(&mut header).await.expect("TCP header");
    let size = u32::from_le_bytes(header[..4].try_into().expect("size bytes")) as usize;
    assert!(size >= header.len());
    let mut frame = Vec::with_capacity(size);
    frame.extend_from_slice(&header);
    frame.resize(size, 0);
    stream
        .read_exact(&mut frame[header.len()..])
        .await
        .expect("TCP body");
    frame
}

#[tokio::test]
async fn tcp_server_accepts_rootless_loopback_protocol_on_any_host_os() {
    let server = Arc::new(
        P9Server::bind(
            MemoryFs::empty(),
            P9ServerOptions {
                host: "127.0.0.1".to_owned(),
                port: 0,
                ..P9ServerOptions::default()
            },
        )
        .await
        .expect("bind loopback TCP server"),
    );
    let serving = Arc::clone(&server);
    let task = tokio::spawn(async move { serving.serve().await });
    let mut stream = timeout(
        Duration::from_secs(2),
        TcpStream::connect(server.local_addr().expect("server address")),
    )
    .await
    .expect("connect does not time out")
    .expect("connect loopback server");

    stream
        .write_all(&version_request(1))
        .await
        .expect("send version");
    let response = read_frame(&mut stream).await;
    assert_type(&response, P9_RVERSION);
    decode_message_as(&response, read_rversion).expect("TCP Rversion decodes");

    stream
        .write_all(&frame(P9_TATTACH, 2, |writer| {
            write_tattach(
                writer,
                &Tattach {
                    fid: 1,
                    afid: P9_NOFID,
                    uname: "tcp-test".to_owned(),
                    aname: String::new(),
                    n_uname: u32::MAX,
                },
            )
        }))
        .await
        .expect("send attach");
    let response = read_frame(&mut stream).await;
    assert_type(&response, P9_RATTACH);
    decode_message_as(&response, read_rattach).expect("TCP Rattach decodes");

    server.shutdown();
    timeout(Duration::from_secs(2), task)
        .await
        .expect("server task stops")
        .expect("server task joins")
        .expect("server exits cleanly");
}
