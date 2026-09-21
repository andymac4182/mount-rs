#![cfg(feature = "otlp")]

use std::io;
use std::time::Duration;

use mount_rs_core::storage::BlockReconcileReport;
use mount_rs_observability::{OtlpConfig, Telemetry, TelemetryConfig, install_otlp};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant, timeout, timeout_at};

const SECRET_PATH: &str = "/tenant/super-secret-w30-collector.txt";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_collector_receives_signals_and_reconciliation_telemetry_without_raw_paths() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, mut receiver) = mpsc::channel(16);
    let (stop_sender, mut stop_receiver) = oneshot::channel();
    let collector = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut stop_receiver => break,
                accepted = listener.accept() => {
                    let (mut stream, _) = accepted?;
                    let (path, body) = read_request(&mut stream).await?;
                    stream
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .await?;
                    sender
                        .send((path, body))
                        .await
                        .map_err(|_| io::Error::other("collector assertion channel closed"))?;
                }
            }
        }
        Ok::<(), io::Error>(())
    });

    let guard = install_otlp(OtlpConfig {
        endpoint: format!("http://{address}"),
        service_name: "w30-collector-test".to_owned(),
        export_timeout: Duration::from_secs(3),
        ..OtlpConfig::default()
    })
    .unwrap();
    let telemetry = Telemetry::new(TelemetryConfig::enabled("w30-collector-test"));

    telemetry
        .observe_result(
            "collector-test",
            "read",
            Some(SECRET_PATH),
            async { Ok::<_, ()>(()) },
            |_| None,
        )
        .await
        .unwrap();
    telemetry.record_bytes("write", 42);
    telemetry.record_reconcile(&BlockReconcileReport {
        scanned: 11,
        protected: 7,
        recent: 3,
        deleted: 1,
    });
    tracing::info!(
        target: "mount_rs.event",
        telemetry_schema = "mount-rs.telemetry.v1",
        event_name = "w30.collector.test",
        boundary = "collector-test",
        operation = "signals",
        "collector integration test"
    );

    guard.force_flush().unwrap();

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut requests = Vec::new();
    loop {
        requests.push(
            timeout_at(deadline, receiver.recv())
                .await
                .expect("collector request timeout")
                .expect("collector stopped before receiving all signals"),
        );
        let mut paths = requests
            .iter()
            .map(|(path, _)| path.as_str())
            .collect::<Vec<_>>();
        paths.sort_unstable();
        paths.dedup();
        if paths == ["/v1/logs", "/v1/metrics", "/v1/traces"] {
            break;
        }
    }
    let shutdown = guard.shutdown();
    stop_sender.send(()).unwrap();
    timeout(Duration::from_secs(10), collector)
        .await
        .expect("collector task timeout")
        .expect("collector task panicked")
        .unwrap();
    while let Ok(request) = receiver.try_recv() {
        requests.push(request);
    }
    shutdown.unwrap();

    let mut paths = requests
        .iter()
        .map(|(path, _)| path.as_str())
        .collect::<Vec<_>>();
    paths.sort_unstable();
    paths.dedup();
    assert_eq!(paths, ["/v1/logs", "/v1/metrics", "/v1/traces"]);
    let metric_body = requests
        .iter()
        .find_map(|(path, body)| (path == "/v1/metrics").then_some(body))
        .expect("collector did not receive metrics");
    for instrument in [
        "mount_rs.blocks.reconcile.scanned",
        "mount_rs.blocks.reconcile.protected",
        "mount_rs.blocks.reconcile.recent",
        "mount_rs.blocks.reconcile.deleted",
    ] {
        assert!(
            contains_ascii(metric_body, instrument),
            "reconciliation metric {instrument} was not exported"
        );
    }
    let log_body = requests
        .iter()
        .find_map(|(path, body)| (path == "/v1/logs").then_some(body))
        .expect("collector did not receive logs");
    assert!(
        contains_ascii(log_body, "mount_rs.blocks.reconciled"),
        "reconciliation completion event was not exported"
    );
    for (_, body) in requests {
        assert!(!body.is_empty(), "collector received an empty OTLP payload");
        assert!(
            !body
                .windows(SECRET_PATH.len())
                .any(|window| window == SECRET_PATH.as_bytes())
        );
    }
}

fn contains_ascii(body: &[u8], value: &str) -> bool {
    body.windows(value.len())
        .any(|window| window == value.as_bytes())
}

async fn read_request(stream: &mut TcpStream) -> io::Result<(String, Vec<u8>)> {
    const HEADER_SEPARATOR: &[u8] = b"\r\n\r\n";
    const MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;

    let mut request = Vec::new();
    let header_end = loop {
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "collector request ended before headers",
            ));
        }
        request.extend_from_slice(&chunk[..read]);
        if request.len() > MAX_REQUEST_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "collector request exceeded test limit",
            ));
        }
        if let Some(position) = request
            .windows(HEADER_SEPARATOR.len())
            .position(|window| window == HEADER_SEPARATOR)
        {
            break position;
        }
    };

    let header = String::from_utf8_lossy(&request[..header_end]);
    if header.lines().any(|line| {
        let Some((name, value)) = line.split_once(':') else {
            return false;
        };
        name.trim().eq_ignore_ascii_case("expect")
            && value.trim().eq_ignore_ascii_case("100-continue")
    }) {
        stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").await?;
    }
    let path = header
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing request path"))?
        .to_owned();
    let content_length = header
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim().eq_ignore_ascii_case("content-length").then(|| {
                value.trim().parse::<usize>().map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "invalid content length")
                })
            })
        })
        .transpose()?
        .unwrap_or(0);
    let body_start = header_end + HEADER_SEPARATOR.len();
    let mut body = request[body_start..].to_vec();
    while body.len() < content_length {
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "collector request ended before body",
            ));
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(content_length);
    Ok((path, body))
}
