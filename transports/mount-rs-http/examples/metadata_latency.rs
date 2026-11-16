//! Metadata latency load client for `scripts/metadata-latency-bench.py`.
//!
//! One task and one keep-alive HTTP/1.1 connection per client, each against
//! its own drive. Each client prepares a file, a directory of 20 entries and
//! a work directory, then repeats the chosen operations at a fixed rate,
//! staggered across clients so they do not arrive together, or back to back
//! when the rate is 0. Prints per-operation latency percentiles as JSON,
//! grouped by whether the client's node owns its drive.
//!
//! usage: metadata_latency <plan.json>
//!
//! ```json
//! {"seconds": 20, "warmup": 3, "rate": 10, "threads": 4,
//!  "ops": ["stat", "head", "list", "mkdir", "rename", "delete"],
//!  "clients": [{"url": "http://127.0.0.1:8080", "drive": "d000",
//!               "token": "...", "forwarded": false}]}
//! ```
//!
//! A response carrying `x-mount-audit-*-ns` headers, as a build with the
//! `audit-forward-timing` feature sends for forwarded requests, also has
//! those stages summarised under `timing/<op>:<stage>`.
use reqwest::Method;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const OPS: [&str; 7] = ["ping", "stat", "head", "list", "mkdir", "rename", "delete"];

struct Client {
    http: reqwest::Client,
    root: String,
    base: String,
    auth: String,
}

impl Client {
    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<(u16, Vec<(String, u64)>), String> {
        let url = if path == "/readyz" {
            format!("{}/readyz", self.root)
        } else {
            format!("{}{}", self.base, path)
        };
        let mut request = self
            .http
            .request(method, url)
            .header("authorization", &self.auth);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.map_err(|e| e.to_string())?;
        let status = response.status().as_u16();
        let timings = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                let stage = name
                    .as_str()
                    .strip_prefix("x-mount-audit-")?
                    .strip_suffix("-ns")?;
                (!stage.contains("wall")).then_some(())?;
                Some((stage.to_owned(), value.to_str().ok()?.parse().ok()?))
            })
            .collect();
        response.bytes().await.map_err(|e| e.to_string())?;
        Ok((status, timings))
    }
}

type Samples = BTreeMap<String, Vec<u32>>;

struct Outcome {
    forwarded: bool,
    samples: Samples,
    timings: Samples,
    errors: u64,
}

fn micros(duration: Duration) -> u32 {
    duration.as_micros().min(u32::MAX as u128) as u32
}

async fn run(index: usize, clients: usize, spec: Value, plan: Value) -> Outcome {
    let http = reqwest::Client::builder()
        .http1_only()
        .tcp_nodelay(true)
        .pool_max_idle_per_host(1)
        .timeout(Duration::from_secs(30))
        .build()
        .expect("HTTP client");
    let url = spec["url"].as_str().expect("client url");
    let client = Client {
        http,
        root: url.to_owned(),
        base: format!("{url}/v1/drives/{}", spec["drive"].as_str().expect("drive")),
        auth: format!("Bearer {}", spec["token"].as_str().expect("token")),
    };
    for directory in ["/list", "/work"] {
        let body = json!({"path": directory, "recursive": true});
        let (status, _) = client
            .send(Method::POST, "/ops/mkdir", Some(body))
            .await
            .expect("setup mkdir");
        assert!(status < 300, "setup mkdir {directory}: {status}");
    }
    for n in 0..20 {
        let path = format!("/fs/list/f{n:02}");
        let (status, _) = client
            .send(Method::PUT, &path, Some(json!({ "n": n })))
            .await
            .expect("setup put");
        assert!(status < 300, "setup put {path}: {status}");
    }
    let (status, _) = client
        .send(
            Method::PUT,
            "/fs/file.txt",
            Some(json!({ "client": index })),
        )
        .await
        .expect("setup put");
    assert!(status < 300, "setup put /fs/file.txt: {status}");

    let chosen: Vec<&'static str> = match plan["ops"].as_array() {
        Some(list) => OPS
            .iter()
            .copied()
            .filter(|op| list.iter().any(|value| value == op))
            .collect(),
        None => OPS[1..].to_vec(),
    };
    let rate = plan["rate"].as_f64().unwrap_or(0.0);
    let gap = (rate > 0.0).then(|| Duration::from_secs_f64(1.0 / rate));
    let measure_from =
        Instant::now() + Duration::from_secs_f64(plan["warmup"].as_f64().unwrap_or(3.0));
    let end = measure_from + Duration::from_secs_f64(plan["seconds"].as_f64().unwrap_or(20.0));
    let mut next = Instant::now()
        + gap.map_or(Duration::ZERO, |gap| {
            gap.mul_f64(index as f64 / clients.max(1) as f64)
        });
    let mut samples: Samples = chosen
        .iter()
        .map(|op| (op.to_string(), Vec::new()))
        .collect();
    let mut timings = Samples::new();
    let mut errors = 0u64;
    // Modes run one after another on the same drives, and a run can stop
    // between a mkdir and its delete, so each run names its own entries.
    let run = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let mut n = 0u64;
    'outer: loop {
        for &op in &chosen {
            if let Some(gap) = gap {
                next += gap;
                tokio::time::sleep_until(next.into()).await;
            }
            if Instant::now() >= end {
                break 'outer;
            }
            let (method, path, body) = match op {
                "ping" => (Method::GET, "/readyz".to_owned(), None),
                "stat" => (Method::GET, "/stat/file.txt".to_owned(), None),
                "head" => (Method::HEAD, "/fs/file.txt".to_owned(), None),
                "list" => (Method::GET, "/entries/list".to_owned(), None),
                "mkdir" => (
                    Method::POST,
                    "/ops/mkdir".to_owned(),
                    Some(json!({ "path": format!("/work/{run}-a{n}") })),
                ),
                "rename" => (
                    Method::POST,
                    "/ops/rename".to_owned(),
                    Some(
                        json!({ "from": format!("/work/{run}-a{n}"), "to": format!("/work/{run}-b{n}") }),
                    ),
                ),
                _ => (Method::DELETE, format!("/fs/work/{run}-b{n}"), None),
            };
            let started = Instant::now();
            let result = client.send(method, &path, body).await;
            let elapsed = micros(started.elapsed());
            // `/readyz` answers requests that carry a token with 400; only
            // its latency matters.
            let good = match &result {
                Ok((status, _)) if op == "ping" => *status < 500,
                Ok((status, _)) if matches!(op, "stat" | "head" | "list") => *status == 200,
                Ok((status, _)) => *status < 300,
                Err(_) => false,
            };
            if started < measure_from {
                continue;
            }
            if !good {
                errors += 1;
                if errors <= 3 {
                    eprintln!("client {index} {op} {path}: {result:?}");
                }
                continue;
            }
            samples.get_mut(op).expect("chosen op").push(elapsed);
            if let Ok((_, stages)) = result {
                for (stage, nanos) in stages {
                    timings
                        .entry(format!("{op}:{stage}"))
                        .or_default()
                        .push((nanos / 1000).min(u32::MAX as u64) as u32);
                }
            }
        }
        n += 1;
    }
    Outcome {
        forwarded: spec["forwarded"].as_bool().unwrap_or(false),
        samples,
        timings,
        errors,
    }
}

