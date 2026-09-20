use mount_rs_core::MemoryFs;
use mount_rs_nfs::constants::{
    CREATE_UNCHECKED, FILE_SYNC, MOUNT_PROGRAM, MOUNT_V3, MOUNTPROC3_MNT, NFS_PROGRAM, NFS_V3,
    NFS3_OK, NFSPROC3_CREATE, NFSPROC3_READ, NFSPROC3_WRITE,
};
use mount_rs_nfs::protocol::{
    Create3args, DirOpArgs, Read3args, Sattr3, Write3args, read_create_res, read_mount_res,
    read_read_res, read_write_res, write_create_args, write_read_args, write_write_args,
};
use mount_rs_nfs::rpc::{RPC_SUCCESS, decode_reply, encode_call, frame_record};
use mount_rs_nfs::xdr::encode_xdr;
use mount_rs_nfs::{NfsServer, NfsServerOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

async fn exchange(stream: &mut TcpStream, call: Vec<u8>) -> Vec<u8> {
    stream
        .write_all(&frame_record(&call).unwrap())
        .await
        .unwrap();

    let mut marker = [0_u8; 4];
    stream.read_exact(&mut marker).await.unwrap();
    let marker = u32::from_be_bytes(marker);
    assert_ne!(marker & 0x8000_0000, 0, "test server returned fragments");
    let length = (marker & 0x7fff_ffff) as usize;
    let mut record = vec![0_u8; length];
    stream.read_exact(&mut record).await.unwrap();
    record
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rootless_tcp_round_trip_uses_real_filesystem_operations() {
    let server = NfsServer::new(MemoryFs::empty(), NfsServerOptions::default());
    let address = server.listen().await.unwrap();
    let mut stream = TcpStream::connect(address).await.unwrap();

    let mount_call = encode_call(
        1,
        MOUNT_PROGRAM,
        MOUNT_V3,
        MOUNTPROC3_MNT,
        None,
        None,
        &encode_xdr(|writer| writer.string("/")),
    );
    let record = exchange(&mut stream, mount_call).await;
    let (reply, mut body) = decode_reply(&record).unwrap();
    assert_eq!(reply.accept_stat, Some(RPC_SUCCESS));
    let mount = read_mount_res(&mut body).unwrap();
    body.end("MOUNT response").unwrap();
    assert_eq!(mount.status, 0);
    let root = mount.fh.unwrap();

    let create_args = encode_xdr(|writer| {
        write_create_args(
            writer,
            &Create3args {
                where_: DirOpArgs {
                    dir: root.clone(),
                    name: "wire.txt".to_owned(),
                },
                mode: CREATE_UNCHECKED,
                attributes: Some(Sattr3 {
                    mode: Some(0o644),
                    ..Sattr3::default()
                }),
                verf: None,
            },
        )
    });
    let create_call = encode_call(
        2,
        NFS_PROGRAM,
        NFS_V3,
        NFSPROC3_CREATE,
        None,
        None,
        &create_args,
    );
    let record = exchange(&mut stream, create_call).await;
    let (reply, mut body) = decode_reply(&record).unwrap();
    assert_eq!(reply.accept_stat, Some(RPC_SUCCESS));
    let created = read_create_res(&mut body).unwrap();
    body.end("CREATE response").unwrap();
    assert_eq!(created.status, NFS3_OK);
    let file = created.obj.unwrap();

    let write_args = encode_xdr(|writer| {
        write_write_args(
            writer,
            &Write3args {
                file: file.clone(),
                offset: 0,
                count: 11,
                stable: FILE_SYNC,
                data: b"hello wire!".to_vec(),
            },
        )
    });
    let write_call = encode_call(
        3,
        NFS_PROGRAM,
        NFS_V3,
        NFSPROC3_WRITE,
        None,
        None,
        &write_args,
    );
    let record = exchange(&mut stream, write_call).await;
    let (reply, mut body) = decode_reply(&record).unwrap();
    assert_eq!(reply.accept_stat, Some(RPC_SUCCESS));
    let written = read_write_res(&mut body).unwrap();
    body.end("WRITE response").unwrap();
    assert_eq!(written.status, NFS3_OK);
    assert_eq!(written.count, 11);
    assert_eq!(written.committed, FILE_SYNC);

    let read_args = encode_xdr(|writer| {
        write_read_args(
            writer,
            &Read3args {
                file,
                offset: 0,
                count: 64,
            },
        )
    });
    let read_call = encode_call(
        4,
        NFS_PROGRAM,
        NFS_V3,
        NFSPROC3_READ,
        None,
        None,
        &read_args,
    );
    let record = exchange(&mut stream, read_call).await;
    let (reply, mut body) = decode_reply(&record).unwrap();
    assert_eq!(reply.accept_stat, Some(RPC_SUCCESS));
    let read = read_read_res(&mut body, 64).unwrap();
    body.end("READ response").unwrap();
    assert_eq!(read.status, NFS3_OK);
    assert_eq!(read.count, 11);
    assert_eq!(read.data, b"hello wire!");

    server.close().await.unwrap();
}
