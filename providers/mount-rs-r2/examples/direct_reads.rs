//! Isolated RustFS read diagnostic; no TiDB, mount cache, sessions or forwarding.
use mount_rs_r2::R2Config;
use object_store::{ObjectStore, path::Path};
use std::{
    env,
    sync::Arc,
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Barrier;

type Error = Box<dyn std::error::Error + Send + Sync>;
fn required(name: &str) -> Result<String, Error> {
    Ok(env::var(name)?)
}
fn p99(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    values[(values.len() * 99).div_ceil(100) - 1]
}
#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<(), Error> {
    if !std::path::Path::new(&required("RUSTFS_RUN_DIR")?).is_dir() {
        return Err("isolated RustFS fixture required".into());
    }
    let endpoint = required("R2_ENDPOINT")?;
    if !endpoint.starts_with("http://127.0.0.1:") {
        return Err("loopback RustFS endpoint required".into());
    }
    let shared = match env::var("AUDIT_SHARED_R2_CLIENT").as_deref() {
        Err(_) | Ok("0") => false,
        Ok("1") => true,
        _ => return Err("AUDIT_SHARED_R2_CLIENT must be 0 or 1".into()),
    };
    let prefix = format!(
        "{}/direct-{}-{}",
        required("RUSTFS_COMBO_PREFIX")?,
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    );
    let config = R2Config {
        endpoint,
        bucket: required("R2_BUCKET")?,
        access_key_id: required("R2_ACCESS_KEY_ID")?,
        secret_access_key: required("R2_SECRET_ACCESS_KEY")?,
        state_key: prefix.clone(),
    };
    let common = config.build_store()?;
    let payload = vec![0x5a; 4096];
    let mut clients: Vec<(Arc<dyn ObjectStore>, Path)> = Vec::new();
    for i in 0..100 {
        let store = if shared {
            common.clone()
        } else {
            config.build_store()?
        };
        let path = Path::from(format!("{prefix}/volume-{i}/block"));
        store.put(&path, payload.clone().into()).await?;
        assert_eq!(store.get(&path).await?.bytes().await?.as_ref(), payload);
        clients.push((store, path));
    }
    println!("clients,shared_client,samples,total_p99_ms,headers_p99_ms,body_p99_ms");
    for count in [1, 5, 10, 25, 50, 100] {
        let barrier = Arc::new(Barrier::new(count));
        let mut tasks = tokio::task::JoinSet::new();
        for (store, path) in clients.iter().take(count) {
            let (store, path, barrier) = (store.clone(), path.clone(), barrier.clone());
            tasks.spawn(async move {
                let mut samples = Vec::new();
                for _ in 0..50 {
                    barrier.wait().await;
                    let start = Instant::now();
                    let response = store.get(&path).await?;
                    let headers = start.elapsed().as_secs_f64() * 1000.0;
                    let body_start = Instant::now();
                    let bytes = response.bytes().await?;
                    let body = body_start.elapsed().as_secs_f64() * 1000.0;
                    let total = start.elapsed().as_secs_f64() * 1000.0;
                    if bytes.len() != 4096 || bytes.iter().any(|b| *b != 0x5a) {
                        return Err("payload mismatch".into());
                    }
                    samples.push((total, headers, body));
                }
                Ok::<_, Error>(samples)
            });
        }
        let (mut total, mut headers, mut body) = (Vec::new(), Vec::new(), Vec::new());
        while let Some(result) = tasks.join_next().await {
            for (t, h, b) in result?? {
                total.push(t);
                headers.push(h);
                body.push(b);
            }
        }
        println!(
            "{count},{shared},{},{:.6},{:.6},{:.6}",
            total.len(),
            p99(&mut total),
            p99(&mut headers),
            p99(&mut body)
        );
    }
    for (store, path) in clients {
        store.delete(&path).await?;
    }
    Ok(())
}
