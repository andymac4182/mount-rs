//! Public HTTP -> owner session -> chunked driver -> gated S3 block service.
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn remote_positional_reads_reach_backing_service_concurrently() {
    for http2 in [false, true] {
        for headers in [false, true] {
            run_overlap(http2, headers).await;
        }
    }
}

async fn run_overlap(http2: bool, headers: bool) {
    use mount_rs_cluster::{SessionCredentials, SessionHttpHandler, VolumeSessions};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let entered = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    // Reads the backing service has been released to answer.
    let answered = Arc::new(AtomicUsize::new(0));
    let finished = answered.clone();
    let arrivals = entered.clone();
    let permits = release.clone();
    let storage = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let arrivals = arrivals.clone();
            let permits = permits.clone();
            let served = finished.clone();
            connections.spawn(async move {
                let mut header = Vec::new();
                while !header.ends_with(b"\r\n\r\n") {
                    header.push(socket.read_u8().await.unwrap());
                    assert!(header.len() < 16384);
                }
                let header = String::from_utf8(header).unwrap();
                let length = header.lines().find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().unwrap())
                }).unwrap_or(0);
                let mut body = vec![0; length];
                socket.read_exact(&mut body).await.unwrap();
                let payload = if header.starts_with("PUT ") {
                    assert_eq!(body, b"data");
                    ""
                } else {
                    assert!(header.starts_with("GET "));
                    arrivals.fetch_add(1, Ordering::SeqCst);
                    permits.acquire().await.unwrap().forget();
                    served.fetch_add(1, Ordering::SeqCst);
                    "data"
                };
                let response = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: \"test\"\r\nLast-Modified: Wed, 01 Jan 2025 00:00:00 GMT\r\nConnection: close\r\n\r\n{payload}", payload.len());
                socket.write_all(response.as_bytes()).await.unwrap();
            });
        }
    });
    // The object-store provider keeps blocks it wrote in a process-local
    // cache, so seed the file through one owner and serve it from a second,
    // cold one: only then does a read have to reach the backing service.
    let root = std::env::temp_dir().join(format!(
        "mount-rs-parallel-{}-{http2}-{headers}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let mut options = mount_rs_sdk::SplitOptions::memory("parallel-public", 4096);
    options.metadata = mount_rs_sdk::StoreConfig::Sqlite {
        path: root.join("metadata.sqlite"),
    };
    options.blocks = mount_rs_sdk::StoreConfig::R2 {
        endpoint,
        bucket: "blocks".into(),
        prefix: "test".into(),
        access_key_id: "test".into(),
        secret_access_key: "test".into(),
        durable: false,
    };
    let seed = mount_rs_sdk::Filesystem::split(options.clone())
        .await
        .unwrap();
    seed.driver().write_file("/data", b"data").await.unwrap();
    seed.shutdown().await.unwrap();
    assert_eq!(entered.load(Ordering::SeqCst), 0);
    let owner = Arc::new(mount_rs_sdk::Filesystem::split(options).await.unwrap());
    let registry =
        Arc::new(VolumeSessions::new(owner.clone(), 2, 4, Duration::from_secs(60)).unwrap());
    let secret = "s".repeat(32);
    let handler = SessionHttpHandler::new(
        "volume",
        SessionCredentials::new(vec![("client".into(), secret.clone())]).unwrap(),
        registry.clone(),
    )
    .unwrap();
    let server = mount_rs_http::HttpServer::start_with_handler(
        Arc::new(handler),
        mount_rs_http::HttpServerOptions {
            http2_prior_knowledge: http2,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let remote = RemoteFs::connect_with_http2(
        &server.url(),
        "volume",
        &secret,
        Duration::from_secs(5),
        http2,
    )
    .await
    .unwrap();
    if headers {
        remote.enable_header_reads().await.unwrap();
    }
    let handle = remote.open("/data", "r", 0).await.unwrap();
    let mut reads = tokio::task::JoinSet::new();
    for _ in 0..2 {
        let handle = handle.clone();
        reads.spawn(async move {
            let mut bytes = [0; 4];
            let n = handle.read(&mut bytes, Some(0)).await.unwrap();
            (n, bytes)
        });
    }
    tokio::time::timeout(Duration::from_secs(2), async {
        while entered.load(Ordering::SeqCst) != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "both public reads must reach backing storage before either completes: entered={} http2={http2} headers={headers}",
            entered.load(Ordering::SeqCst)
        )
    });
    // Close is sent while both reads are inside the owner's handle. The
    // owner, not the client, makes it wait for them: it must not return
    // before the backing service has answered both.
    let closing = handle.clone();
    let answered_at_close = answered.clone();
    let close = tokio::spawn(async move {
        closing.close().await.unwrap();
        answered_at_close.load(Ordering::SeqCst)
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !close.is_finished(),
        "close returned with reads still running on the owner: http2={http2} headers={headers}"
    );
    release.add_permits(2);
    assert_eq!(close.await.unwrap(), 2);
    // Both reads were admitted before the close, so both complete.
    while let Some(result) = reads.join_next().await {
        assert_eq!(result.unwrap(), (4, *b"data"));
    }
    let mut bytes = [0; 4];
    assert_eq!(
        handle.read(&mut bytes, Some(0)).await.unwrap_err().code,
        ErrorCode::Ebadf
    );
    handle.close().await.unwrap();
    remote.shutdown().await.unwrap();
    server.close().await.unwrap();
    registry.close().await.unwrap();
    owner.shutdown().await.unwrap();
    storage.abort();
    assert!(storage.await.unwrap_err().is_cancelled());
    let _ = std::fs::remove_dir_all(&root);
}
