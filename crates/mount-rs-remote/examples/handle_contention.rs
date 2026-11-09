//! Same-handle admission diagnostic. Control serializes calls in this binary;
//! it is not a measurement of an older revision or cross-AZ production.
use mount_rs_cluster::{SessionCredentials, SessionHttpHandler, VolumeSessions};
use mount_rs_core::FsDriver;
use mount_rs_http::{HttpServer, HttpServerOptions};
use mount_rs_remote::RemoteFs;
use mount_rs_sdk::{Filesystem, SplitOptions};
use serde_json::json;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

#[tokio::main(worker_threads = 4)]
async fn main() {
    let owner = Arc::new(
        Filesystem::split(SplitOptions::memory("handle-load", 4096))
            .await
            .unwrap(),
    );
    let sessions =
        Arc::new(VolumeSessions::new(owner.clone(), 2, 4, Duration::from_secs(300)).unwrap());
    let credential = "h".repeat(32);
    let handler = SessionHttpHandler::new(
        "volume",
        SessionCredentials::new(vec![("client".into(), credential.clone())]).unwrap(),
        sessions.clone(),
    )
    .unwrap();
    let server = HttpServer::start_with_handler(
        Arc::new(handler),
        HttpServerOptions {
            http2_prior_knowledge: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let remote = RemoteFs::connect_with_http2(
        &server.url(),
        "volume",
        &credential,
        Duration::from_secs(30),
        true,
    )
    .await
    .unwrap();
    let handle = remote.open("/data", "w+", 0o600).await.unwrap();
    assert_eq!(handle.write(&vec![37; 4096], Some(0)).await.unwrap(), 4096);
    let mut rows = Vec::new();
    for (pass, serialized) in [("a1", true), ("b1", false), ("b2", false), ("a2", true)] {
        for callers in [1, 5, 10, 25, 50, 100] {
            for metadata in [true, false] {
                let barrier = Arc::new(tokio::sync::Barrier::new(callers));
                let gate = Arc::new(tokio::sync::Semaphore::new(1));
                let mut tasks = tokio::task::JoinSet::new();
                for _ in 0..callers {
                    let (handle, barrier, gate) = (handle.clone(), barrier.clone(), gate.clone());
                    tasks.spawn(async move {
                        let mut samples = Vec::new();
                        let mut bytes = [0; 4096];
                        for iteration in 0..=100 {
                            barrier.wait().await;
                            let started = Instant::now();
                            let permit = if serialized {
                                Some(gate.acquire().await.unwrap())
                            } else {
                                None
                            };
                            if metadata {
                                assert_eq!(handle.stat().await.unwrap().size, 4096);
                            } else {
                                assert_eq!(handle.read(&mut bytes, Some(0)).await.unwrap(), 4096);
                                assert!(bytes.iter().all(|b| *b == 37));
                            }
                            drop(permit);
                            if iteration != 0 {
                                samples.push(started.elapsed().as_secs_f64() * 1000.0);
                            }
                        }
                        samples
                    });
                }
                let mut values = Vec::new();
                while let Some(result) = tasks.join_next().await {
                    values.extend(result.unwrap());
                }
                values.sort_by(f64::total_cmp);
                assert_eq!(values.len(), callers * 100);
                let percentile = |p: usize| values[(values.len() * p).div_ceil(100) - 1];
                rows.push(json!({"pass":pass,"serialized_control":serialized,"callers":callers,"operation":if metadata {"metadata"}else{"read-4k"},"samples":values.len(),"p50_ms":percentile(50),"p99_ms":percentile(99),"max_ms":values.last().unwrap()}));
                eprintln!("completed pass={pass} callers={callers} metadata={metadata}");
            }
        }
    }
    handle.close().await.unwrap();
    remote.shutdown().await.unwrap();
    server.close().await.unwrap();
    sessions.close().await.unwrap();
    owner.shutdown().await.unwrap();
    println!(
        "{}",
        json!({"complete":true,"errors":0,"http2":true,"workers":4,"providers":"memory","scope":"one server, one session and handle; shared client/server runtime; read-only barrier bursts; one warmup excluded; serialized control includes queue wait; no old-revision or production claim","results":rows})
    );
}
