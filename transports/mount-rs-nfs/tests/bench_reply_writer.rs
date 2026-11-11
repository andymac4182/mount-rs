//! Benchmark for the per-connection reply path: one real loopback TCP
//! connection through the server's own connection loop, with a closed loop
//! of `depth` calls in flight.
//!
//! `cargo test --release -p mount-rs-nfs --test bench_reply_writer -- --ignored --nocapture`
//!
//! Each line reports calls per second and the p50 and p99 of the time from
//! writing a call to parsing its reply.

use std::time::{Duration, Instant};

use mount_rs_core::FsDriver;
use mount_rs_memfs::MemoryFs;
use mount_rs_nfs::constants::{
    MOUNT_PROGRAM, MOUNT_V3, MOUNTPROC3_MNT, MOUNTPROC3_NULL, NFS_PROGRAM, NFS_V3, NFS3_OK,
    NFSPROC3_LOOKUP, NFSPROC3_READ,
};
use mount_rs_nfs::protocol::{
    DirOpArgs, Read3args, read_lookup_res, read_mount_res, write_dir_op, write_read_args,
};
use mount_rs_nfs::xdr::encode_xdr;
use mount_rs_nfs::{
    NfsServer, NfsServerOptions, RecordAssembler, decode_reply, encode_call, frame_record,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

async fn exchange(stream: &mut TcpStream, call: Vec<u8>) -> Vec<u8> {
    stream
        .write_all(&frame_record(&call).unwrap())
        .await
        .unwrap();
    let mut marker = [0_u8; 4];
    stream.read_exact(&mut marker).await.unwrap();
    let mut record = vec![0_u8; (u32::from_be_bytes(marker) & 0x7fff_ffff) as usize];
    stream.read_exact(&mut record).await.unwrap();
    record
}

/// Keep `depth` calls in flight until `total` replies have arrived. `frame`
/// is one framed call whose xid (bytes 4..8) is rewritten per call.
async fn closed_loop(
    stream: &mut TcpStream,
    name: &str,
    frame: &[u8],
    reply_bytes: usize,
    depth: usize,
    total: usize,
) {
    let mut sent_at = vec![Instant::now(); total];
    let mut latencies = Vec::with_capacity(total);
    let mut assembler = RecordAssembler::default();
    let mut buffer = vec![0_u8; 256 * 1024];
    let mut out = Vec::new();
    let mut sent = 0_usize;
    let started = Instant::now();
    let push = |out: &mut Vec<u8>, sent: &mut usize, sent_at: &mut Vec<Instant>| {
        let at = out.len();
        out.extend_from_slice(frame);
        out[at + 4..at + 8].copy_from_slice(&(*sent as u32).to_be_bytes());
        sent_at[*sent] = Instant::now();
        *sent += 1;
    };
    for _ in 0..depth.min(total) {
        push(&mut out, &mut sent, &mut sent_at);
    }
    stream.write_all(&out).await.unwrap();
    while latencies.len() < total {
        let count = stream.read(&mut buffer).await.unwrap();
        assert!(count > 0, "server closed the benchmark connection");
        out.clear();
        for record in assembler.push(&buffer[..count]).unwrap() {
            assert_eq!(record.len(), reply_bytes, "unexpected reply size");
            let xid = u32::from_be_bytes(record[..4].try_into().unwrap()) as usize;
            latencies.push(sent_at[xid].elapsed());
            if sent < total {
                push(&mut out, &mut sent, &mut sent_at);
            }
        }
        if !out.is_empty() {
            stream.write_all(&out).await.unwrap();
        }
    }
    let wall = started.elapsed();
    latencies.sort_unstable();
    println!(
        "writerbench call={name} depth={depth} calls/s={:.0} p50={:?} p99={:?}",
        total as f64 / wall.as_secs_f64(),
        latencies[total / 2],
        latencies[total * 99 / 100],
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "benchmark"]
async fn bench_reply_writer_loopback() {
    const READ_BYTES: u32 = 32 * 1024;
    let fs = MemoryFs::empty();
    fs.write_file("/data", &vec![7_u8; READ_BYTES as usize])
        .await
        .unwrap();
    let server = NfsServer::new(fs, NfsServerOptions::default());
    let address = server.listen().await.unwrap();
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream.set_nodelay(true).unwrap();

    let record = exchange(
        &mut stream,
        encode_call(
            1,
            MOUNT_PROGRAM,
            MOUNT_V3,
            MOUNTPROC3_MNT,
            None,
            None,
            &encode_xdr(|writer| writer.string("/")),
        ),
    )
    .await;
    let (_, mut body) = decode_reply(&record).unwrap();
    let root = read_mount_res(&mut body).unwrap().fh.unwrap();
    let record = exchange(
        &mut stream,
        encode_call(
            2,
            NFS_PROGRAM,
            NFS_V3,
            NFSPROC3_LOOKUP,
            None,
            None,
            &encode_xdr(|writer| {
                write_dir_op(
                    writer,
                    &DirOpArgs {
                        dir: root.clone(),
                        name: "data".to_owned(),
                    },
                )
            }),
        ),
    )
    .await;
    let (_, mut body) = decode_reply(&record).unwrap();
    let found = read_lookup_res(&mut body).unwrap();
    assert_eq!(found.status, NFS3_OK);
    let file = found.object.unwrap();

    let null = frame_record(&encode_call(
        0,
        MOUNT_PROGRAM,
        MOUNT_V3,
        MOUNTPROC3_NULL,
        None,
        None,
        &[],
    ))
    .unwrap();
    let read = frame_record(&encode_call(
        0,
        NFS_PROGRAM,
        NFS_V3,
        NFSPROC3_READ,
        None,
        None,
        &encode_xdr(|writer| {
            write_read_args(
                writer,
                &Read3args {
                    file: file.clone(),
                    offset: 0,
                    count: READ_BYTES,
                },
            )
        }),
    ))
    .unwrap();
    let null_reply = exchange(&mut stream, null[4..].to_vec()).await.len();
    let read_reply = exchange(&mut stream, read[4..].to_vec()).await.len();
    assert!(read_reply > READ_BYTES as usize);

    // Warm up the connection, the runtime and the allocator.
    closed_loop(&mut stream, "warmup", &null, null_reply, 16, 20_000).await;
    // `NFS_BENCH_DEPTHS=1,16` and `NFS_BENCH_ROUNDS=1` narrow a run, so that
    // two builds can be alternated quickly on a busy machine.
    let depths = match std::env::var("NFS_BENCH_DEPTHS") {
        Ok(list) => list
            .split(',')
            .map(|depth| depth.parse().unwrap())
            .collect(),
        Err(_) => vec![1_usize, 16, 64],
    };
    let rounds = std::env::var("NFS_BENCH_ROUNDS").map_or(3, |rounds| rounds.parse().unwrap());
    for round in 0..rounds {
        println!("writerbench round={round}");
        for &depth in &depths {
            let scale = if depth == 1 { 1 } else { 4 };
            closed_loop(
                &mut stream,
                "null",
                &null,
                null_reply,
                depth,
                30_000 * scale,
            )
            .await;
            closed_loop(
                &mut stream,
                "read32k",
                &read,
                read_reply,
                depth,
                15_000 * scale,
            )
            .await;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