fn summary(mut values: Vec<u32>) -> Value {
    if values.is_empty() {
        return json!({ "count": 0 });
    }
    values.sort_unstable();
    let len = values.len();
    let quantile = |q: f64| values[((len as f64 * q) as usize).min(len - 1)] as f64 / 1000.0;
    let under = values.iter().filter(|&&value| value < 1000).count() as f64 / len as f64;
    let mean = values.iter().map(|&value| value as f64).sum::<f64>() / len as f64 / 1000.0;
    json!({
        "count": len,
        "p50_ms": quantile(0.5),
        "p90_ms": quantile(0.9),
        "p99_ms": quantile(0.99),
        "p999_ms": quantile(0.999),
        "max_ms": values[len - 1] as f64 / 1000.0,
        "mean_ms": mean,
        "under_1ms": under,
    })
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: metadata_latency <plan.json>");
    let plan: Value = serde_json::from_str(&std::fs::read_to_string(path).expect("read plan"))
        .expect("plan JSON");
    let threads = plan["threads"].as_u64().unwrap_or(4) as usize;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(threads)
        .enable_all()
        .build()
        .expect("runtime");
    let specs = plan["clients"].as_array().expect("clients").clone();
    let outcomes = runtime.block_on(async {
        let count = specs.len();
        let tasks: Vec<_> = specs
            .into_iter()
            .enumerate()
            .map(|(index, spec)| tokio::spawn(run(index, count, spec, plan.clone())))
            .collect();
        let mut outcomes = Vec::new();
        for task in tasks {
            outcomes.push(task.await.expect("client task"));
        }
        outcomes
    });
    let mut groups = Samples::new();
    let mut timings = Samples::new();
    let mut errors = 0;
    for outcome in outcomes {
        errors += outcome.errors;
        let route = if outcome.forwarded {
            "forwarded"
        } else {
            "local"
        };
        for (op, values) in outcome.samples {
            groups
                .entry(format!("{route}/{op}"))
                .or_default()
                .extend(&values);
            groups
                .entry(format!("all/{op}"))
                .or_default()
                .extend(values);
        }
        for (stage, values) in outcome.timings {
            timings
                .entry(format!("timing/{stage}"))
                .or_default()
                .extend(values);
        }
    }
    let mut report = Map::new();
    for (key, values) in groups.into_iter().chain(timings) {
        report.insert(key, summary(values));
    }
    report.insert("errors".into(), json!(errors));
    println!(
        "{}",
        serde_json::to_string_pretty(&Value::Object(report)).expect("report")
    );
}
