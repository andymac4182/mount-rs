//! Local transport baseline, not native/TiDB/AWS qualification.
use mount_rs_cluster::{SessionCredentials, SessionHttpHandler, VolumeSessions};
use mount_rs_core::FsDriver;
use mount_rs_http::{HttpBody, HttpRequestHandler, HttpServer};
use mount_rs_remote::RemoteFs;
use mount_rs_sdk::{BlockCacheOptions, Filesystem, SplitOptions, SplitRuntimeOptions, StoreConfig};
use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};
struct Router(BTreeMap<String, SessionHttpHandler>);
impl HttpRequestHandler for Router {
    fn handle(
        &self,
        request: http::Request<HttpBody>,
    ) -> Pin<Box<dyn Future<Output = http::Response<HttpBody>> + Send + '_>> {
        let volume = request.uri().path().split('/').nth(3).unwrap_or_default();
        // Only benchmark-owned requests are generated; unknown routes still
        // reach a handler that validates the complete route before dispatch.
        let handler = self
            .0
            .get(volume)
            .unwrap_or_else(|| self.0.first_key_value().unwrap().1);
        handler.handle(request)
    }
}
#[tokio::main(worker_threads = 4)]
async fn main() {
    let count: usize = std::env::args()
        .nth(1)
        .expect("client count")
        .parse()
        .unwrap();
    assert!((1..=100).contains(&count));
    let provider = std::env::args().nth(2).unwrap_or_else(|| "memory".into());
    assert!(matches!(
        provider.as_str(),
        "memory" | "local" | "local-cached" | "rustfs" | "rustfs-cached" | "rustfs-disk"
    ));
    let metadata = std::env::args().nth(3).unwrap_or_else(|| "memory".into());
    assert!(matches!(metadata.as_str(), "memory" | "tidb"));
    let pool = if metadata == "tidb" {
        let fixture = std::env::var("MOUNT_RS_TIDB_RUN_DIR").expect("owned TiDB fixture required");
        assert!(std::path::Path::new(&fixture).is_dir());
        let connection = std::env::var("MOUNT_RS_TIDB_URL").expect("fixture URL required");
        let url = reqwest::Url::parse(&connection).expect("valid fixture URL");
        assert!(
            url.scheme() == "mysql" && matches!(url.host_str(), Some("localhost" | "127.0.0.1"))
        );
        assert!(
            url.username() == "root" && url.password().is_none(),
            "only isolated credential-free fixture accepted"
        );
        Some(mount_rs_sdk::TidbStorePool::new(&connection, 16).unwrap())
    } else {
        None
    };
    let run_nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let disk_budget = mount_rs_sdk::DiskCacheBudget::default();
    let local_root =
        std::env::temp_dir().join(format!("mount-rs-session-load-{}", std::process::id()));
    if provider.starts_with("local") || provider == "rustfs-disk" {
        std::fs::create_dir(&local_root).unwrap();
    }
    let object_client = if provider.starts_with("rustfs") {
        assert!(
            std::path::Path::new(
                &std::env::var("RUSTFS_RUN_DIR").expect("owned RustFS fixture required")
            )
            .is_dir()
        );
        let config = mount_rs_r2::R2Config::from_env().expect("RustFS fixture config");
        let endpoint = reqwest::Url::parse(&config.endpoint).expect("fixture endpoint");
        assert!(
            endpoint.scheme() == "http"
                && matches!(endpoint.host_str(), Some("localhost" | "127.0.0.1"))
        );
        Some(mount_rs_sdk::SharedR2Client::from_config(&config).unwrap())
    } else {
        None
    };
    let iterations = 100usize;
    let secret = "b".repeat(32);
    let mut owners = Vec::new();
    let mut registries = Vec::new();
    let mut routes = BTreeMap::new();
    for i in 0..count {
        let volume = format!("volume-{i}");
        let mut options = SplitOptions::memory(&volume, 4096);
        if let Some(pool) = &pool {
            options.metadata = StoreConfig::TidbShared {
                pool: pool.clone(),
                volume_key: format!("session-load-{run_nonce}-{i}"),
                durable: false,
            };
        }
        if provider.starts_with("local") {
            std::fs::create_dir(local_root.join(&volume)).unwrap();
            options.blocks = StoreConfig::Local {
                path: local_root.join(&volume),
            };
        }
        if let Some(client) = &object_client {
            options.blocks = StoreConfig::R2Shared {
                client: client.clone(),
                prefix: format!(
                    "{}/session-{run_nonce}/{volume}",
                    std::env::var("RUSTFS_COMBO_PREFIX").expect("fixture prefix")
                ),
                durable: false,
            };
        }
        let runtime = SplitRuntimeOptions {
            block_cache: provider.ends_with("-cached").then_some(BlockCacheOptions {
                max_bytes: 65536,
                max_entries: 16,
            }),
            ..SplitRuntimeOptions::default()
        };
        let owner = Arc::new(if provider == "rustfs-disk" {
            Filesystem::split_with_disk_cache_budget(
                options,
                runtime,
                mount_rs_sdk::DiskCacheOptions {
                    root: local_root.clone(),
                    scope: format!("{run_nonce}-{volume}"),
                    max_bytes: 65536,
                    max_entries: 16,
                },
                disk_budget.clone(),
            )
            .await
            .unwrap()
        } else {
            Filesystem::split_with_runtime(options, runtime)
                .await
                .unwrap()
        });
        let file = owner.driver().open("/file", "w", 0o600).await.unwrap();
        assert_eq!(file.write(&vec![42; 4096], Some(0)).await.unwrap(), 4096);
        file.close().await.unwrap();
        let registry =
            Arc::new(VolumeSessions::new(owner.clone(), 1, 4, Duration::from_secs(3600)).unwrap());
        routes.insert(
            volume.clone(),
            SessionHttpHandler::new(
                &volume,
                SessionCredentials::new(vec![("client".into(), secret.clone())]).unwrap(),
                registry.clone(),
            )
            .unwrap(),
        );
        owners.push(owner);
        registries.push(registry);
    }
    let server = HttpServer::start_with_handler(Arc::new(Router(routes)), Default::default())
        .await
        .unwrap();
    let mut clients = Vec::new();
    for i in 0..count {
        let fs = RemoteFs::connect(
            &server.url(),
            &format!("volume-{i}"),
            &secret,
            Duration::from_secs(10),
        )
        .await
        .unwrap();
        let file = fs.open("/file", "r", 0).await.unwrap();
        fs.stat("/file").await.unwrap();
        file.read(&mut vec![0; 4096], Some(0)).await.unwrap();
        clients.push((fs, file));
    }
    let mut phases = Vec::new();
    for content in [false, true] {
        let barrier = Arc::new(tokio::sync::Barrier::new(count + 1));
        let mut tasks = Vec::new();
        for (fs, file) in &clients {
            let (fs, file, barrier) = (fs.clone(), file.clone(), barrier.clone());
            tasks.push(tokio::spawn(async move {
                let mut samples = Vec::with_capacity(iterations);
                let mut buffer = vec![0; 4096];
                barrier.wait().await;
                for _ in 0..iterations {
                    let start = Instant::now();
                    if content {
                        assert_eq!(file.read(&mut buffer, Some(0)).await.unwrap(), 4096);
                    } else {
                        assert_eq!(fs.stat("/file").await.unwrap().size, 4096);
                    }
                    samples.push(start.elapsed().as_secs_f64() * 1000.0);
                    if content {
                        assert!(buffer.iter().all(|v| *v == 42));
                    }
                }
                samples
            }));
        }
        let start = Instant::now();
        barrier.wait().await;
        let mut samples = Vec::new();
        for task in tasks {
            samples.extend(task.await.unwrap());
        }
        let elapsed = start.elapsed().as_secs_f64();
        samples.sort_by(f64::total_cmp);
        let percentile =
            |p: f64| samples[((samples.len() as f64 * p).ceil() as usize).saturating_sub(1)];
        phases.push(serde_json::json!({"operation":if content {"retained_read_4k"} else {"stat_path"},"samples":samples.len(),"p50_ms":percentile(0.5),"p95_ms":percentile(0.95),"p99_ms":percentile(0.99),"max_ms":samples.last(),"operations_per_second":samples.len() as f64 / elapsed}));
    }
    for (fs, file) in clients {
        file.close().await.unwrap();
        fs.shutdown().await.unwrap();
    }
    server.close().await.unwrap();
    for registry in registries {
        registry.close().await.unwrap();
    }
    let mut cache_hits = 0;
    let mut cache_misses = 0;
    let mut disk_hits = 0;
    let mut disk_misses = 0;
    let mut disk_errors = 0;
    for owner in owners {
        if let Some(stats) = owner.disk_cache_stats().await.unwrap() {
            disk_hits += stats.hits;
            disk_misses += stats.misses;
            disk_errors += stats.io_failures + stats.corruptions + stats.admission_rejections;
        }
        if let Some(stats) = owner.block_cache_stats().await {
            cache_hits += stats.hits;
            cache_misses += stats.misses;
        }
        owner.shutdown().await.unwrap();
    }
    if let Some(pool) = pool {
        pool.close().await.unwrap();
    }
    if provider.starts_with("local") || provider == "rustfs-disk" {
        std::fs::remove_dir_all(&local_root).unwrap();
    }
    println!(
        "{}",
        serde_json::json!({"servers":1,"clients":count,"unique_volumes":count,"workers":4,"metadata_provider":metadata,"tidb_pool_limit":16,"block_provider":provider,"block_cache_hits":cache_hits,"block_cache_misses":cache_misses,"disk_hits":disk_hits,"disk_misses":disk_misses,"disk_errors":disk_errors,"transport":"loopback HTTP session RPC","cache":"warm","errors":0,"phases":phases})
    );
}
